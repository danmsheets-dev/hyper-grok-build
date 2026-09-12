//! Provider-aware OpenAI Codex quota state and presentation.

use std::time::{Duration, Instant};

use agent_client_protocol as acp;
use chrono::{DateTime, TimeZone as _, Utc};
use xai_grok_shell::extensions::codex_usage::{
    CodexCredits, CodexUsageBucket, CodexUsageError, CodexUsageResponse, CodexUsageWindow,
    CodexUsageWindowKind,
};

use crate::app::actions::Effect;
use crate::app::agent::AgentId;
use crate::app::app_view::{ActiveView, AppView};

const BACKGROUND_TTL: Duration = Duration::from_secs(60);
const MAX_PROVIDER_LABEL_CHARS: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowanceProvider {
    Codex,
    Xai,
    Unsupported(String),
}

pub fn allowance_provider(model_id: Option<&str>) -> AllowanceProvider {
    let Some(model_id) = model_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return AllowanceProvider::Xai;
    };
    let lower = model_id.to_ascii_lowercase();
    if lower.starts_with("openai-codex/") || lower.starts_with("codex:") {
        return AllowanceProvider::Codex;
    }
    if lower.starts_with("grok-")
        || lower.starts_with("grok:")
        || lower.starts_with("xai/")
        || !lower.contains('/')
    {
        return AllowanceProvider::Xai;
    }
    let provider = model_id.split_once('/').map_or(model_id, |(provider, _)| provider);
    AllowanceProvider::Unsupported(sanitize_provider_label(provider))
}

pub fn sanitize_provider_label(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .filter(|c| !crate::render::line_utils::is_unsafe_display_char(*c))
        .take(MAX_PROVIDER_LABEL_CHARS)
        .collect();
    let sanitized = sanitized.trim();
    if sanitized.is_empty() {
        "unknown provider".to_string()
    } else {
        sanitized.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexQuotaTarget {
    pub seq: u64,
    pub agent_id: Option<AgentId>,
    pub session_id: Option<acp::SessionId>,
    pub session_binding_epoch: Option<u32>,
    pub model_switch_generation: u64,
    pub model_id: String,
    pub modal_nonce: u64,
    pub manual: bool,
}

#[derive(Debug, Clone)]
pub struct CodexQuotaSnapshot {
    pub account_key: String,
    pub auth_generation: String,
    pub plan_type: Option<String>,
    pub fetched_at: String,
    pub buckets: Vec<CodexUsageBucket>,
    pub credits: Option<CodexCredits>,
}

#[derive(Debug, Clone)]
pub enum CodexQuotaDisplay {
    Loading,
    Fresh(CodexQuotaSnapshot),
    NoLimits {
        account_key: String,
        auth_generation: String,
        plan_type: Option<String>,
        fetched_at: String,
        credits: Option<CodexCredits>,
    },
    AuthUnavailable,
    ForbiddenUnsupported,
    Transient {
        error: CodexUsageError,
        stale: Option<CodexQuotaSnapshot>,
    },
}

#[derive(Debug, Default)]
pub struct CodexQuotaState {
    pub latest_issued: u64,
    pub in_flight: Option<CodexQuotaTarget>,
    pub display: Option<CodexQuotaDisplay>,
    pub last_good: Option<CodexQuotaSnapshot>,
    pub last_completed_at: Option<Instant>,
    pub last_completed_target: Option<CodexQuotaTarget>,
}

fn current_target(app: &AppView, manual: bool, modal_nonce: u64) -> Option<CodexQuotaTarget> {
    match app.active_view {
        ActiveView::Agent(agent_id) => {
            let agent = app.agents.get(&agent_id)?;
            let model_id = agent.session.models.current.as_ref()?.0.to_string();
            matches!(allowance_provider(Some(&model_id)), AllowanceProvider::Codex).then(|| {
                CodexQuotaTarget {
                    seq: 0,
                    agent_id: Some(agent_id),
                    session_id: agent.session.session_id.clone(),
                    session_binding_epoch: Some(agent.session_binding_epoch),
                    model_switch_generation: agent.session.model_switch_generation,
                    model_id,
                    modal_nonce,
                    manual,
                }
            })
        }
        ActiveView::Welcome => {
            let model_id = app.models.current.as_ref()?.0.to_string();
            matches!(allowance_provider(Some(&model_id)), AllowanceProvider::Codex).then(|| {
                CodexQuotaTarget {
                    seq: 0,
                    agent_id: None,
                    session_id: None,
                    session_binding_epoch: None,
                    model_switch_generation: 0,
                    model_id,
                    modal_nonce,
                    manual,
                }
            })
        }
        _ => None,
    }
}

fn same_request(a: &CodexQuotaTarget, b: &CodexQuotaTarget) -> bool {
    a.agent_id == b.agent_id
        && a.session_id == b.session_id
        && a.session_binding_epoch == b.session_binding_epoch
        && a.model_switch_generation == b.model_switch_generation
        && a.model_id == b.model_id
}

pub fn request_for_current(
    app: &mut AppView,
    manual: bool,
    modal_nonce: u64,
) -> Vec<Effect> {
    let Some(mut target) = current_target(app, manual, modal_nonce) else {
        return vec![];
    };
    if app
        .codex_quota
        .in_flight
        .as_ref()
        .is_some_and(|pending| same_request(pending, &target))
    {
        return vec![];
    }
    if !manual
        && app
            .codex_quota
            .last_completed_at
            .is_some_and(|at| at.elapsed() < BACKGROUND_TTL)
        && app
            .codex_quota
            .last_completed_target
            .as_ref()
            .is_some_and(|completed| same_request(completed, &target))
    {
        return vec![];
    }
    app.codex_quota.latest_issued = app.codex_quota.latest_issued.wrapping_add(1).max(1);
    target.seq = app.codex_quota.latest_issued;
    app.codex_quota.in_flight = Some(target.clone());
    if app.codex_quota.display.is_none() {
        app.codex_quota.display = Some(CodexQuotaDisplay::Loading);
    }
    let loading_display = app.codex_quota.display.clone();
    if let Some(agent_id) = target.agent_id
        && let Some(agent) = app.agents.get_mut(&agent_id)
        && let Some(crate::views::modal::ActiveModal::UsageInfo { state }) =
            agent.active_modal.as_mut()
        && state.fetch_nonce == modal_nonce
    {
        state.codex_quota = loading_display;
    }
    vec![Effect::FetchCodexQuota { target }]
}

fn target_is_current(app: &AppView, target: &CodexQuotaTarget) -> bool {
    if target.seq != app.codex_quota.latest_issued
        || app
            .codex_quota
            .in_flight
            .as_ref()
            .is_none_or(|pending| pending.seq != target.seq)
    {
        return false;
    }
    match target.agent_id {
        Some(agent_id) => {
            let Some(agent) = app.agents.get(&agent_id) else {
                return false;
            };
            agent.session.session_id == target.session_id
                && Some(agent.session_binding_epoch) == target.session_binding_epoch
                && agent.session.model_switch_generation == target.model_switch_generation
                && agent.session.models.current.as_ref().map(|id| id.0.as_ref())
                    == Some(target.model_id.as_str())
                && matches!(allowance_provider(Some(&target.model_id)), AllowanceProvider::Codex)
        }
        None => {
            matches!(app.active_view, ActiveView::Welcome)
                && app.models.current.as_ref().map(|id| id.0.as_ref())
                    == Some(target.model_id.as_str())
                && matches!(allowance_provider(Some(&target.model_id)), AllowanceProvider::Codex)
        }
    }
}

pub fn handle_result(
    app: &mut AppView,
    target: CodexQuotaTarget,
    response: CodexUsageResponse,
) -> Vec<Effect> {
    if !target_is_current(app, &target) {
        return vec![];
    }

    let display = match response {
        CodexUsageResponse::Fresh {
            account_key,
            auth_generation,
            plan_type,
            fetched_at,
            buckets,
            credits,
        } => {
            let snapshot = CodexQuotaSnapshot {
                account_key,
                auth_generation,
                plan_type,
                fetched_at,
                buckets,
                credits,
            };
            app.codex_quota.last_good = Some(snapshot.clone());
            CodexQuotaDisplay::Fresh(snapshot)
        }
        CodexUsageResponse::NoLimits {
            account_key,
            auth_generation,
            plan_type,
            fetched_at,
            credits,
        } => {
            app.codex_quota.last_good = None;
            CodexQuotaDisplay::NoLimits {
                account_key,
                auth_generation,
                plan_type,
                fetched_at,
                credits,
            }
        }
        CodexUsageResponse::AuthUnavailable => {
            app.codex_quota.last_good = None;
            CodexQuotaDisplay::AuthUnavailable
        }
        CodexUsageResponse::ForbiddenUnsupported { .. } => {
            app.codex_quota.last_good = None;
            CodexQuotaDisplay::ForbiddenUnsupported
        }
        CodexUsageResponse::Transient {
            account_key,
            auth_generation,
            error,
            stale_eligible,
        } => {
            let stale = stale_eligible.then(|| app.codex_quota.last_good.clone()).flatten().filter(
                |cached| {
                    account_key.as_deref() == Some(cached.account_key.as_str())
                        && auth_generation.as_deref() == Some(cached.auth_generation.as_str())
                },
            );
            CodexQuotaDisplay::Transient { error, stale }
        }
    };
    app.codex_quota.in_flight = None;
    app.codex_quota.last_completed_at = Some(Instant::now());
    app.codex_quota.last_completed_target = Some(target.clone());
    app.codex_quota.display = Some(display.clone());

    if let Some(agent_id) = target.agent_id
        && let Some(agent) = app.agents.get_mut(&agent_id)
    {
        agent.codex_quota = Some(display.clone());
        if let Some(crate::views::modal::ActiveModal::UsageInfo { state }) =
            agent.active_modal.as_mut()
            && state.fetch_nonce == target.modal_nonce
        {
            state.codex_quota = Some(display.clone());
        }
        if target.manual && app.screen_mode.is_minimal() {
            agent.scrollback.push_block(crate::scrollback::block::RenderBlock::system(
                display_text(&display),
            ));
        }
    }
    vec![]
}

pub fn display_text(display: &CodexQuotaDisplay) -> String {
    let mut lines = vec!["OpenAI Codex allowance".to_string()];
    match display {
        CodexQuotaDisplay::Loading => lines.push("Loading usage…".to_string()),
        CodexQuotaDisplay::Fresh(snapshot) => {
            if let Some(plan) = snapshot.plan_type.as_deref().map(sanitize_provider_label) {
                lines[0].push_str(&format!(" ({plan})"));
            }
            if snapshot.buckets.is_empty() {
                lines.push("No rate-limit buckets were reported.".to_string());
            }
            for bucket in &snapshot.buckets {
                append_bucket(&mut lines, bucket, &snapshot.fetched_at);
            }
            append_credits(&mut lines, snapshot.credits.as_ref());
        }
        CodexQuotaDisplay::NoLimits {
            plan_type, credits, ..
        } => {
            if let Some(plan) = plan_type.as_deref().map(sanitize_provider_label) {
                lines[0].push_str(&format!(" ({plan})"));
            }
            lines.push("No rate limits were reported for this account.".to_string());
            append_credits(&mut lines, credits.as_ref());
        }
        CodexQuotaDisplay::AuthUnavailable => {
            lines.push("Sign in with /login openai to view limits.".to_string())
        }
        CodexQuotaDisplay::ForbiddenUnsupported => lines.push(
            "OpenAI did not make subscription limits available for this account.".to_string(),
        ),
        CodexQuotaDisplay::Transient { error, stale } => {
            lines.push(format!("Refresh failed: {}.", error_label(*error)));
            if let Some(snapshot) = stale {
                lines.push(format!("Showing cached data from {}.", age_label(&snapshot.fetched_at)));
                for bucket in &snapshot.buckets {
                    append_bucket(&mut lines, bucket, &snapshot.fetched_at);
                }
                append_credits(&mut lines, snapshot.credits.as_ref());
            }
        }
    }
    lines.join("\n")
}

fn append_bucket(lines: &mut Vec<String>, bucket: &CodexUsageBucket, fetched_at: &str) {
    let name = bucket
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&bucket.id);
    let name = sanitize_provider_label(name);
    let status = match (bucket.allowed, bucket.limit_reached) {
        (_, Some(true)) | (Some(false), _) => " — limit reached",
        (Some(true), Some(false)) => " — allowed",
        _ => "",
    };
    lines.push(format!("{name}{status}"));
    if bucket.windows.is_empty() {
        lines.push("  Window details unavailable".to_string());
    }
    for window in &bucket.windows {
        lines.push(format_window(window, fetched_at));
    }
}

fn format_window(window: &CodexUsageWindow, fetched_at: &str) -> String {
    let kind = match window.kind {
        CodexUsageWindowKind::Primary => "Primary",
        CodexUsageWindowKind::Secondary => "Secondary",
    };
    let usage = window
        .used_percent
        .filter(|value| value.is_finite())
        .map(|value| format!("{:.0}% used", value.clamp(0.0, 100.0)))
        .unwrap_or_else(|| "usage unknown".to_string());
    let duration = window
        .limit_window_seconds
        .map(duration_label)
        .unwrap_or_else(|| "window unknown".to_string());
    let reset = reset_time(window, fetched_at)
        .map(|when| when.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "reset unknown".to_string());
    format!("  {kind}: {usage} · {duration} · resets {reset}")
}

fn reset_time(window: &CodexUsageWindow, fetched_at: &str) -> Option<DateTime<Utc>> {
    if let Some(timestamp) = window.reset_at {
        return Utc.timestamp_opt(timestamp, 0).single();
    }
    let fetched = DateTime::parse_from_rfc3339(fetched_at).ok()?.with_timezone(&Utc);
    fetched.checked_add_signed(chrono::Duration::seconds(window.reset_after_seconds?))
}

fn duration_label(seconds: i64) -> String {
    if seconds <= 0 {
        return "window unknown".to_string();
    }
    if seconds % 86_400 == 0 {
        format!("{}d window", seconds / 86_400)
    } else if seconds % 3_600 == 0 {
        format!("{}h window", seconds / 3_600)
    } else if seconds % 60 == 0 {
        format!("{}m window", seconds / 60)
    } else {
        format!("{seconds}s window")
    }
}

fn append_credits(lines: &mut Vec<String>, credits: Option<&CodexCredits>) {
    let Some(credits) = credits else { return };
    if credits.unlimited == Some(true) {
        lines.push("Credits: unlimited".to_string());
    } else if let Some(balance) = credits.balance.as_deref() {
        lines.push(format!("Credits balance: {}", sanitize_provider_label(balance)));
    } else if credits.has_credits == Some(true) {
        lines.push("Credits: available (balance not reported)".to_string());
    }
}

fn error_label(error: CodexUsageError) -> &'static str {
    match error {
        CodexUsageError::Timeout => "request timed out",
        CodexUsageError::Network => "network error",
        CodexUsageError::RateLimited => "rate limited",
        CodexUsageError::Server => "server error",
        CodexUsageError::InvalidResponse => "invalid response",
        CodexUsageError::ResponseTooLarge => "response too large",
    }
}

fn age_label(fetched_at: &str) -> String {
    let Ok(fetched) = DateTime::parse_from_rfc3339(fetched_at) else {
        return "an unknown time".to_string();
    };
    let seconds = Utc::now()
        .signed_duration_since(fetched.with_timezone(&Utc))
        .num_seconds()
        .max(0);
    if seconds < 60 {
        "less than a minute ago".to_string()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

pub fn warning(display: Option<&CodexQuotaDisplay>) -> Option<(String, bool)> {
    let snapshot = match display? {
        CodexQuotaDisplay::Fresh(snapshot) => snapshot,
        CodexQuotaDisplay::Transient { stale: Some(snapshot), .. } => snapshot,
        _ => return None,
    };
    let reached = snapshot
        .buckets
        .iter()
        .any(|bucket| bucket.limit_reached == Some(true) || bucket.allowed == Some(false));
    reached.then(|| ("OpenAI Codex: a usage bucket reached its limit".to_string(), true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(kind: CodexUsageWindowKind) -> CodexUsageWindow {
        CodexUsageWindow {
            kind,
            used_percent: Some(42.4),
            limit_window_seconds: Some(7_200),
            reset_at: None,
            reset_after_seconds: Some(60),
        }
    }

    #[test]
    fn provider_routing_never_treats_openai_api_key_as_codex_or_xai() {
        assert_eq!(allowance_provider(Some("openai-codex/gpt-5.6-sol")), AllowanceProvider::Codex);
        assert_eq!(allowance_provider(Some("codex:gpt-5.4")), AllowanceProvider::Codex);
        assert_eq!(allowance_provider(Some("grok-4.1")), AllowanceProvider::Xai);
        assert_eq!(allowance_provider(Some("openai/gpt-5")), AllowanceProvider::Unsupported("openai".into()));
    }

    #[test]
    fn fresh_render_preserves_buckets_windows_unknowns_and_credit_units() {
        let display = CodexQuotaDisplay::Fresh(CodexQuotaSnapshot {
            account_key: "a".into(),
            auth_generation: "g".into(),
            plan_type: Some("Plus".into()),
            fetched_at: "2026-09-05T12:00:00Z".into(),
            buckets: vec![
                CodexUsageBucket {
                    id: "codex".into(),
                    name: None,
                    allowed: Some(true),
                    limit_reached: Some(false),
                    windows: vec![window(CodexUsageWindowKind::Primary), window(CodexUsageWindowKind::Secondary)],
                },
                CodexUsageBucket {
                    id: "spark".into(),
                    name: Some("Spark\u{1b}[31m".into()),
                    allowed: None,
                    limit_reached: None,
                    windows: vec![],
                },
            ],
            credits: Some(CodexCredits { has_credits: Some(true), unlimited: None, balance: Some("12.50 points".into()) }),
        });
        let text = display_text(&display);
        assert!(text.contains("Primary: 42% used · 2h window · resets 2026-09-05 12:01 UTC"));
        assert!(text.contains("Secondary"));
        assert!(text.contains("Spark[31m"));
        assert!(text.contains("Window details unavailable"));
        assert!(text.contains("Credits balance: 12.50 points"));
        assert!(!text.contains('$'));
    }

    #[test]
    fn missing_and_expired_reset_values_remain_honest() {
        let missing = CodexUsageWindow { kind: CodexUsageWindowKind::Primary, used_percent: None, limit_window_seconds: None, reset_at: None, reset_after_seconds: None };
        assert_eq!(format_window(&missing, "bad"), "  Primary: usage unknown · window unknown · resets reset unknown");
        let expired = CodexUsageWindow { reset_at: Some(1), ..missing };
        assert!(format_window(&expired, "bad").contains("1970-01-01 00:00 UTC"));
    }

    #[test]
    fn warning_does_not_enforce_from_percent_alone() {
        let snapshot = CodexQuotaSnapshot {
            account_key: "a".into(), auth_generation: "g".into(), plan_type: None,
            fetched_at: "2026-09-05T12:00:00Z".into(),
            buckets: vec![CodexUsageBucket { id: "main".into(), name: None, allowed: Some(true), limit_reached: Some(false), windows: vec![CodexUsageWindow { used_percent: Some(100.0), ..window(CodexUsageWindowKind::Primary) }] }],
            credits: None,
        };
        assert!(warning(Some(&CodexQuotaDisplay::Fresh(snapshot))).is_none());
    }
}
