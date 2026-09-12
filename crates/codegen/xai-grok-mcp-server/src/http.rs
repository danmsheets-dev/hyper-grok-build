//! Loopback HTTP transport.
//!
//! # Authentication
//!
//! `StreamableHttpService` authenticates nothing. Its only access control is
//! `allowed_hosts`, which defaults to loopback: a DNS-rebinding defence, not a
//! credential. The token travels in `Authorization: Bearer` (scheme matched
//! case-insensitively) and is compared in constant time; the secret path
//! segment is defence in depth.
//!
//! Browser origins need no separate check. A cross-origin request carrying an
//! `Authorization` header forces a CORS preflight, the preflight has no token,
//! and it is answered 401 with no `Access-Control-Allow-*` headers.
//!
//! The tunnel is invoked with `--http-host-header 127.0.0.1:<port>` so rmcp's
//! loopback `allowed_hosts` default holds end to end.
//!
//! # Connections
//!
//! The server accepts HTTP/1.1 only, through its own accept loop rather than
//! `axum::serve`, so it can bound what an unauthenticated peer costs: at most
//! [`MAX_CONNECTIONS`] connections, each allowed [`HEADER_READ_TIMEOUT`] to
//! finish its request headers, into a buffer of at most 64 KiB.
//!
//! # Order of work per request
//!
//! Authentication runs first and reads no body. An authenticated request then
//! takes one of [`MAX_CONCURRENT_REQUESTS`] slots, waiting if none is free, and
//! holds it until rmcp has finished with the request, including when the client
//! goes away mid-call. Only then is the body read, within [`BODY_READ_TIMEOUT`],
//! into a buffer capped at [`MAX_BODY_BYTES`], and refused if it holds more than
//! [`MAX_JSON_VALUES`] JSON values: parsing builds two trees at tens of bytes per
//! value, so bytes alone do not bound memory. Per slot that is at most about
//! three copies of the body plus a bounded parse.
//!
//! The body limit has to be enforced here: `axum::extract::DefaultBodyLimit`
//! only affects axum's own extractors, and rmcp reads the raw body with an
//! unbounded `collect()`.
//!
//! # Client disconnects
//!
//! In stateless mode rmcp serves each request on its own task and waits for a
//! response channel. If the HTTP request future is dropped mid-call, that
//! channel's receiver goes with it, the response send fails, and rmcp's
//! per-request loop never exits. Each request is therefore run on a spawned task
//! that owns the receiver until the handler finishes, so a disconnect cannot
//! strand it.
//!
//! # Shutdown
//!
//! [`shutdown_gracefully`] refuses new tool calls, stops accepting connections,
//! lets running calls finish within the toolset's drain window, and only then
//! ends requests still waiting, so a call that completes in the window returns
//! its result.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::StreamableHttpServerConfig;
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::handler::TurboMcpHandler;
use crate::oauth::{
    AuthorizationRequest, MAX_OAUTH_BODY_BYTES, OauthRefusal, OauthState, RegistrationRequest,
    TokenRequest,
};
use crate::toolset::{ServeEvent, ServedToolset};

/// Requests larger than this are refused.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// JSON values one request body may hold (counted as opening brackets and
/// separators outside strings).
pub const MAX_JSON_VALUES: usize = 100_000;

/// Authenticated requests handled at once.
pub const MAX_CONCURRENT_REQUESTS: usize = 16;

/// Open connections accepted at once.
pub const MAX_CONNECTIONS: usize = 64;

/// How long a connection has to finish sending request headers.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an authenticated request has to finish sending its body.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

const MAX_HEADER_BUFFER_BYTES: usize = 64 * 1024;

/// How long a closing connection may take to finish its current request. Longer
/// than the toolset's drain window, so a call that finishes during shutdown can
/// still deliver its response.
const CONNECTION_CLOSE_GRACE: Duration = Duration::from_secs(15);

type McpService = StreamableHttpService<TurboMcpHandler, LocalSessionManager>;

/// Timeouts the tests shorten.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ServeOptions {
    pub header_read_timeout: Duration,
    pub body_read_timeout: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            header_read_timeout: HEADER_READ_TIMEOUT,
            body_read_timeout: BODY_READ_TIMEOUT,
        }
    }
}

/// A running server. Dropping the handle stops it, so a startup abandoned
/// part-way leaves no listener behind.
pub struct ServeHandle {
    /// The full loopback URL, including the secret path segment.
    pub url: String,
    /// The bearer token a client must present.
    pub token: String,
    /// The OAuth state, so the operator can be shown the code that approves a
    /// connection and the tunnel can name the URL tokens are bound to.
    pub oauth: OauthState,
    /// The address the listener is bound to.
    pub addr: SocketAddr,
    pub port: u16,
    /// Stops the accept loop.
    accept_cancel: CancellationToken,
    /// Asks open connections to close once their current request is done.
    close_connections: CancellationToken,
    /// Ends rmcp requests still waiting for a response.
    rmcp_cancel: CancellationToken,
    slots: Arc<Semaphore>,
    connections: Arc<Semaphore>,
}

impl ServeHandle {
    /// Where a client with no token is pointed to find out how to get one.
    /// Derived from the resource as it stands, so it follows the tunnel instead
    /// of naming the loopback address the server first bound.
    pub fn metadata_url(&self) -> String {
        self.oauth.metadata_url()
    }

    /// Stop accepting connections. Open connections stay open.
    pub fn stop_accepting(&self) {
        self.accept_cancel.cancel();
    }

    /// Ask open connections to close once their current request is done.
    pub fn close_connections(&self) {
        self.close_connections.cancel();
    }

    /// Stop everything, including requests still waiting on a tool call.
    pub fn shutdown(&self) {
        self.accept_cancel.cancel();
        self.close_connections.cancel();
        self.rmcp_cancel.cancel();
    }

    /// Request slots currently free.
    pub fn free_request_slots(&self) -> usize {
        self.slots.available_permits()
    }

    /// Connections currently open.
    pub fn open_connections(&self) -> usize {
        MAX_CONNECTIONS - self.connections.available_permits()
    }
}

impl Drop for ServeHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Shut a server down without losing work: refuse new tool calls, stop
/// accepting connections, let running calls finish (up to the toolset's drain
/// window, then cancel them), end any request still waiting, and wait for the
/// accept loop.
pub async fn shutdown_gracefully(
    handle: &ServeHandle,
    toolset: &ServedToolset,
    join: tokio::task::JoinHandle<()>,
) {
    toolset.begin_shutdown();
    handle.stop_accepting();
    toolset.shutdown().await;
    // Only now are connections asked to close, so each has its full grace to
    // write a response produced at the end of the drain.
    handle.close_connections();
    // A call that has finished is not yet a delivered response. Let rmcp hand
    // every result back, which returns its request slot, and let the connections
    // write them and close, before ending whatever is still waiting.
    let deadline = tokio::time::Instant::now() + CONNECTION_CLOSE_GRACE;
    while (handle.free_request_slots() < MAX_CONCURRENT_REQUESTS || handle.open_connections() > 0)
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(10), join).await;
}

#[derive(Clone)]
struct AuthState {
    token: Arc<String>,
    toolset: Arc<ServedToolset>,
    /// Carries the metadata URL a 401 points at (RFC 9728). Read per request
    /// rather than snapshotted: the resource changes when the tunnel comes up,
    /// and a value frozen at bind time names this server's loopback address,
    /// which a remote client resolves to itself.
    oauth: OauthState,
}

/// Whether `body` holds more than `limit` JSON values, counted as opening
/// brackets plus separators outside strings: a cheap upper bound on the trees a
/// parser builds. Malformed JSON is left for the parser to refuse.
pub(crate) fn json_value_count_exceeds(body: &[u8], limit: usize) -> bool {
    let mut count = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in body {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' | b'{' | b',' | b':' => {
                count += 1;
                if count > limit {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Read the whole body within the timeout into a buffer of at most
/// [`MAX_BODY_BYTES`] and [`MAX_JSON_VALUES`], or refuse (413, 408, 400).
pub(crate) async fn limit_body(
    State(options): State<ServeOptions>,
    request: Request,
    next: Next,
) -> Response {
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > MAX_BODY_BYTES as u64) {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let (parts, body) = request.into_parts();
    let read = tokio::time::timeout(
        options.body_read_timeout,
        Limited::new(body, MAX_BODY_BYTES).collect(),
    )
    .await;
    let bytes = match read {
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
        Ok(Ok(collected)) => collected.to_bytes(),
        Ok(Err(e)) if e.downcast_ref::<LengthLimitError>().is_some() => {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "request body could not be read");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    if json_value_count_exceeds(&bytes, MAX_JSON_VALUES) {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

/// The request slot a request holds, carried in its extensions so it travels
/// with the request into rmcp's task and is released only when rmcp is done.
#[derive(Clone)]
struct RequestSlot(#[allow(dead_code)] Arc<OwnedSemaphorePermit>);

async fn limit_concurrency(
    State(slots): State<Arc<Semaphore>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Ok(slot) = slots.acquire_owned().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    request.extensions_mut().insert(RequestSlot(Arc::new(slot)));
    next.run(request).await
}

/// The credential of an `Authorization` header using the Bearer scheme. The
/// scheme name is case-insensitive (RFC 7235, section 2.1).
fn bearer_credential(value: &str) -> Option<&str> {
    let (scheme, credential) = value.trim().split_once(|c: char| c.is_ascii_whitespace())?;
    scheme
        .eq_ignore_ascii_case("Bearer")
        .then(|| credential.trim())
}

async fn require_bearer(State(state): State<AuthState>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_credential)
        .unwrap_or_default();

    // Either credential admits a request: the token the operator can paste, or
    // an access token this server issued for this resource. `accepts` is what
    // binds a token to its audience, so one minted for another URL is refused.
    let admitted = token_matches(presented.as_bytes(), state.token.as_bytes())
        || (!presented.is_empty() && state.oauth.accepts(presented));
    if !admitted {
        // The observer reports rejections to the operator, throttled.
        tracing::debug!("rejected request with missing or invalid bearer token");
        state.toolset.notify(ServeEvent::Unauthorized);
        // Points a client with no token at the metadata that says how to get
        // one (RFC 9728 section 5.1). Built without unwrapping on the URL: a
        // request path must not panic on a header value.
        let challenge = crate::oauth::challenge(&state.oauth.metadata_url());
        let value = axum::http::HeaderValue::from_str(&challenge)
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("Bearer"));
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::WWW_AUTHENTICATE, value)
            .body(Body::empty())
            .expect("static response builds");
    }
    next.run(request).await
}

fn token_matches(given: &[u8], expected: &[u8]) -> bool {
    // `subtle`, not `ring::constant_time`: ring deprecated its comparison with
    // an explicit "no promises regarding side channels" note. Token length is
    // fixed and not secret, so the length check leaks nothing.
    use subtle::ConstantTimeEq;
    given.len() == expected.len() && bool::from(given.ct_eq(expected))
}

/// Run the rmcp service on a task it owns, so a client disconnect cannot drop
/// rmcp's response receiver mid-call (see the module docs).
async fn mcp_entry(State(service): State<McpService>, mut request: Request) -> Response {
    // The request slot moves into the task, so a client that disconnects
    // mid-call does not free a slot while rmcp still holds its body.
    let slot = request.extensions_mut().remove::<RequestSlot>();
    let task = tokio::spawn(async move {
        use tower::ServiceExt;
        let _slot = slot;
        service.oneshot(request).await
    });
    match task.await {
        Ok(Ok(response)) => response.map(Body::new),
        Ok(Err(never)) => match never {},
        Err(e) => {
            tracing::error!(error = %e, "mcp request task failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The endpoints a client reaches before it has any credential: metadata,
/// registration, consent and token exchange. Every one is bounded — bodies are
/// capped far below the MCP limit, the stores behind them have caps of their
/// own, and each refusal is reported to the operator through the same throttled
/// line as a bad bearer token.
/// What the OAuth handlers share. The toolset is here only so a refusal can be
/// reported to the operator on the same throttled line as a bad bearer token:
/// these endpoints answer before any credential exists, so a scan of them is
/// otherwise silent.
#[derive(Clone)]
struct OauthRoutesState {
    oauth: OauthState,
    toolset: Arc<ServedToolset>,
}

// Lets the handlers that need nothing but the OAuth state keep asking for it.
impl axum::extract::FromRef<OauthRoutesState> for OauthState {
    fn from_ref(state: &OauthRoutesState) -> Self {
        state.oauth.clone()
    }
}

fn oauth_routes(state: OauthRoutesState, metadata_path: &str) -> axum::Router {
    use axum::routing::{get, post};

    axum::Router::new()
        .route(metadata_path, get(protected_resource_metadata))
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route("/oauth/register", post(register))
        .route(
            "/oauth/authorize",
            get(authorize_form).post(authorize_submit),
        )
        .route("/oauth/token", post(token))
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(MAX_OAUTH_BODY_BYTES))
}

async fn protected_resource_metadata(State(state): State<OauthState>) -> Response {
    axum::Json(state.protected_resource_metadata()).into_response()
}

async fn authorization_server_metadata(State(state): State<OauthState>) -> Response {
    axum::Json(state.authorization_server_metadata()).into_response()
}

/// An OAuth error response: the code the client acts on, and nothing else.
/// The operator is told separately — these endpoints answer before any
/// credential exists, so a scan of them would otherwise leave no trace.
fn oauth_error(toolset: &ServedToolset, refusal: OauthRefusal) -> Response {
    // Through the same throttled line as a bad bearer token, so a burst of
    // guesses against the console code cannot flood the terminal.
    toolset.notify(ServeEvent::Unauthorized);
    oauth_error_only(refusal)
}

/// The wire answer alone, for the paths that have already reported.
fn oauth_error_only(refusal: OauthRefusal) -> Response {
    let status = match refusal {
        OauthRefusal::InvalidClient
        | OauthRefusal::ConsentRefused
        | OauthRefusal::ConsentExpired => StatusCode::UNAUTHORIZED,
        OauthRefusal::TooMany => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_REQUEST,
    };
    tracing::debug!(reason = refusal.reason(), "refused an OAuth request");
    (
        status,
        axum::Json(serde_json::json!({ "error": refusal.code() })),
    )
        .into_response()
}

async fn register(
    State(state): State<OauthRoutesState>,
    axum::Json(request): axum::Json<RegistrationRequest>,
) -> Response {
    match state.oauth.register(&request) {
        Ok(registration) => (StatusCode::CREATED, axum::Json(registration)).into_response(),
        Err(refusal) => oauth_error(&state.toolset, refusal),
    }
}

/// The consent page. It shows what is being approved and asks for the code
/// Turbo printed on the operator's terminal; without that code nothing is
/// issued, so reaching this endpoint is not enough to obtain a token.
async fn authorize_form(
    State(state): State<OauthRoutesState>,
    axum::extract::Query(request): axum::extract::Query<AuthorizationRequest>,
) -> Response {
    let prompt = match state.oauth.check_authorization(&request) {
        Ok(prompt) => prompt,
        Err(refusal) => return oauth_error(&state.toolset, refusal),
    };
    // The window starts here, not at process start: it measures the operator's
    // trip from the terminal to this page. Serving the page again re-arms it,
    // so a setup that takes a few attempts is not locked out.
    state.oauth.begin_consent();
    let client = html_escape(&display_safe(&prompt.client_name, 64));
    let redirect = html_escape(&display_safe(&prompt.redirect_uri, 128));
    let page = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Approve this connection</title>\
         <body style=\"font-family:system-ui;max-width:34rem;margin:4rem auto\">\
         <h1>Approve this connection?</h1>\
         <p>A client asks to use Turbo's tools inside the roots you approved.</p>\
         <p><b>Client:</b> {client}<br><b>Sends you back to:</b> {redirect}</p>\
         <p>Turbo printed a code in the terminal it is running in. Enter it to approve.</p>\
         <form method=\"post\">\
         <input name=\"consent_code\" autocomplete=\"off\" autofocus \
         style=\"font-size:1.2rem;padding:.4rem\">\
         <button style=\"font-size:1.2rem;padding:.4rem 1rem\">Approve</button></form></body>"
    );
    // This page is the whole of the operator's decision, so it must not be
    // framed by another site, must not leak the query (which carries the client
    // id and redirect URI) through a referrer, and must not be cached.
    (
        [
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        axum::response::Html(page),
    )
        .into_response()
}

/// The approval itself. The form posts back to this same URL, so the query
/// carries the parameters the page displayed and the approval decides on
/// exactly those. On success the browser is sent back to the client with the
/// authorization code, which is useless without the PKCE verifier only the
/// client holds.
async fn authorize_submit(
    State(state): State<OauthRoutesState>,
    axum::extract::Query(request): axum::extract::Query<AuthorizationRequest>,
    axum::extract::Form(form): axum::extract::Form<ConsentForm>,
) -> Response {
    // A `state` longer than this is refused rather than truncated: the client
    // compares what comes back against what it sent, so a shortened one would
    // fail its own check and look like a client bug instead of our cap.
    if request
        .state
        .as_ref()
        .is_some_and(|s| s.len() > MAX_STATE_BYTES)
    {
        return oauth_error(&state.toolset, OauthRefusal::InvalidRequest);
    }
    match state.oauth.approve(&request, &form.consent_code) {
        Ok(code) => {
            let mut location = format!(
                "{}{}code={}",
                request.redirect_uri,
                if request.redirect_uri.contains('?') {
                    "&"
                } else {
                    "?"
                },
                code
            );
            if let Some(carried) = &request.state {
                // Escaped, not interpolated: an unescaped `state` lets a client
                // append parameters of its own to the callback (a second
                // `code=` among them), and silently mangles any state holding
                // `+`, `=`, `#` or a space, which is the shape base64 takes.
                location.push_str(&format!("&state={}", query_escape(carried)));
            }
            match axum::http::HeaderValue::from_str(&location) {
                Ok(value) => (StatusCode::FOUND, [(header::LOCATION, value)]).into_response(),
                Err(_) => oauth_error(&state.toolset, OauthRefusal::InvalidRequest),
            }
        }
        Err(refusal) => oauth_error(&state.toolset, refusal),
    }
}

#[derive(serde::Deserialize)]
struct ConsentForm {
    #[serde(default)]
    consent_code: String,
}

async fn token(
    State(state): State<OauthRoutesState>,
    axum::extract::Form(request): axum::extract::Form<TokenRequest>,
) -> Response {
    match state.oauth.token(&request) {
        Ok(issued) => (
            StatusCode::OK,
            [(
                header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            )],
            axum::Json(issued),
        )
            .into_response(),
        Err(refusal) => oauth_error(&state.toolset, refusal),
    }
}

/// The longest `state` this server will echo back into a redirect. It arrives
/// in the query string, so the body cap does not bound it, and it is copied
/// into a response header.
const MAX_STATE_BYTES: usize = 2048;

/// Escape a value for one query-string parameter, leaving only the characters
/// RFC 3986 calls unreserved. Everything else is percent-encoded, so the value
/// cannot end the parameter and start another.
fn query_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// What the consent page may show of a value the client chose. Control and
/// format characters go — a bidi override can make a redirect URI read as a
/// different host — and the result is bounded, because the operator reads these
/// two values to decide whether to approve and a wall of text is not a value.
fn display_safe(value: &str, budget: usize) -> String {
    let mut out = String::new();
    let mut shown = 0usize;
    for c in value.chars() {
        // Cc and Cf: controls, and the bidi and zero-width formatters.
        if c.is_control()
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
        {
            continue;
        }
        if shown >= budget {
            out.push('…');
            break;
        }
        out.push(c);
        shown += 1;
    }
    out
}

/// Escape text for the consent page. Every value shown there came from the
/// client, so none of it is trusted.
fn html_escape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#39;".to_string(),
            other => other.to_string(),
        })
        .collect()
}

fn random_hex(bytes: usize) -> String {
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut buf = vec![0u8; bytes];
    rng.fill(&mut buf).expect("system RNG");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Build the router and bind it to loopback. `port` of `None` picks an
/// ephemeral port.
pub async fn serve(
    toolset: Arc<ServedToolset>,
    port: Option<u16>,
) -> anyhow::Result<(ServeHandle, tokio::task::JoinHandle<()>)> {
    serve_with_options(toolset, port, ServeOptions::default()).await
}

pub(crate) async fn serve_with_options(
    toolset: Arc<ServedToolset>,
    port: Option<u16>,
    options: ServeOptions,
) -> anyhow::Result<(ServeHandle, tokio::task::JoinHandle<()>)> {
    let path_segment = random_hex(16);
    let token = random_hex(32);
    let accept_cancel = CancellationToken::new();
    let close_connections = CancellationToken::new();
    let rmcp_cancel = CancellationToken::new();

    let factory_toolset = toolset.clone();
    let service: McpService = StreamableHttpService::new(
        move || Ok(TurboMcpHandler::new(factory_toolset.clone())),
        Arc::new(LocalSessionManager::default()),
        {
            // #[non_exhaustive]: mutate a Default rather than struct-literal it.
            let mut cfg = StreamableHttpServerConfig::default();
            cfg.cancellation_token = rmcp_cancel.clone();
            cfg.json_response = true;
            cfg.stateful_mode = false;
            cfg
        },
    );

    let mcp_path = format!("/{path_segment}/mcp");
    let slots = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));

    // 127.0.0.1 only, never 0.0.0.0. Bound before the router is built: a token
    // is bound to the URL a client reaches this server at, port included, so
    // that URL has to exist first.
    let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0))).await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}{mcp_path}");
    let metadata_path = crate::oauth::protected_resource_path(&mcp_path);
    // No metadata URL is built here on purpose. The resource changes when a
    // tunnel reports its public URL, so anything derived from `addr` now would
    // name loopback for the rest of the run; it is derived per request instead.
    let oauth = OauthState::new(url.clone());

    let auth = AuthState {
        token: Arc::new(token.clone()),
        toolset: toolset.clone(),
        oauth: oauth.clone(),
    };

    // Layers wrap outward, so they run bottom to top: authenticate, take a
    // request slot, then read the body.
    let mcp = axum::Router::new()
        .route(&mcp_path, any(mcp_entry))
        .with_state(service)
        .layer(axum::middleware::from_fn_with_state(options, limit_body))
        .layer(axum::middleware::from_fn_with_state(
            slots.clone(),
            limit_concurrency,
        ))
        .layer(axum::middleware::from_fn_with_state(auth, require_bearer));

    // Merged *after* that layer, so they answer before any credential exists: a
    // client cannot be asked to authenticate at the endpoints that tell it how
    // to authenticate. Each is bounded on its own (see `oauth_routes`).
    // `merge` copies the other router's already-built routes, so layers applied
    // to `mcp` above do not reach them: the body timeout has to be layered on
    // the OAuth router itself. Without it a complete request head with a
    // Content-Length it never sends parks a connection for good, and 64 of
    // those take the whole server down — the bearer path with it, since they
    // share the connection budget.
    let app = mcp.merge(
        oauth_routes(
            OauthRoutesState {
                oauth: oauth.clone(),
                toolset,
            },
            &metadata_path,
        )
        .layer(axum::middleware::from_fn_with_state(options, limit_body)),
    );
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    let join = tokio::spawn(accept_loop(
        listener,
        app,
        connections.clone(),
        accept_cancel.clone(),
        close_connections.clone(),
        options,
    ));

    Ok((
        ServeHandle {
            url,
            token,
            oauth,
            addr,
            port: addr.port(),
            accept_cancel,
            close_connections,
            rmcp_cancel,
            slots,
            connections,
        },
        join,
    ))
}

async fn accept_loop(
    listener: TcpListener,
    app: axum::Router,
    connections: Arc<Semaphore>,
    accept_cancel: CancellationToken,
    close_connections: CancellationToken,
    options: ServeOptions,
) {
    loop {
        let stream = tokio::select! {
            _ = accept_cancel.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => stream,
                Err(e) => {
                    // Out of descriptors, for example: back off instead of spinning.
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            tracing::warn!("closing a connection: {MAX_CONNECTIONS} connections are already open");
            drop(stream);
            continue;
        };
        let service = TowerToHyperService::new(app.clone());
        let cancel = close_connections.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(options.header_read_timeout)
                .max_buf_size(MAX_HEADER_BUFFER_BYTES);
            let connection = builder.serve_connection(TokioIo::new(stream), service);
            tokio::pin!(connection);
            tokio::select! {
                result = connection.as_mut() => {
                    if let Err(e) = result {
                        tracing::debug!(error = %e, "connection ended with an error");
                    }
                }
                _ = cancel.cancelled() => {
                    connection.as_mut().graceful_shutdown();
                    if tokio::time::timeout(CONNECTION_CLOSE_GRACE, connection.as_mut())
                        .await
                        .is_err()
                    {
                        tracing::debug!("a connection did not close within the grace period");
                    }
                }
            }
        });
    }
}
