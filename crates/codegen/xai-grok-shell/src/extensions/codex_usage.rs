//! Read-only OpenAI Codex subscription usage extension.

use std::future::Future;
use std::time::Duration;

use agent_client_protocol as acp;
use futures_util::StreamExt as _;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::auth::GrokAuth;

const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodexUsageRequest {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum CodexUsageResponse {
    Fresh {
        #[serde(rename = "accountKey")]
        account_key: String,
        #[serde(rename = "authGeneration")]
        auth_generation: String,
        #[serde(rename = "planType", skip_serializing_if = "Option::is_none")]
        plan_type: Option<String>,
        #[serde(rename = "fetchedAt")]
        fetched_at: String,
        buckets: Vec<CodexUsageBucket>,
        #[serde(skip_serializing_if = "Option::is_none")]
        credits: Option<CodexCredits>,
    },
    NoLimits {
        #[serde(rename = "accountKey")]
        account_key: String,
        #[serde(rename = "authGeneration")]
        auth_generation: String,
        #[serde(rename = "planType", skip_serializing_if = "Option::is_none")]
        plan_type: Option<String>,
        #[serde(rename = "fetchedAt")]
        fetched_at: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        credits: Option<CodexCredits>,
    },
    AuthUnavailable,
    ForbiddenUnsupported {
        #[serde(rename = "accountKey")]
        account_key: String,
        #[serde(rename = "authGeneration")]
        auth_generation: String,
    },
    Transient {
        #[serde(rename = "accountKey", skip_serializing_if = "Option::is_none")]
        account_key: Option<String>,
        #[serde(rename = "authGeneration", skip_serializing_if = "Option::is_none")]
        auth_generation: Option<String>,
        error: CodexUsageError,
        #[serde(rename = "staleEligible")]
        stale_eligible: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CodexUsageError {
    Timeout,
    Network,
    RateLimited,
    Server,
    InvalidResponse,
    ResponseTooLarge,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsageBucket {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_reached: Option<bool>,
    pub windows: Vec<CodexUsageWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsageWindow {
    pub kind: CodexUsageWindowKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_window_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_after_seconds: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CodexUsageWindowKind {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCredits {
    #[serde(alias = "has_credits", skip_serializing_if = "Option::is_none")]
    pub has_credits: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unlimited: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance: Option<String>,
}

#[derive(Deserialize)]
struct UsagePayload {
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit: Option<RateLimitDetails>,
    #[serde(default)]
    additional_rate_limits: Option<Vec<AdditionalRateLimit>>,
    #[serde(default)]
    credits: Option<CodexCredits>,
}

#[derive(Deserialize)]
struct AdditionalRateLimit {
    metered_feature: String,
    limit_name: String,
    #[serde(default)]
    rate_limit: Option<RateLimitDetails>,
}

#[derive(Deserialize)]
struct RateLimitDetails {
    #[serde(default)]
    allowed: Option<bool>,
    #[serde(default)]
    limit_reached: Option<bool>,
    #[serde(default)]
    primary_window: Option<RateLimitWindow>,
    #[serde(default)]
    secondary_window: Option<RateLimitWindow>,
}

#[derive(Deserialize)]
struct RateLimitWindow {
    #[serde(default)]
    used_percent: Option<f64>,
    #[serde(default)]
    limit_window_seconds: Option<i64>,
    #[serde(default)]
    reset_after_seconds: Option<i64>,
    #[serde(default)]
    reset_at: Option<i64>,
}

struct AuthIdentity {
    account_id: String,
    account_key: String,
    auth_generation: String,
}

impl AuthIdentity {
    fn from_auth(auth: &GrokAuth, epoch: Option<&str>) -> Option<Self> {
        let account_id = auth.account_id.as_deref()?.trim();
        if account_id.is_empty() || auth.key.trim().is_empty() {
            return None;
        }
        let account_key = opaque_id(b"codex-account-v1\0", &[account_id.as_bytes()]);
        let legacy_created;
        let lifecycle = if let Some(epoch) = epoch.map(str::trim).filter(|epoch| !epoch.is_empty())
        {
            epoch
        } else {
            legacy_created = auth
                .create_time
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
            &legacy_created
        };
        let auth_generation = opaque_id(
            b"codex-auth-generation-v1\0",
            &[account_id.as_bytes(), lifecycle.as_bytes()],
        );
        Some(Self {
            account_id: account_id.to_owned(),
            account_key,
            auth_generation,
        })
    }
}

fn opaque_id(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    for part in parts {
        hasher.update(part);
        hasher.update(&[0]);
    }
    format!("v1:{}", hasher.finalize().to_hex())
}

type PairedAuth = (GrokAuth, Option<String>);

fn current_paired_auth() -> Option<PairedAuth> {
    let path = crate::auth::auth_json_path();
    let home = path.parent().unwrap_or(&path);
    crate::auth::read_openai_codex_auth_with_epoch(home)
}

fn pair_resolved_auth(auth: GrokAuth) -> Option<PairedAuth> {
    let (current, epoch) = current_paired_auth()?;
    if current.key != auth.key
        || current.account_id.as_deref().map(str::trim) != auth.account_id.as_deref().map(str::trim)
        || current.refresh_token.as_deref().map(str::trim)
            != auth.refresh_token.as_deref().map(str::trim)
    {
        return None;
    }
    Some((auth, epoch))
}

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(_agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let _: CodexUsageRequest = parse_params(args)?;
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => return to_raw_response(&transient(None, CodexUsageError::Network)),
    };
    let initial = crate::auth::openai_codex::ensure_openai_codex_auth()
        .await
        .and_then(pair_resolved_auth);
    let response = fetch_with_refresh(
        &client,
        CODEX_USAGE_URL,
        REQUEST_TIMEOUT,
        initial,
        || async {
            crate::auth::openai_codex::force_refresh_openai_codex_auth()
                .await
                .and_then(pair_resolved_auth)
        },
    )
    .await;
    to_raw_response(&invalidate_completed_result_if_auth_changed(
        response,
        current_paired_auth(),
    ))
}

fn invalidate_completed_result_if_auth_changed(
    response: CodexUsageResponse,
    current: Option<PairedAuth>,
) -> CodexUsageResponse {
    let expected = match &response {
        CodexUsageResponse::Fresh {
            account_key,
            auth_generation,
            ..
        }
        | CodexUsageResponse::NoLimits {
            account_key,
            auth_generation,
            ..
        }
        | CodexUsageResponse::ForbiddenUnsupported {
            account_key,
            auth_generation,
        } => Some((account_key.as_str(), auth_generation.as_str())),
        CodexUsageResponse::AuthUnavailable | CodexUsageResponse::Transient { .. } => None,
    };
    let Some((account_key, auth_generation)) = expected else {
        return response;
    };
    let current = current
        .as_ref()
        .and_then(|(auth, epoch)| AuthIdentity::from_auth(auth, epoch.as_deref()))
        .map(|identity| (identity.account_key, identity.auth_generation));
    if current
        .as_ref()
        .is_some_and(|(current_account, current_generation)| {
            current_account == account_key && current_generation == auth_generation
        })
    {
        response
    } else {
        CodexUsageResponse::AuthUnavailable
    }
}

async fn fetch_with_refresh<F, Fut>(
    client: &reqwest::Client,
    url: &str,
    request_timeout: Duration,
    initial: Option<PairedAuth>,
    mut force_refresh: F,
) -> CodexUsageResponse
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<PairedAuth>>,
{
    let Some((initial, initial_epoch)) = initial else {
        return CodexUsageResponse::AuthUnavailable;
    };
    let Some(identity) = AuthIdentity::from_auth(&initial, initial_epoch.as_deref()) else {
        return CodexUsageResponse::AuthUnavailable;
    };
    match fetch_once(client, url, request_timeout, &initial, &identity).await {
        FetchOutcome::Response(response) => response,
        FetchOutcome::Unauthorized => {
            let Some((refreshed, refreshed_epoch)) = force_refresh().await else {
                return CodexUsageResponse::AuthUnavailable;
            };
            let Some(refreshed_identity) =
                AuthIdentity::from_auth(&refreshed, refreshed_epoch.as_deref())
            else {
                return CodexUsageResponse::AuthUnavailable;
            };
            match fetch_once(
                client,
                url,
                request_timeout,
                &refreshed,
                &refreshed_identity,
            )
            .await
            {
                FetchOutcome::Response(response) => response,
                FetchOutcome::Unauthorized => CodexUsageResponse::AuthUnavailable,
            }
        }
    }
}

enum FetchOutcome {
    Response(CodexUsageResponse),
    Unauthorized,
}

async fn fetch_once(
    client: &reqwest::Client,
    url: &str,
    request_timeout: Duration,
    auth: &GrokAuth,
    identity: &AuthIdentity,
) -> FetchOutcome {
    let account_header = match HeaderValue::from_str(&identity.account_id) {
        Ok(value) => value,
        Err(_) => return FetchOutcome::Response(CodexUsageResponse::AuthUnavailable),
    };
    let bearer = match HeaderValue::from_str(&format!("Bearer {}", auth.key)) {
        Ok(value) => value,
        Err(_) => return FetchOutcome::Response(CodexUsageResponse::AuthUnavailable),
    };
    let user_agent = format!("turbo/{}", xai_grok_version::VERSION);
    let result = client
        .get(url)
        .header(AUTHORIZATION, bearer)
        .header("ChatGPT-Account-Id", account_header)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, user_agent)
        .timeout(request_timeout)
        .send()
        .await;
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            let kind = if error.is_timeout() {
                CodexUsageError::Timeout
            } else {
                CodexUsageError::Network
            };
            return FetchOutcome::Response(transient(Some(identity), kind));
        }
    };
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return FetchOutcome::Unauthorized;
    }
    if status == reqwest::StatusCode::FORBIDDEN
        || status.is_redirection()
        || status.is_client_error()
    {
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return FetchOutcome::Response(transient(Some(identity), CodexUsageError::RateLimited));
        }
        return FetchOutcome::Response(CodexUsageResponse::ForbiddenUnsupported {
            account_key: identity.account_key.clone(),
            auth_generation: identity.auth_generation.clone(),
        });
    }
    if status.is_server_error() {
        return FetchOutcome::Response(transient(Some(identity), CodexUsageError::Server));
    }
    if !status.is_success() {
        return FetchOutcome::Response(transient(Some(identity), CodexUsageError::Network));
    }

    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return FetchOutcome::Response(transient(
            Some(identity),
            CodexUsageError::ResponseTooLarge,
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let kind = if error.is_timeout() {
                    CodexUsageError::Timeout
                } else {
                    CodexUsageError::Network
                };
                return FetchOutcome::Response(transient(Some(identity), kind));
            }
        };
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return FetchOutcome::Response(transient(
                Some(identity),
                CodexUsageError::ResponseTooLarge,
            ));
        }
        body.extend_from_slice(&chunk);
    }
    FetchOutcome::Response(parse_success(&body, identity))
}

fn parse_success(body: &[u8], identity: &AuthIdentity) -> CodexUsageResponse {
    if body.iter().all(u8::is_ascii_whitespace) {
        return transient(Some(identity), CodexUsageError::InvalidResponse);
    }
    let payload: UsagePayload = match serde_json::from_slice(body) {
        Ok(payload) => payload,
        Err(_) => return transient(Some(identity), CodexUsageError::InvalidResponse),
    };
    let mut buckets = Vec::new();
    if let Some(rate_limit) = payload.rate_limit {
        match map_bucket("codex".to_owned(), None, rate_limit) {
            Ok(bucket) => buckets.push(bucket),
            Err(error) => return transient(Some(identity), error),
        }
    }
    for additional in payload.additional_rate_limits.unwrap_or_default() {
        let details = additional.rate_limit.unwrap_or(RateLimitDetails {
            allowed: None,
            limit_reached: None,
            primary_window: None,
            secondary_window: None,
        });
        match map_bucket(
            additional.metered_feature,
            Some(additional.limit_name),
            details,
        ) {
            Ok(bucket) => buckets.push(bucket),
            Err(error) => return transient(Some(identity), error),
        }
    }
    let fetched_at = fetched_at();
    if buckets.is_empty() {
        no_limits(identity, payload.plan_type, payload.credits, fetched_at)
    } else {
        CodexUsageResponse::Fresh {
            account_key: identity.account_key.clone(),
            auth_generation: identity.auth_generation.clone(),
            plan_type: payload.plan_type,
            fetched_at,
            buckets,
            credits: payload.credits,
        }
    }
}

fn map_bucket(
    id: String,
    name: Option<String>,
    details: RateLimitDetails,
) -> Result<CodexUsageBucket, CodexUsageError> {
    if id.trim().is_empty() || name.as_deref().is_some_and(|value| value.trim().is_empty()) {
        return Err(CodexUsageError::InvalidResponse);
    }
    let mut windows = Vec::new();
    if let Some(window) = details.primary_window {
        windows.push(map_window(CodexUsageWindowKind::Primary, window)?);
    }
    if let Some(window) = details.secondary_window {
        windows.push(map_window(CodexUsageWindowKind::Secondary, window)?);
    }
    Ok(CodexUsageBucket {
        id,
        name,
        allowed: details.allowed,
        limit_reached: details.limit_reached,
        windows,
    })
}

fn map_window(
    kind: CodexUsageWindowKind,
    window: RateLimitWindow,
) -> Result<CodexUsageWindow, CodexUsageError> {
    if window
        .used_percent
        .is_some_and(|percent| !percent.is_finite() || !(0.0..=100.0).contains(&percent))
        || window
            .limit_window_seconds
            .is_some_and(|seconds| seconds < 0)
        || window
            .reset_after_seconds
            .is_some_and(|seconds| seconds < 0)
        || window.reset_at.is_some_and(|timestamp| timestamp < 0)
    {
        return Err(CodexUsageError::InvalidResponse);
    }
    Ok(CodexUsageWindow {
        kind,
        used_percent: window.used_percent,
        limit_window_seconds: window.limit_window_seconds,
        reset_at: window.reset_at,
        reset_after_seconds: window.reset_after_seconds,
    })
}

fn fetched_at() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn no_limits(
    identity: &AuthIdentity,
    plan_type: Option<String>,
    credits: Option<CodexCredits>,
    fetched_at: String,
) -> CodexUsageResponse {
    CodexUsageResponse::NoLimits {
        account_key: identity.account_key.clone(),
        auth_generation: identity.auth_generation.clone(),
        plan_type,
        fetched_at,
        credits,
    }
}

fn transient(identity: Option<&AuthIdentity>, error: CodexUsageError) -> CodexUsageResponse {
    CodexUsageResponse::Transient {
        account_key: identity.map(|value| value.account_key.clone()),
        auth_generation: identity.map(|value| value.auth_generation.clone()),
        error,
        stale_eligible: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMode;
    use chrono::{TimeZone as _, Utc};
    use serial_test::serial;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn auth(token: &str, account: &str, second: i64) -> GrokAuth {
        GrokAuth {
            key: token.into(),
            auth_mode: AuthMode::OpenAiCodex,
            account_id: Some(account.into()),
            create_time: Utc.timestamp_opt(second, 0).unwrap(),
            ..Default::default()
        }
    }

    fn paired_auth(token: &str, account: &str, second: i64) -> PairedAuth {
        (auth(token, account, second), Some("test-login".into()))
    }

    fn identity() -> AuthIdentity {
        AuthIdentity::from_auth(
            &auth("synthetic-token", "acct-synthetic", 10),
            Some("test-login"),
        )
        .unwrap()
    }

    fn parse(json: serde_json::Value) -> CodexUsageResponse {
        parse_success(serde_json::to_vec(&json).unwrap().as_slice(), &identity())
    }

    #[test]
    fn public_quota_responses_roundtrip_for_pager_decoding() {
        let responses = [
            parse(serde_json::json!({
                "plan_type": "pro",
                "rate_limit": {"primary_window": {"used_percent": 25, "limit_window_seconds": 18000}},
                "credits": {"has_credits": true, "balance": "12"}
            })),
            parse(serde_json::json!({})),
            CodexUsageResponse::AuthUnavailable,
            CodexUsageResponse::ForbiddenUnsupported {
                account_key: "account".into(),
                auth_generation: "login".into(),
            },
            transient(Some(&identity()), CodexUsageError::RateLimited),
        ];
        for response in responses {
            let encoded = serde_json::to_string(&response).unwrap();
            let decoded: CodexUsageResponse = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, response);
        }
    }

    #[test]
    fn maps_primary_secondary_and_timestamps_without_unit_conversion() {
        let response = parse(serde_json::json!({
            "plan_type": "pro",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 12.5, "limit_window_seconds": 1234, "reset_at": 99},
                "secondary_window": {"used_percent": 48, "reset_after_seconds": 77}
            }
        }));
        let CodexUsageResponse::Fresh {
            buckets, plan_type, ..
        } = response
        else {
            panic!()
        };
        assert_eq!(plan_type.as_deref(), Some("pro"));
        assert_eq!(buckets[0].windows.len(), 2);
        assert_eq!(buckets[0].windows[0].limit_window_seconds, Some(1234));
        assert_eq!(buckets[0].windows[0].reset_at, Some(99));
        assert_eq!(buckets[0].windows[1].reset_after_seconds, Some(77));
    }

    #[test]
    fn preserves_unknown_spark_buckets_and_optional_credits() {
        let response = parse(serde_json::json!({
            "plan_type": "plus",
            "credits": {"has_credits": true, "unlimited": false, "balance": "9.99"},
            "additional_rate_limits": [{
                "metered_feature": "codex-spark-next", "limit_name": "Unknown Spark",
                "rate_limit": {"primary_window": {"used_percent": 3}}
            }]
        }));
        let CodexUsageResponse::Fresh {
            buckets, credits, ..
        } = response
        else {
            panic!()
        };
        assert_eq!(buckets[0].id, "codex-spark-next");
        assert_eq!(buckets[0].name.as_deref(), Some("Unknown Spark"));
        let credits = credits.unwrap();
        assert_eq!(credits.has_credits, Some(true));
        assert_eq!(credits.unlimited, Some(false));
        assert_eq!(credits.balance.as_deref(), Some("9.99"));
    }

    #[test]
    fn missing_and_null_limits_are_no_limits_not_zero_usage() {
        for json in [
            serde_json::json!({}),
            serde_json::json!({"rate_limit": null, "additional_rate_limits": null}),
        ] {
            assert!(matches!(parse(json), CodexUsageResponse::NoLimits { .. }));
        }
        assert!(matches!(
            parse_success(b"", &identity()),
            CodexUsageResponse::Transient {
                error: CodexUsageError::InvalidResponse,
                stale_eligible: true,
                ..
            }
        ));
    }

    #[test]
    fn malformed_percent_and_timestamps_are_safe_transient_errors() {
        for json in [
            serde_json::json!({"rate_limit":{"primary_window":{"used_percent":"many"}}}),
            serde_json::json!({"rate_limit":{"primary_window":{"used_percent":101}}}),
            serde_json::json!({"rate_limit":{"primary_window":{"reset_at":-1}}}),
        ] {
            assert!(matches!(
                parse(json),
                CodexUsageResponse::Transient {
                    error: CodexUsageError::InvalidResponse,
                    stale_eligible: true,
                    ..
                }
            ));
        }
    }

    #[test]
    fn identity_changes_for_relogin_same_account_without_hashing_bearer() {
        let first_auth = auth("secret-one", "same-account", 10);
        let second_auth = auth("secret-two", "same-account", 11);
        let first = AuthIdentity::from_auth(&first_auth, Some("login-one")).unwrap();
        let second = AuthIdentity::from_auth(&second_auth, Some("login-two")).unwrap();
        assert_eq!(first.account_key, second.account_key);
        assert_ne!(first.auth_generation, second.auth_generation);
        let serialized =
            serde_json::to_string(&no_limits(&first, None, None, fetched_at())).unwrap();
        assert!(!serialized.contains("secret-one"));
        assert!(!serialized.contains("same-account"));
    }

    #[test]
    fn identity_survives_ordinary_token_rotation_with_same_login_epoch() {
        let before_auth = auth("rotating-secret-one", "same-account", 10);
        let after_auth = auth("rotating-secret-two", "same-account", 11);

        let before = AuthIdentity::from_auth(&before_auth, Some("stable-login-epoch")).unwrap();
        let after = AuthIdentity::from_auth(&after_auth, Some("stable-login-epoch")).unwrap();
        assert_eq!(before.account_key, after.account_key);
        assert_eq!(before.auth_generation, after.auth_generation);
    }

    struct Reply {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
        stall: Option<Duration>,
    }

    fn server(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        std::thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 2048];
                loop {
                    let count = stream.read(&mut buffer).unwrap_or(0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if count == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&bytes).into_owned());
                if let Some(stall) = reply.stall {
                    std::thread::sleep(stall);
                    continue;
                }
                let reason = match reply.status {
                    200 => "OK",
                    302 => "Found",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    429 => "Too Many Requests",
                    _ => "Error",
                };
                let has_content_length = reply
                    .headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("Content-Length"));
                let mut head = format!("HTTP/1.1 {} {}\r\n", reply.status, reason);
                if !has_content_length {
                    head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
                }
                head.push_str("Connection: close\r\n");
                for (name, value) in reply.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&reply.body).unwrap();
            }
        });
        (format!("http://{address}/backend-api/wham/usage"), requests)
    }

    fn client(timeout: Duration) -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .unwrap()
    }

    #[test]
    fn completed_fresh_result_is_rejected_after_logout_or_account_replacement() {
        let original = auth("token-old", "account-old", 10);
        let original_identity = AuthIdentity::from_auth(&original, Some("old-login")).unwrap();
        let response = no_limits(&original_identity, Some("pro".into()), None, fetched_at());
        assert_eq!(
            invalidate_completed_result_if_auth_changed(response.clone(), None),
            CodexUsageResponse::AuthUnavailable
        );

        let replacement = auth("token-new", "account-new", 11);
        assert_eq!(
            invalidate_completed_result_if_auth_changed(
                response,
                Some((replacement, Some("new-login".into()))),
            ),
            CodexUsageResponse::AuthUnavailable
        );
    }

    #[test]
    fn completed_result_survives_same_login_token_refresh() {
        let original = auth("token-old", "account", 10);
        let identity = AuthIdentity::from_auth(&original, Some("same-login")).unwrap();
        let response = no_limits(&identity, Some("pro".into()), None, fetched_at());
        let refreshed = (auth("token-new", "account", 11), Some("same-login".into()));
        assert_eq!(
            invalidate_completed_result_if_auth_changed(response.clone(), Some(refreshed)),
            response
        );
    }

    #[tokio::test]
    async fn sends_exact_auth_account_accept_and_user_agent_headers() {
        let (url, requests) = server(vec![Reply {
            status: 200,
            headers: vec![],
            body: b"{}".to_vec(),
            stall: None,
        }]);
        let response = fetch_with_refresh(
            &client(Duration::from_secs(2)),
            &url,
            Duration::from_secs(2),
            Some(paired_auth("synthetic-token", "acct-synthetic", 10)),
            || async { None },
        )
        .await;
        assert!(matches!(response, CodexUsageResponse::NoLimits { .. }));
        let request = requests.lock().unwrap()[0].to_ascii_lowercase();
        assert!(request.starts_with("get /backend-api/wham/usage http/1.1\r\n"));
        assert!(request.contains("authorization: bearer synthetic-token\r\n"));
        assert!(request.contains("chatgpt-account-id: acct-synthetic\r\n"));
        assert!(request.contains("accept: application/json\r\n"));
        assert!(request.contains(
            &format!("user-agent: turbo/{}\r\n", xai_grok_version::VERSION).to_ascii_lowercase()
        ));
    }

    #[tokio::test]
    async fn retries_exactly_once_with_forced_refresh_after_401() {
        let (url, requests) = server(vec![
            Reply {
                status: 401,
                headers: vec![],
                body: vec![],
                stall: None,
            },
            Reply {
                status: 200,
                headers: vec![],
                body: br#"{"plan_type":"pro"}"#.to_vec(),
                stall: None,
            },
        ]);
        let refreshed = paired_auth("fresh-token", "acct-synthetic", 11);
        let response = fetch_with_refresh(
            &client(Duration::from_secs(2)),
            &url,
            Duration::from_secs(2),
            Some(paired_auth("old-token", "acct-synthetic", 10)),
            || {
                let refreshed = refreshed.clone();
                async move { Some(refreshed) }
            },
        )
        .await;
        assert!(matches!(response, CodexUsageResponse::NoLimits { .. }));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("Bearer old-token"));
        assert!(requests[1].contains("Bearer fresh-token"));
    }

    #[tokio::test]
    async fn second_401_is_auth_unavailable() {
        let (url, _) = server(vec![
            Reply {
                status: 401,
                headers: vec![],
                body: vec![],
                stall: None,
            },
            Reply {
                status: 401,
                headers: vec![],
                body: vec![],
                stall: None,
            },
        ]);
        let refreshed = paired_auth("fresh", "acct", 11);
        let response = fetch_with_refresh(
            &client(Duration::from_secs(2)),
            &url,
            Duration::from_secs(2),
            Some(paired_auth("old", "acct", 10)),
            || {
                let refreshed = refreshed.clone();
                async move { Some(refreshed) }
            },
        )
        .await;
        assert_eq!(response, CodexUsageResponse::AuthUnavailable);
    }

    #[tokio::test]
    async fn classifies_redirect_403_429_and_5xx_without_body_disclosure() {
        for (status, expected) in [
            (302, "forbidden"),
            (403, "forbidden"),
            (429, "rate"),
            (503, "server"),
        ] {
            let (url, requests) = server(vec![Reply {
                status,
                headers: if status == 302 {
                    vec![("Location", "https://evil.example/leak".into())]
                } else {
                    vec![]
                },
                body: b"SECRET UPSTREAM BODY".to_vec(),
                stall: None,
            }]);
            let response = fetch_with_refresh(
                &client(Duration::from_secs(2)),
                &url,
                Duration::from_secs(2),
                Some(paired_auth("token", "acct", 10)),
                || async { None },
            )
            .await;
            match expected {
                "forbidden" => assert!(matches!(
                    response,
                    CodexUsageResponse::ForbiddenUnsupported { .. }
                )),
                "rate" => assert!(matches!(
                    response,
                    CodexUsageResponse::Transient {
                        error: CodexUsageError::RateLimited,
                        ..
                    }
                )),
                _ => assert!(matches!(
                    response,
                    CodexUsageResponse::Transient {
                        error: CodexUsageError::Server,
                        ..
                    }
                )),
            }
            assert_eq!(
                requests.lock().unwrap().len(),
                1,
                "redirect must not be followed"
            );
            assert!(!format!("{response:?}").contains("SECRET"));
        }
    }

    #[tokio::test]
    async fn bounds_streamed_and_content_length_responses() {
        for headers in [
            vec![("Content-Length", (MAX_RESPONSE_BYTES + 1).to_string())],
            vec![],
        ] {
            let body = if headers.is_empty() {
                vec![b'x'; MAX_RESPONSE_BYTES + 1]
            } else {
                vec![]
            };
            let (url, _) = server(vec![Reply {
                status: 200,
                headers,
                body,
                stall: None,
            }]);
            let response = fetch_with_refresh(
                &client(Duration::from_secs(2)),
                &url,
                Duration::from_secs(2),
                Some(paired_auth("token", "acct", 10)),
                || async { None },
            )
            .await;
            assert!(matches!(
                response,
                CodexUsageResponse::Transient {
                    error: CodexUsageError::ResponseTooLarge,
                    ..
                }
            ));
        }
    }

    #[tokio::test]
    async fn timeout_is_stale_eligible_transient() {
        let (url, _) = server(vec![Reply {
            status: 200,
            headers: vec![],
            body: vec![],
            stall: Some(Duration::from_millis(200)),
        }]);
        let response = fetch_with_refresh(
            &client(Duration::from_millis(30)),
            &url,
            Duration::from_millis(30),
            Some(paired_auth("token", "acct", 10)),
            || async { None },
        )
        .await;
        assert!(matches!(
            response,
            CodexUsageResponse::Transient {
                error: CodexUsageError::Timeout,
                stale_eligible: true,
                ..
            }
        ));
    }

    #[test]
    #[serial]
    fn production_url_ignores_model_endpoint_override() {
        let _guard = xai_grok_test_support::EnvGuard::set(
            "GROK_OPENAI_CODEX_BASE_URL",
            "http://127.0.0.1:1/attacker",
        );
        assert_eq!(
            CODEX_USAGE_URL,
            "https://chatgpt.com/backend-api/wham/usage"
        );
    }

    #[test]
    fn serialized_response_has_stable_public_shape_and_no_credentials() {
        let response = parse(serde_json::json!({
            "plan_type":"pro",
            "rate_limit":{"primary_window":{"used_percent":25,"limit_window_seconds":3600,"reset_at":123}}
        }));
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["status"], "fresh");
        assert_eq!(value["planType"], "pro");
        assert_eq!(value["buckets"][0]["id"], "codex");
        assert!(value.get("key").is_none());
        let text = value.to_string();
        assert!(!text.contains("synthetic-token"));
        assert!(!text.contains("acct-synthetic"));
    }
}
