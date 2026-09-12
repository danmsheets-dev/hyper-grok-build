//! OAuth 2.1 for clients that cannot send a static bearer header.
//!
//! # Why this exists
//!
//! ChatGPT's connector discovers a server's OAuth metadata and registers
//! itself; it never offers to send an `Authorization: Bearer <token>` the
//! operator pasted. The MCP authorization specification makes that flow a
//! resource server's obligation: protected resource metadata (RFC 9728), a
//! `WWW-Authenticate` challenge pointing at it, and tokens validated as issued
//! **for this server** (RFC 8707). `rmcp` 2.1 ships OAuth for the client side
//! only, so everything here is this crate's to write and to audit.
//!
//! # What is trusted, and what is not
//!
//! Four of these endpoints answer before any credential exists, so each is
//! bounded on its own: small bodies, a cap on registered clients and pending
//! codes, single-use codes that expire in a minute, `S256` only, and exact
//! redirect matching. Rejections go through the same throttled operator line as
//! a bad bearer token, so a scan cannot flood the terminal.
//!
//! Consent has no user database to draw on. It binds to the console, which is
//! the trust boundary the bearer token already has: Turbo prints a code, and
//! the browser must return it. The window that code is good for opens when the
//! approval page is served — never at process start, which would make approval
//! impossible on a server that had been running longer — and loading the page
//! again reopens it. Inside that window the code may be used more than once, so
//! a client can be reconnected after a tunnel rebind clears its grant without
//! restarting the server and re-rolling every other credential. Reaching
//! `/oauth/authorize` is not enough to mint a token.
//!
//! Tokens are opaque random strings, never JWTs: there are no signing keys to
//! manage, and the process that issues a token is the only one that validates
//! it. They live in memory and die with the process, exactly as the bearer
//! token does.
//!
//! A token widens nothing. Every call still passes [`crate::guard::PathGuard`],
//! in the tier the operator started; a token is a way in, not a permission.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// How long an access token lasts. Short, because a leaked one cannot be
/// revoked from outside the process.
const ACCESS_TOKEN_TTL: Duration = Duration::from_secs(60 * 60);

/// How long a refresh token lasts without use.
const REFRESH_TOKEN_TTL: Duration = Duration::from_secs(60 * 60 * 24 * 14);

/// How long an authorization code is good for. The client redeems it
/// immediately; a minute is generous.
const CODE_TTL: Duration = Duration::from_secs(60);

/// How long the operator has to enter the console code.
const CONSENT_TTL: Duration = Duration::from_secs(5 * 60);

/// Clients registered at once. Registration is unauthenticated, so it cannot be
/// allowed to grow without bound.
const MAX_CLIENTS: usize = 32;

/// Authorization codes pending at once.
const MAX_PENDING_CODES: usize = 32;

/// Grants held at once. Issuing one takes the console code, but a holder of
/// that code could otherwise ask for tokens without limit, and this is the one
/// store nothing else bounds.
const MAX_GRANTS: usize = 64;

/// Bytes accepted on the unauthenticated endpoints, far below the MCP limit.
pub const MAX_OAUTH_BODY_BYTES: usize = 64 * 1024;

/// The scope this server issues. One tool surface, one scope.
const SCOPE: &str = "mcp";

/// A registered client. Public only: a client that cannot keep a secret is not
/// given one, which is what OAuth 2.1 requires of a native or browser client.
#[derive(Debug, Clone)]
struct Client {
    name: String,
    redirect_uris: Vec<String>,
    registered: Instant,
}

/// An authorization code, bound to everything it was issued against.
#[derive(Debug, Clone)]
struct PendingCode {
    client_id: String,
    redirect_uri: String,
    /// The PKCE challenge, which the verifier must reproduce.
    code_challenge: String,
    /// The resource the token will be valid for.
    resource: String,
    issued: Instant,
}

/// An issued access token and the refresh token that replaces it.
#[derive(Debug, Clone)]
struct Grant {
    client_id: String,
    /// The resource this token may be used at, checked on every MCP request.
    resource: String,
    issued: Instant,
    refresh: String,
    refresh_issued: Instant,
}

/// Why a request was refused. The wire answer is an OAuth error code; the
/// detail goes to the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OauthRefusal {
    /// The request itself is malformed: a missing or unusable parameter.
    InvalidRequest,
    /// The client id is not registered.
    InvalidClient,
    /// The code, verifier or refresh token did not check out.
    InvalidGrant,
    /// Registration or a code store is full.
    TooMany,
    /// The console code was wrong.
    ConsentRefused,
    /// The consent window closed, or never opened. Kept apart from a wrong
    /// code: the operator's remedy is to load the page again, not to retype.
    ConsentExpired,
}

impl OauthRefusal {
    /// The `error` member RFC 6749 section 5.2 defines for this refusal.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidClient => "invalid_client",
            Self::InvalidGrant => "invalid_grant",
            // No RFC 6749 code fits a server-side cap; `temporarily_unavailable`
            // is the honest one: the same request may work later.
            Self::TooMany => "temporarily_unavailable",
            // RFC 6749 defines nothing narrower for either, and inventing a
            // code would only confuse a conforming client. What separates them
            // reaches the operator through `reason`.
            Self::ConsentRefused | Self::ConsentExpired => "access_denied",
        }
    }

    /// What the operator is told, which is more than the client learns.
    pub fn reason(self) -> &'static str {
        match self {
            Self::InvalidRequest => "a malformed OAuth request",
            Self::InvalidClient => "an unregistered client id",
            Self::InvalidGrant => {
                "an authorization code, verifier or refresh token that did not check out"
            }
            Self::TooMany => "more registered clients or pending codes than this server keeps",
            Self::ConsentRefused => "a wrong console code",
            Self::ConsentExpired => {
                "an approval that came too late, or before the consent page was opened"
            }
        }
    }
}

/// Everything the OAuth endpoints share. Cheap to clone; the state is behind
/// one lock, because every operation here is short and rare.
#[derive(Clone)]
pub struct OauthState {
    inner: Arc<Mutex<Inner>>,
    /// The code the operator must enter to approve a connection, printed on
    /// their terminal when the server starts.
    consent_code: Arc<String>,
}

struct Inner {
    /// The URL a client actually reaches this server at: the loopback URL until
    /// a tunnel reports its public one. Tokens are bound to it.
    resource: String,
    clients: HashMap<String, Client>,
    codes: HashMap<String, PendingCode>,
    grants: HashMap<String, Grant>,
    /// Refresh tokens already used, with when they were spent so the record can
    /// be dropped once the chain it belonged to could no longer be live. A
    /// second use is theft, and revokes the chain it came from.
    spent_refresh: HashMap<String, (String, Instant)>,
    /// When the operator was last asked to approve a connection. `None` until
    /// the consent page is served: the window measures how long the operator
    /// has to carry the code from their terminal to the browser, so it cannot
    /// start at process start — a server up longer than the window would
    /// refuse every correct code it was ever given.
    consent_started: Option<Instant>,
}

impl OauthState {
    /// Build the state for a server whose loopback URL is `resource`.
    pub fn new(resource: String) -> Self {
        Self {
            consent_code: Arc::new(consent_code()),
            inner: Arc::new(Mutex::new(Inner {
                resource,
                clients: HashMap::new(),
                codes: HashMap::new(),
                grants: HashMap::new(),
                spent_refresh: HashMap::new(),
                consent_started: None,
            })),
        }
    }

    /// Start the consent window. Called when the consent page is served, so the
    /// five minutes measure the operator's trip from terminal to browser rather
    /// than the server's uptime. Serving the page again re-arms it: a first-run
    /// setup that takes a few tries is the normal case, not an attack.
    pub fn begin_consent(&self) {
        self.lock().consent_started = Some(Instant::now());
    }

    /// The code the operator must enter to approve a connection.
    pub fn consent_code(&self) -> &str {
        &self.consent_code
    }

    /// Point the metadata at the URL clients really use, once the tunnel has
    /// one. Tokens issued before this are bound to the old resource and stop
    /// being accepted, which is correct: they were issued for another URL.
    pub fn set_resource(&self, resource: String) {
        let mut inner = self.lock();
        inner.resource = resource;
        inner.grants.clear();
        inner.codes.clear();
        // Spent records belong to grants that no longer exist. Keeping them
        // would report a later refresh as theft when it is only a rebind.
        inner.spent_refresh.clear();
    }

    /// The resource identifier clients name in `resource` parameters.
    pub fn resource(&self) -> String {
        self.lock().resource.clone()
    }

    /// Where a client with no credential is told to look, derived from the
    /// resource as it stands now. Computed per call rather than stored: the
    /// resource changes when the tunnel comes up, and a snapshot taken at bind
    /// time would point a remote client at this server's loopback address.
    pub fn metadata_url(&self) -> String {
        let resource = self.resource();
        format!(
            "{}{}",
            issuer_of(&resource),
            protected_resource_path(&path_of(&resource))
        )
    }

    /// Insert a grant bound to `resource` and return its access token, so a
    /// test can reach the audience comparison in [`Self::accepts`]. There is no
    /// way to do this through the public API: `set_resource` is the only thing
    /// that changes the resource, and it clears every grant, so a token with a
    /// stale audience never survives to be judged. Without this seam, deleting
    /// the comparison would leave the suite green.
    #[cfg(test)]
    pub(crate) fn testing_issue_grant(&self, resource: &str) -> String {
        let mut inner = self.lock();
        inner
            .issue("test-client".to_string(), resource.to_string())
            .access_token
    }

    /// Push the consent window back by `by`, so a test can reach an expiry that
    /// is otherwise five minutes of wall clock away. Moving the stored instant
    /// rather than pausing the runtime: these tests drive a real listener and a
    /// real HTTP client, and a paused clock distorts both.
    #[cfg(test)]
    pub(crate) fn testing_age_consent(&self, by: Duration) {
        let mut inner = self.lock();
        // A window pushed further back than the process has existed is simply
        // one that has closed, which is what `None` already means here.
        inner.consent_started = inner.consent_started.and_then(|at| at.checked_sub(by));
    }

    /// Age every grant's access token by `by`, leaving its refresh timestamp
    /// alone so the grant survives `expire` and the access check is what judges
    /// it — which is the distinction the TTL fix turns on.
    #[cfg(test)]
    pub(crate) fn testing_age_grants(&self, by: Duration) {
        let mut inner = self.lock();
        for grant in inner.grants.values_mut() {
            if let Some(earlier) = grant.issued.checked_sub(by) {
                grant.issued = earlier;
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock means a panic while holding it; the state is still
        // structurally sound, and refusing every request afterwards would be
        // worse than continuing.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Protected resource metadata (RFC 9728). `authorization_servers` names
    /// this server, which hosts the authorization endpoints itself.
    pub fn protected_resource_metadata(&self) -> ProtectedResourceMetadata {
        let resource = self.resource();
        let issuer = issuer_of(&resource);
        ProtectedResourceMetadata {
            authorization_servers: vec![issuer],
            resource,
            scopes_supported: vec![SCOPE.to_string()],
            bearer_methods_supported: vec!["header".to_string()],
        }
    }

    /// Authorization server metadata (RFC 8414).
    pub fn authorization_server_metadata(&self) -> AuthorizationServerMetadata {
        let issuer = issuer_of(&self.resource());
        AuthorizationServerMetadata {
            authorization_endpoint: format!("{issuer}/oauth/authorize"),
            token_endpoint: format!("{issuer}/oauth/token"),
            registration_endpoint: format!("{issuer}/oauth/register"),
            issuer,
            response_types_supported: vec!["code".to_string()],
            grant_types_supported: vec![
                "authorization_code".to_string(),
                "refresh_token".to_string(),
            ],
            code_challenge_methods_supported: vec!["S256".to_string()],
            token_endpoint_auth_methods_supported: vec!["none".to_string()],
            scopes_supported: vec![SCOPE.to_string()],
        }
    }

    /// Register a client (RFC 7591). Public clients only: no secret is issued,
    /// so none can leak.
    pub fn register(&self, request: &RegistrationRequest) -> Result<Registration, OauthRefusal> {
        if request.redirect_uris.is_empty() {
            return Err(OauthRefusal::InvalidRequest);
        }
        for uri in &request.redirect_uris {
            if !redirect_uri_is_allowed(uri) {
                return Err(OauthRefusal::InvalidRequest);
            }
        }
        let mut inner = self.lock();
        inner.expire();
        // A full table means someone has been registering, not that this client
        // is unwelcome. Refusing here would let 32 unauthenticated requests lock
        // the operator's own connector out for the life of the process, so make
        // room instead — but never at the cost of a client that is working.
        while inner.clients.len() >= MAX_CLIENTS {
            let live: std::collections::HashSet<String> = inner
                .grants
                .values()
                .map(|grant| grant.client_id.clone())
                .collect();
            let Some(oldest) = inner
                .clients
                .iter()
                .filter(|(id, _)| !live.contains(*id))
                .min_by_key(|(_, client)| client.registered)
                .map(|(id, _)| id.clone())
            else {
                // Every registration has a live grant: the table is genuinely
                // full of clients in use, and evicting one would break it.
                return Err(OauthRefusal::TooMany);
            };
            inner.clients.remove(&oldest);
        }
        let client_id = random_id(16);
        inner.clients.insert(
            client_id.clone(),
            Client {
                name: request.client_name.clone().unwrap_or_default(),
                redirect_uris: request.redirect_uris.clone(),
                registered: Instant::now(),
            },
        );
        Ok(Registration {
            client_id,
            client_name: request.client_name.clone(),
            redirect_uris: request.redirect_uris.clone(),
            token_endpoint_auth_method: "none".to_string(),
            grant_types: vec![
                "authorization_code".to_string(),
                "refresh_token".to_string(),
            ],
            response_types: vec!["code".to_string()],
        })
    }

    /// What an authorization request asks for, once it is known to be usable.
    /// The code is issued only after the operator approves.
    pub fn check_authorization(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<AuthorizationPrompt, OauthRefusal> {
        if request.response_type != "code" || request.code_challenge_method != "S256" {
            return Err(OauthRefusal::InvalidRequest);
        }
        if request.code_challenge.len() < 43 || request.code_challenge.len() > 128 {
            return Err(OauthRefusal::InvalidRequest);
        }
        let inner = self.lock();
        let Some(client) = inner.clients.get(&request.client_id) else {
            return Err(OauthRefusal::InvalidClient);
        };
        // Exact match, as OAuth 2.1 requires: a prefix rule is how open
        // redirects happen.
        if !client
            .redirect_uris
            .iter()
            .any(|uri| uri == &request.redirect_uri)
        {
            return Err(OauthRefusal::InvalidRequest);
        }
        // The token must be bound to the resource the client will use it at.
        // An absent `resource` is this server; a different one is refused
        // rather than silently narrowed.
        if let Some(asked) = &request.resource
            && !same_resource(asked, &inner.resource)
        {
            return Err(OauthRefusal::InvalidRequest);
        }
        Ok(AuthorizationPrompt {
            client_name: client.name.clone(),
            redirect_uri: request.redirect_uri.clone(),
            state: request.state.clone(),
        })
    }

    /// Approve an authorization request with the console code, and issue the
    /// authorization code the client redeems.
    pub fn approve(
        &self,
        request: &AuthorizationRequest,
        presented: &str,
    ) -> Result<String, OauthRefusal> {
        self.check_authorization(request)?;
        let mut inner = self.lock();
        // Expiry is reported apart from a wrong code: they are different
        // problems for the operator, and telling them apart is the difference
        // between "type it again" and "load the page again".
        match inner.consent_started {
            None => return Err(OauthRefusal::ConsentExpired),
            Some(started) if started.elapsed() > CONSENT_TTL => {
                return Err(OauthRefusal::ConsentExpired);
            }
            Some(_) => {}
        }
        if !constant_time_eq(presented.trim().as_bytes(), self.consent_code.as_bytes()) {
            return Err(OauthRefusal::ConsentRefused);
        }
        inner.expire();
        if inner.codes.len() >= MAX_PENDING_CODES {
            return Err(OauthRefusal::TooMany);
        }
        let code = random_id(32);
        let resource = inner.resource.clone();
        inner.codes.insert(
            code.clone(),
            PendingCode {
                client_id: request.client_id.clone(),
                redirect_uri: request.redirect_uri.clone(),
                code_challenge: request.code_challenge.clone(),
                resource,
                issued: Instant::now(),
            },
        );
        Ok(code)
    }

    /// Exchange an authorization code for tokens, or refresh them.
    pub fn token(&self, request: &TokenRequest) -> Result<TokenResponse, OauthRefusal> {
        let mut inner = self.lock();
        inner.expire();
        match request.grant_type.as_str() {
            "authorization_code" => {
                let (Some(code), Some(verifier)) = (&request.code, &request.code_verifier) else {
                    return Err(OauthRefusal::InvalidRequest);
                };
                // Single use: taken out of the store whether or not it checks
                // out, so a guess cannot be retried against the same code.
                let Some(pending) = inner.codes.remove(code) else {
                    return Err(OauthRefusal::InvalidGrant);
                };
                if pending.client_id != request.client_id
                    || request
                        .redirect_uri
                        .as_ref()
                        .is_none_or(|uri| uri != &pending.redirect_uri)
                {
                    return Err(OauthRefusal::InvalidGrant);
                }
                if !verifier_matches(verifier, &pending.code_challenge) {
                    return Err(OauthRefusal::InvalidGrant);
                }
                Ok(inner.issue(pending.client_id, pending.resource))
            }
            "refresh_token" => {
                let Some(presented) = &request.refresh_token else {
                    return Err(OauthRefusal::InvalidRequest);
                };
                // A refresh token used twice is theft. Kept, not removed: a
                // third presentation must be reported too, and removing the
                // record would erase the evidence of the second.
                if let Some((successor, _)) = inner.spent_refresh.get(presented).cloned() {
                    // The legitimate rotation may already have replaced the
                    // successor, so say what was actually revoked rather than
                    // claiming a revocation that did not happen.
                    let revoked = inner.grants.remove(&successor).is_some();
                    // `error`, not `warn`: `turbo mcp serve` installs an
                    // "error" filter by default, and this is the only signal
                    // the operator gets that a token was stolen.
                    tracing::error!(
                        revoked,
                        "an OAuth refresh token was presented twice, which means it was copied"
                    );
                    return Err(OauthRefusal::InvalidGrant);
                }
                let Some((access, grant)) = inner
                    .grants
                    .iter()
                    .find(|(_, grant)| grant.refresh == *presented)
                    .map(|(access, grant)| (access.clone(), grant.clone()))
                else {
                    return Err(OauthRefusal::InvalidGrant);
                };
                if grant.client_id != request.client_id {
                    return Err(OauthRefusal::InvalidGrant);
                }
                inner.grants.remove(&access);
                let issued = inner.issue(grant.client_id, grant.resource);
                inner.spent_refresh.insert(
                    presented.clone(),
                    (issued.access_token.clone(), Instant::now()),
                );
                Ok(issued)
            }
            _ => Err(OauthRefusal::InvalidRequest),
        }
    }

    /// Whether `token` is one this server issued, for the resource it is being
    /// presented at. Audience binding: a token minted for another URL, or by
    /// another server, is not accepted (RFC 8707, and the MCP specification's
    /// token-passthrough rule).
    pub fn accepts(&self, token: &str) -> bool {
        let mut inner = self.lock();
        inner.expire();
        let resource = inner.resource.clone();
        inner.grants.get(token).is_some_and(|grant| {
            // The access token's own lifetime is enforced here rather than in
            // `expire`, because the grant has to outlive it: the refresh token
            // it carries stays usable for far longer. `expires_in` told the
            // client an hour, so an hour is what this honours.
            grant.issued.elapsed() <= ACCESS_TOKEN_TTL && same_resource(&grant.resource, &resource)
        })
    }
}

impl Inner {
    /// Drop what has expired. Called before every decision, so nothing here
    /// grows on a schedule of its own.
    fn expire(&mut self) {
        self.codes
            .retain(|_, code| code.issued.elapsed() <= CODE_TTL);
        self.grants.retain(|_, grant| {
            grant.issued.elapsed() <= ACCESS_TOKEN_TTL
                || grant.refresh_issued.elapsed() <= REFRESH_TOKEN_TTL
        });
        self.clients
            .retain(|_, client| client.registered.elapsed() <= REFRESH_TOKEN_TTL);
        // Once the chain a spent token belonged to can no longer be live,
        // remembering it buys nothing and the map would grow for the life of
        // the process.
        self.spent_refresh
            .retain(|_, (_, spent)| spent.elapsed() <= REFRESH_TOKEN_TTL);
    }

    /// Drop the oldest grant while the store is over its cap. A console-code
    /// holder can ask for tokens repeatedly, so this store needs a bound like
    /// the others; the oldest is the one whose access token is nearest expiry.
    fn cap_grants(&mut self) {
        while self.grants.len() >= MAX_GRANTS {
            let Some(oldest) = self
                .grants
                .iter()
                .min_by_key(|(_, grant)| grant.issued)
                .map(|(access, _)| access.clone())
            else {
                break;
            };
            self.grants.remove(&oldest);
        }
    }

    fn issue(&mut self, client_id: String, resource: String) -> TokenResponse {
        self.cap_grants();
        let access = random_id(32);
        let refresh = random_id(32);
        let now = Instant::now();
        self.grants.insert(
            access.clone(),
            Grant {
                client_id,
                resource,
                issued: now,
                refresh: refresh.clone(),
                refresh_issued: now,
            },
        );
        TokenResponse {
            access_token: access,
            token_type: "Bearer".to_string(),
            expires_in: ACCESS_TOKEN_TTL.as_secs(),
            refresh_token: refresh,
            scope: SCOPE.to_string(),
        }
    }
}

/// Protected resource metadata, RFC 9728 section 2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtectedResourceMetadata {
    pub resource: String,
    pub authorization_servers: Vec<String>,
    pub scopes_supported: Vec<String>,
    pub bearer_methods_supported: Vec<String>,
}

/// Authorization server metadata, RFC 8414 section 2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: String,
    pub response_types_supported: Vec<String>,
    pub grant_types_supported: Vec<String>,
    pub code_challenge_methods_supported: Vec<String>,
    pub token_endpoint_auth_methods_supported: Vec<String>,
    pub scopes_supported: Vec<String>,
}

/// A dynamic client registration request, RFC 7591 section 2. Only the members
/// this server acts on are read; the rest are ignored, as the RFC allows.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RegistrationRequest {
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub client_name: Option<String>,
}

/// What registration answers with.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Registration {
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    pub redirect_uris: Vec<String>,
    pub token_endpoint_auth_method: String,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
}

/// The query an authorization request carries.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthorizationRequest {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
}

/// What the operator is shown before approving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationPrompt {
    pub client_name: String,
    pub redirect_uri: String,
    pub state: Option<String>,
}

/// A token request, RFC 6749 section 4.1.3 and 6.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub client_id: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub code_verifier: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
}

/// What the token endpoint answers with.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub refresh_token: String,
    pub scope: String,
}

/// The issuer for a resource URL: its origin, since the authorization endpoints
/// are served beside the MCP route rather than under its secret path.
fn issuer_of(resource: &str) -> String {
    let Some(scheme_end) = resource.find("://") else {
        return resource.to_string();
    };
    let after = &resource[scheme_end + 3..];
    let host_end = after.find('/').unwrap_or(after.len());
    resource[..scheme_end + 3 + host_end].to_string()
}

/// The path a client fetches protected resource metadata from. RFC 9728 puts
/// the resource's own path *after* the well-known segment.
/// The path part of a resource identifier, which RFC 9728 inserts into the
/// well-known URL. An identifier with no path contributes none.
fn path_of(resource: &str) -> String {
    let after_scheme = match resource.find("://") {
        Some(end) => &resource[end + 3..],
        None => resource,
    };
    match after_scheme.find('/') {
        Some(slash) => after_scheme[slash..].to_string(),
        None => String::new(),
    }
}

pub fn protected_resource_path(resource_path: &str) -> String {
    let trimmed = resource_path.trim_end_matches('/');
    format!("/.well-known/oauth-protected-resource{trimmed}")
}

/// The `WWW-Authenticate` value a 401 carries, which is how a client finds the
/// metadata (RFC 9728 section 5.1).
pub fn challenge(metadata_url: &str) -> String {
    format!("Bearer resource_metadata=\"{metadata_url}\"")
}

/// Whether two resource identifiers name the same server. Compared without a
/// trailing slash, and with scheme and host case-insensitive, as RFC 8707 and
/// the MCP specification ask.
fn same_resource(a: &str, b: &str) -> bool {
    normalize_resource(a) == normalize_resource(b)
}

fn normalize_resource(resource: &str) -> String {
    let trimmed = resource.trim_end_matches('/');
    match trimmed.find("://") {
        // Only the scheme and host fold case; a path can be case-sensitive.
        Some(end) => {
            let after = &trimmed[end + 3..];
            let host_end = after.find('/').unwrap_or(after.len());
            format!(
                "{}{}",
                trimmed[..end + 3 + host_end].to_lowercase(),
                &after[host_end..]
            )
        }
        None => trimmed.to_lowercase(),
    }
}

/// Whether a redirect URI is one OAuth 2.1 allows: HTTPS, or a loopback address
/// for a native client.
fn redirect_uri_is_allowed(uri: &str) -> bool {
    if uri.contains('#') {
        return false;
    }
    if let Some(rest) = uri.strip_prefix("https://") {
        return !rest.is_empty();
    }
    if let Some(rest) = uri.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or_default();
        // `http://127.0.0.1:80@evil.example/cb` is `evil.example` to every real
        // URL parser, while scanning for the first `:` or `/` reads it as
        // loopback. OAuth 2.1 forbids userinfo in a redirect URI, so refuse it
        // rather than parse around it.
        if authority.contains('@') {
            return false;
        }
        // A bracketed IPv6 literal carries colons of its own, so the port is
        // whatever follows the closing bracket. Splitting on every colon left
        // the bracketed arms below unreachable.
        let host = match authority.find(']') {
            Some(end) => &authority[..=end],
            None => authority.split(':').next().unwrap_or_default(),
        };
        return matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1");
    }
    // A private-use scheme (`com.example.app:/callback`) is how a native client
    // comes back, and OAuth 2.1 allows it.
    uri.find(':').is_some_and(|colon| {
        let scheme = &uri[..colon];
        scheme.contains('.') && !scheme.contains('/')
    })
}

/// Whether a PKCE verifier reproduces its challenge (S256 only).
fn verifier_matches(verifier: &str, challenge: &str) -> bool {
    if verifier.len() < 43 || verifier.len() > 128 {
        return false;
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref());
    constant_time_eq(computed.as_bytes(), challenge.as_bytes())
}

fn constant_time_eq(given: &[u8], expected: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    given.len() == expected.len() && bool::from(given.ct_eq(expected))
}

/// A random identifier, hex so it survives every transport unescaped.
fn random_id(bytes: usize) -> String {
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut buf = vec![0u8; bytes];
    rng.fill(&mut buf).expect("system RNG");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// The code the operator types to approve a connection: eight characters from
/// an alphabet without look-alikes, so reading it off a terminal is reliable.
fn consent_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut buf = [0u8; 8];
    rng.fill(&mut buf).expect("system RNG");
    buf.iter()
        .map(|byte| ALPHABET[usize::from(*byte) % ALPHABET.len()] as char)
        .collect()
}
