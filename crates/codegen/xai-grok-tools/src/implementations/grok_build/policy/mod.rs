//! Context-aware session policy engine (FR fr_01a028e7bf6875339bfa220a9adc5c9b).
//!
//! Repo/session source: `.grok/policy.toml` (preferred) or `grok.toml` `[policy]`.
//! Operator overlay: `GROK_POLICY_*` env vars. The pager/shell host injects the
//! resolved snapshot as `Params<PolicyParams>` at session start.
//!
//! **Enforced at tool dispatch** ([`FinalizedToolset::call`](crate::registry::types::FinalizedToolset))
//! before execute — not just in the system prompt. File-edit tools still re-check
//! `deny_paths` / `max_diff_lines` before I/O; bash/monitor re-check `deny_commands`.
//! Violations fail closed as [`POLICY_DENIED`] (`ToolError::custom`).

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::types::tool::ToolKind;

/// Custom `ToolError` code for every policy refusal.
pub const POLICY_DENIED: &str = "policy_denied";

/// Workspace-relative path of the dedicated policy file.
pub const POLICY_TOML_RELATIVE: &str = ".grok/policy.toml";

/// Alternate project file that may carry a `[policy]` table.
pub const GROK_TOML_RELATIVE: &str = "grok.toml";

/// Host/config-injected policy params.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyParams {
    /// Path fragments refused for edits (component-suffix match, case-insensitive).
    #[serde(default)]
    pub deny_paths: Vec<String>,
    /// Command substrings refused in bash (case-insensitive).
    #[serde(default)]
    pub deny_commands: Vec<String>,
    /// Refuse an edit whose replacement adds more than this many lines.
    #[serde(default)]
    pub max_diff_lines: Option<u64>,
    /// Tool kinds / names refused at dispatch (`edit`, `write`, `execute`, …).
    #[serde(default)]
    pub deny_tool_classes: Vec<String>,
    /// Tool kinds / names that require `confirm: true` in the call args.
    #[serde(default)]
    pub require_confirm_for: Vec<String>,
    /// Set when a policy file existed but could not be read or parsed.
    /// Dispatch fails closed until the file is fixed.
    #[serde(skip)]
    pub load_error: Option<String>,
}

/// On-disk TOML shape (flat keys, or nested under `[policy]`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    #[serde(default)]
    max_diff_lines: Option<u64>,
    #[serde(default)]
    deny_paths: Vec<String>,
    #[serde(default)]
    deny_commands: Vec<String>,
    #[serde(default)]
    deny_tool_classes: Vec<String>,
    #[serde(default)]
    require_confirm_for: Vec<String>,
}

impl From<PolicyFile> for PolicyParams {
    fn from(file: PolicyFile) -> Self {
        Self {
            deny_paths: file.deny_paths,
            deny_commands: file.deny_commands,
            max_diff_lines: file.max_diff_lines,
            deny_tool_classes: file.deny_tool_classes,
            require_confirm_for: file.require_confirm_for,
            load_error: None,
        }
    }
}

impl PolicyParams {
    /// Resolve from env first (so operators can gate without code changes),
    /// falling back to injected params for unset keys.
    pub fn resolve(injected: Option<&Self>) -> Self {
        Self::resolve_from(injected, None)
    }

    /// File (workspace) → injected snapshot → `GROK_POLICY_*` env (highest).
    ///
    /// A malformed / unreadable policy file fails closed and is **not**
    /// overwritten by injected params.
    pub fn resolve_from(injected: Option<&Self>, workspace: Option<&Path>) -> Self {
        let file = workspace.map(Self::load_from_workspace);
        if let Some(file) = file.as_ref()
            && file.load_error.is_some()
        {
            return file.clone();
        }
        let mut out = injected.cloned().unwrap_or_default();
        if let Some(file) = file.as_ref() {
            out.overlay_file(file);
        }
        out.apply_env();
        out
    }

    /// Load `.grok/policy.toml` or `grok.toml` `[policy]` from `dir`.
    /// Missing files → default (no policy). Present-but-broken → `load_error`.
    pub fn load_from_workspace(dir: &Path) -> Self {
        let dedicated = dir.join(POLICY_TOML_RELATIVE);
        if dedicated.is_file() {
            return load_policy_file(&dedicated, PolicySource::Dedicated);
        }
        let grok_toml = dir.join(GROK_TOML_RELATIVE);
        if grok_toml.is_file() {
            return load_policy_file(&grok_toml, PolicySource::GrokToml);
        }
        Self::default()
    }

    fn overlay_file(&mut self, file: &Self) {
        if !file.deny_paths.is_empty() {
            self.deny_paths = file.deny_paths.clone();
        }
        if !file.deny_commands.is_empty() {
            self.deny_commands = file.deny_commands.clone();
        }
        if !file.deny_tool_classes.is_empty() {
            self.deny_tool_classes = file.deny_tool_classes.clone();
        }
        if !file.require_confirm_for.is_empty() {
            self.require_confirm_for = file.require_confirm_for.clone();
        }
        if file.max_diff_lines.is_some() {
            self.max_diff_lines = file.max_diff_lines;
        }
    }

    fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("GROK_POLICY_DENY_PATHS") {
            let parsed = split_env_list(&v);
            if !parsed.is_empty() {
                self.deny_paths = parsed;
            }
        }
        if let Ok(v) = std::env::var("GROK_POLICY_DENY_COMMANDS") {
            let parsed = split_env_list(&v);
            if !parsed.is_empty() {
                self.deny_commands = parsed;
            }
        }
        if let Ok(v) = std::env::var("GROK_POLICY_DENY_TOOL_CLASSES") {
            let parsed = split_env_list(&v);
            if !parsed.is_empty() {
                self.deny_tool_classes = parsed;
            }
        }
        if let Ok(v) = std::env::var("GROK_POLICY_REQUIRE_CONFIRM_FOR") {
            let parsed = split_env_list(&v);
            if !parsed.is_empty() {
                self.require_confirm_for = parsed;
            }
        }
        if let Ok(v) = std::env::var("GROK_POLICY_MAX_DIFF_LINES")
            && let Ok(n) = v.trim().parse::<u64>()
        {
            self.max_diff_lines = Some(n);
        }
    }

    /// Fail closed at the dispatcher before the tool body runs.
    pub fn enforce_dispatch(
        &self,
        tool_name: &str,
        tool_kind: Option<ToolKind>,
        args: &serde_json::Value,
    ) -> Result<(), xai_tool_runtime::ToolError> {
        self.enforce_dispatch_with(tool_name, tool_kind, args, false)
    }

    /// [`Self::enforce_dispatch`], with the added-line count behind
    /// `max_diff_lines` bounded in time and memory when `bounded` is set, as it
    /// must be for text from an untrusted client.
    pub fn enforce_dispatch_with(
        &self,
        tool_name: &str,
        tool_kind: Option<ToolKind>,
        args: &serde_json::Value,
        bounded: bool,
    ) -> Result<(), xai_tool_runtime::ToolError> {
        if let Some(err) = &self.load_error {
            return Err(denied_error(tool_name, "policy_file", err));
        }
        if let Some(class) = self.class_listed(&self.deny_tool_classes, tool_name, tool_kind) {
            return Err(denied_error(
                tool_name,
                "deny_tool_classes",
                &format!("tool class `{class}`"),
            ));
        }
        if let Some(class) = self.class_listed(&self.require_confirm_for, tool_name, tool_kind)
            && !args_confirmed(args)
        {
            return Err(denied_error(
                tool_name,
                "require_confirm_for",
                &format!("tool class `{class}` requires `confirm: true`"),
            ));
        }
        if let Some(path) = dispatch_path(args)
            && let Some(frag) = self.path_denied(Path::new(path))
        {
            return Err(denied_error(
                tool_name,
                "deny_paths",
                &format!("`{path}` — matched deny-path fragment `{frag}`"),
            ));
        }
        // Counting added lines diffs the arguments, so it runs only under a limit.
        if self.max_diff_lines.is_some()
            && let Some((added, exact)) = dispatch_added_lines(args, bounded)
            && let Some((added, max)) = self.diff_exceeds_limit(added)
        {
            return Err(denied_error(
                tool_name,
                "max_diff_lines",
                &diff_limit_detail(added, max, exact),
            ));
        }
        Ok(())
    }

    /// True when any deny-path fragment matches a path component suffix
    /// (`target/` matches `repo/target/debug`, `.env` matches `.env.local`).
    pub fn path_denied(&self, path: &Path) -> Option<String> {
        if self.deny_paths.is_empty() {
            return None;
        }
        let normalized = path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        for frag in &self.deny_paths {
            let frag = frag.trim();
            if frag.is_empty() {
                continue;
            }
            let needle = frag.replace('\\', "/").to_ascii_lowercase();
            let needle = needle.strip_suffix('/').unwrap_or(&needle);
            if normalized.contains(&format!("/{needle}/"))
                || normalized.ends_with(&format!("/{needle}"))
                || normalized
                    .split('/')
                    .any(|c| c == needle || (needle.starts_with('.') && c.starts_with(needle)))
            {
                return Some(frag.to_owned());
            }
        }
        None
    }

    /// True when any deny-command substring appears in the command.
    ///
    /// Matches the raw string and a dequoted/whitespace-collapsed haystack so
    /// `cu''rl`, `cur\l`, and `rm  -rf` cannot dodge a `curl` / `rm -rf` rule
    /// (F59). ANSI-C `$'\143url'` is decoded. This is still not a full shell
    /// parser — glob/`eval` assembly can evade; those belong on `[permission]`.
    pub fn command_denied(&self, command: &str) -> Option<String> {
        let raw = command.to_ascii_lowercase();
        let dequoted = dequote_shell_haystack(command).to_ascii_lowercase();
        self.deny_commands
            .iter()
            .find(|f| {
                let needle = f.trim().to_ascii_lowercase();
                if needle.is_empty() {
                    return false;
                }
                let collapsed = collapse_ws(&needle);
                raw.contains(&needle)
                    || dequoted.contains(&needle)
                    || collapse_ws(&raw).contains(&collapsed)
                    || collapse_ws(&dequoted).contains(&collapsed)
            })
            .cloned()
    }

    /// True when an edit adding `added_lines` exceeds the configured ceiling.
    pub fn diff_exceeds_limit(&self, added_lines: u64) -> Option<(u64, u64)> {
        let max = self.max_diff_lines?;
        if added_lines > max {
            Some((added_lines, max))
        } else {
            None
        }
    }

    fn class_listed(
        &self,
        list: &[String],
        tool_name: &str,
        kind: Option<ToolKind>,
    ) -> Option<String> {
        let name = tool_name.to_ascii_lowercase();
        let kind_key = kind.map(|k| k.as_key().to_ascii_lowercase());
        for item in list {
            let needle = item.trim().to_ascii_lowercase();
            if needle.is_empty() {
                continue;
            }
            if name == needle || kind_key.as_deref() == Some(needle.as_str()) {
                return Some(item.trim().to_owned());
            }
        }
        None
    }
}

enum PolicySource {
    Dedicated,
    GrokToml,
}

fn load_policy_file(path: &Path, source: PolicySource) -> PolicyParams {
    match std::fs::read_to_string(path) {
        Ok(raw) => match parse_policy_toml(&raw, source) {
            Ok(file) => PolicyParams::from(file),
            Err(e) => broken_policy(path, e),
        },
        Err(e) => broken_policy(path, e),
    }
}

fn parse_policy_toml(raw: &str, source: PolicySource) -> Result<PolicyFile, String> {
    let value: toml::Value = toml::from_str(raw).map_err(|e| format!("invalid TOML: {e}"))?;
    match source {
        PolicySource::Dedicated => {
            if let Some(inner) = value.get("policy") {
                inner
                    .clone()
                    .try_into()
                    .map_err(|e| format!("invalid [policy] table: {e}"))
            } else {
                value
                    .try_into()
                    .map_err(|e| format!("invalid policy.toml: {e}"))
            }
        }
        PolicySource::GrokToml => match value.get("policy") {
            None => Ok(PolicyFile::default()),
            Some(inner) => inner
                .clone()
                .try_into()
                .map_err(|e| format!("invalid [policy] table: {e}")),
        },
    }
}

fn broken_policy(path: &Path, err: impl std::fmt::Display) -> PolicyParams {
    PolicyParams {
        load_error: Some(format!("{}: {err}", path.display())),
        ..Default::default()
    }
}

fn dispatch_path(args: &serde_json::Value) -> Option<&str> {
    ["file_path", "path", "target_file"]
        .iter()
        .find_map(|k| args.get(*k).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Lines the edit in `args` adds, and whether that count is exact.
fn dispatch_added_lines(args: &serde_json::Value, bounded: bool) -> Option<(u64, bool)> {
    let new = args
        .get("new_string")
        .and_then(serde_json::Value::as_str)
        .or_else(|| args.get("content").and_then(serde_json::Value::as_str))
        .or_else(|| args.get("contents").and_then(serde_json::Value::as_str))?;
    let old = args
        .get("old_string")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    Some(if bounded {
        let count = crate::types::output::line_diff_bounded(old, new);
        (count.added.max(0) as u64, count.exact)
    } else {
        (
            crate::types::output::line_diff(old, new).0.max(0) as u64,
            true,
        )
    })
}

/// The detail of a `max_diff_lines` refusal. An inexact count can be higher
/// than the lines the edit really adds, so the detail says so.
pub fn diff_limit_detail(added: u64, max: u64, exact: bool) -> String {
    if exact {
        format!("edit adding {added} lines (limit {max})")
    } else {
        format!("edit adding up to {added} lines, too many to count exactly (limit {max})")
    }
}

fn args_confirmed(args: &serde_json::Value) -> bool {
    args.get("confirm").and_then(serde_json::Value::as_bool) == Some(true)
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            prev_space = false;
            out.push(c);
        }
    }
    out
}

/// Strip shell quotes/backslashes and decode `$'\ooo'` / `$'\xHH'` so a
/// substring deny list sees the command the shell will run, not the spelling.
fn dequote_shell_haystack(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        // ANSI-C quoting: $'...'
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\'' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                    match bytes[i] {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'\\' | b'\'' => out.push(bytes[i] as char),
                        b'x' if i + 2 < bytes.len() => {
                            let hex = &s[i + 1..i + 3];
                            if let Ok(v) = u8::from_str_radix(hex, 16) {
                                out.push(v as char);
                                i += 2;
                            } else {
                                out.push('x');
                            }
                        }
                        d if d.is_ascii_digit() => {
                            let mut val = (d - b'0') as u32;
                            let mut n = 1;
                            while n < 3 && i + n < bytes.len() && bytes[i + n].is_ascii_digit() {
                                val = val * 8 + (bytes[i + n] - b'0') as u32;
                                n += 1;
                            }
                            if let Some(ch) = char::from_u32(val) {
                                out.push(ch);
                            }
                            i += n - 1;
                        }
                        other => out.push(other as char),
                    }
                    i += 1;
                    continue;
                }
                out.push(bytes[i] as char);
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // closing quote
            }
            continue;
        }
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                i += 1;
                out.push(bytes[i] as char);
            }
            b'\'' | b'"' | b'`' => {}
            c if c.is_ascii_whitespace() => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            c => out.push(c as char),
        }
        i += 1;
    }
    out
}

fn split_env_list(raw: &str) -> Vec<String> {
    raw.split([';', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Named refusal message builders shared by the enforcement sites.
pub fn denial(tool: &str, rule: &str, detail: &str) -> String {
    format!("policy denied ({tool}): rule `{rule}` matched {detail}")
}

/// Fail-closed `ToolError` with code [`POLICY_DENIED`].
pub fn denied_error(tool: &str, rule: &str, detail: &str) -> xai_tool_runtime::ToolError {
    xai_tool_runtime::ToolError::custom(POLICY_DENIED, denial(tool, rule, detail))
}

/// True when `path` is a `$GROK_HOME` credential file that must not be mutated.
pub fn grok_home_credential_denied(path: &Path) -> bool {
    xai_grok_sandbox::write_denied_grok_home_credential(path)
}

/// Model-facing refusal for grok-home credential writes.
pub fn grok_home_credential_denial(tool: &str, path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("credential");
    denial(
        tool,
        "grok_home_credentials",
        &format!("`{}` (`{name}` under $GROK_HOME)", path.display()),
    ) + " — use `/providers` or the platform login flow, not file edits"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;

    use crate::registry::types::FinalizedToolset;
    use crate::types::resources::{Cwd, Params};
    use crate::types::tool::ToolNamespace;
    use crate::types::tool_metadata::ToolMetadata;

    fn policy(deny_paths: &[&str], deny_commands: &[&str], max: Option<u64>) -> PolicyParams {
        PolicyParams {
            deny_paths: deny_paths.iter().map(|s| s.to_string()).collect(),
            deny_commands: deny_commands.iter().map(|s| s.to_string()).collect(),
            max_diff_lines: max,
            ..Default::default()
        }
    }

    fn write_policy_toml(dir: &Path, body: &str) -> PathBuf {
        let grok = dir.join(".grok");
        fs::create_dir_all(&grok).unwrap();
        let path = grok.join("policy.toml");
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn deny_paths_match_components_and_suffixes() {
        let p = policy(&["production", ".env"], &[], None);
        assert!(
            p.path_denied(std::path::Path::new("src/production/main.rs"))
                .is_some()
        );
        assert!(p.path_denied(std::path::Path::new(".env")).is_some());
        assert!(
            p.path_denied(std::path::Path::new("config/.env.local"))
                .is_some()
        );
        assert!(
            p.path_denied(std::path::Path::new("src/lib/producer.rs"))
                .is_none()
        );
        assert!(p.path_denied(std::path::Path::new("src/main.rs")).is_none());
        assert!(
            p.path_denied(std::path::Path::new("src\\production\\main.rs"))
                .is_some()
        );
    }

    #[test]
    fn deny_commands_match_case_insensitively() {
        let p = policy(&[], &["terraform destroy", "drop table"], None);
        assert!(
            p.command_denied("Terraform Destroy -auto-approve")
                .is_some()
        );
        assert!(p.command_denied("psql -c 'DROP TABLE users'").is_some());
        assert!(p.command_denied("cargo build").is_none());
    }

    #[test]
    fn deny_commands_see_through_quotes_escapes_and_whitespace() {
        let p = policy(&[], &["curl", "rm -rf"], None);
        assert!(p.command_denied("cu''rl -s https://evil.example").is_some());
        assert!(p.command_denied("cur\\l -s https://evil.example").is_some());
        assert!(p.command_denied("rm  -rf /data").is_some());
        assert!(
            p.command_denied("$'\\143url' -s https://evil.example")
                .is_some()
        );
        assert!(p.command_denied("cargo build").is_none());
    }

    #[test]
    fn diff_limit_reports_pair_when_exceeded() {
        let p = policy(&[], &[], Some(200));
        assert_eq!(p.diff_exceeds_limit(199), None);
        assert_eq!(p.diff_exceeds_limit(201), Some((201, 200)));
        assert!(policy(&[], &[], None).diff_exceeds_limit(999_999).is_none());
    }

    #[test]
    fn denial_message_names_tool_and_rule() {
        let msg = denial("search_replace", "deny_paths", "production");
        assert!(msg.starts_with("policy denied (search_replace)"));
        assert!(msg.contains("deny_paths"));
    }

    #[test]
    fn grok_home_credential_denied_auth_json() {
        let path = xai_grok_config::grok_home().join("auth.json");
        assert!(
            grok_home_credential_denied(&path),
            "writing $GROK_HOME/auth.json must be policy-denied"
        );
        let msg = grok_home_credential_denial("search_replace", &path);
        assert!(msg.contains("grok_home_credentials"), "{msg}");
        assert!(!grok_home_credential_denied(std::path::Path::new(
            "src/main.rs"
        )));
    }

    #[test]
    fn load_policy_toml_from_grok_dir() {
        let tmp = TempDir::new().unwrap();
        write_policy_toml(
            tmp.path(),
            r#"
max_diff_lines = 3
deny_paths = [".env", "secrets/"]
deny_tool_classes = ["execute"]
require_confirm_for = ["write"]
"#,
        );
        let p = PolicyParams::load_from_workspace(tmp.path());
        assert!(p.load_error.is_none(), "{:?}", p.load_error);
        assert_eq!(p.max_diff_lines, Some(3));
        assert_eq!(p.deny_paths, vec![".env", "secrets/"]);
        assert_eq!(p.deny_tool_classes, vec!["execute"]);
        assert_eq!(p.require_confirm_for, vec!["write"]);
    }

    #[test]
    fn load_policy_from_grok_toml_table() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("grok.toml"),
            r#"
[other]
ignored = true
[policy]
max_diff_lines = 8
deny_paths = ["production"]
"#,
        )
        .unwrap();
        let p = PolicyParams::load_from_workspace(tmp.path());
        assert!(p.load_error.is_none(), "{:?}", p.load_error);
        assert_eq!(p.max_diff_lines, Some(8));
        assert_eq!(p.deny_paths, vec!["production"]);
    }

    #[test]
    fn dedicated_policy_toml_wins_over_grok_toml() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("grok.toml"),
            "[policy]\ndeny_paths = [\"from-grok\"]\n",
        )
        .unwrap();
        write_policy_toml(tmp.path(), "deny_paths = [\"from-dedicated\"]\n");
        let p = PolicyParams::load_from_workspace(tmp.path());
        assert_eq!(p.deny_paths, vec!["from-dedicated"]);
    }

    #[test]
    fn malformed_policy_toml_fails_closed() {
        let tmp = TempDir::new().unwrap();
        write_policy_toml(tmp.path(), "max_diff_lines = \"nope\"\n");
        let p = PolicyParams::load_from_workspace(tmp.path());
        assert!(p.load_error.is_some(), "expected load_error");
        let err = p
            .enforce_dispatch(
                "write",
                Some(ToolKind::Write),
                &json!({"file_path": "ok.txt"}),
            )
            .expect_err("broken policy file must fail closed");
        let msg = err.to_string();
        assert!(msg.contains("policy_file"), "{msg}");
        assert_eq!(err.details.as_ref().unwrap()["code"], POLICY_DENIED);
    }

    #[test]
    fn enforce_dispatch_refuses_write_to_deny_path() {
        let p = policy(&[".env"], &[], None);
        let err = p
            .enforce_dispatch(
                "write",
                Some(ToolKind::Write),
                &json!({"file_path": ".env", "content": "stolen=1\n"}),
            )
            .expect_err("deny_path write must be refused");
        let msg = err.to_string();
        assert!(msg.contains("policy denied (write)"), "{msg}");
        assert!(msg.contains("deny_paths"), "{msg}");
        assert_eq!(err.details.as_ref().unwrap()["code"], POLICY_DENIED);
    }

    #[test]
    fn enforce_dispatch_refuses_when_max_diff_lines_exceeded() {
        let p = policy(&[], &[], Some(1));
        let err = p
            .enforce_dispatch(
                "search_replace",
                Some(ToolKind::Edit),
                &json!({
                    "file_path": "src/lib.rs",
                    "old_string": "before",
                    "new_string": "after\nsecond\nthird"
                }),
            )
            .expect_err("oversized edit must be refused");
        let msg = err.to_string();
        assert!(msg.contains("policy denied (search_replace)"), "{msg}");
        assert!(msg.contains("max_diff_lines"), "{msg}");
        assert_eq!(err.details.as_ref().unwrap()["code"], POLICY_DENIED);
    }

    #[test]
    fn enforce_dispatch_refuses_denied_tool_class() {
        let p = PolicyParams {
            deny_tool_classes: vec!["execute".into()],
            ..Default::default()
        };
        let err = p
            .enforce_dispatch(
                "run_terminal_cmd",
                Some(ToolKind::Execute),
                &json!({"command": "echo hi"}),
            )
            .expect_err("denied tool class must be refused");
        assert!(err.to_string().contains("deny_tool_classes"));
    }

    #[test]
    fn enforce_dispatch_refuses_without_confirm() {
        let p = PolicyParams {
            require_confirm_for: vec!["write".into()],
            ..Default::default()
        };
        p.enforce_dispatch(
            "write",
            Some(ToolKind::Write),
            &json!({"file_path": "ok.txt", "content": "x", "confirm": true}),
        )
        .expect("confirm:true must pass");
        let err = p
            .enforce_dispatch(
                "write",
                Some(ToolKind::Write),
                &json!({"file_path": "ok.txt", "content": "x"}),
            )
            .expect_err("missing confirm must fail closed");
        assert!(err.to_string().contains("require_confirm_for"));
    }

    #[derive(Debug)]
    struct PolicyWriteStub;

    impl ToolMetadata for PolicyWriteStub {
        fn kind(&self) -> ToolKind {
            ToolKind::Write
        }
        fn tool_namespace(&self) -> ToolNamespace {
            ToolNamespace::GrokBuild
        }
        fn description_template(&self) -> &str {
            "policy write stub"
        }
    }

    impl xai_tool_runtime::Tool for PolicyWriteStub {
        type Args = serde_json::Value;
        type Output = String;
        fn id(&self) -> xai_tool_protocol::ToolId {
            xai_tool_protocol::ToolId::new("policy_write_stub").expect("valid")
        }
        fn description(
            &self,
            _ctx: &xai_tool_runtime::ListToolsContext,
        ) -> xai_tool_types::ToolDescription {
            xai_tool_types::ToolDescription::new("policy_write_stub", "policy write stub")
        }
        async fn run(
            &self,
            _ctx: xai_tool_runtime::ToolCallContext,
            _input: serde_json::Value,
        ) -> Result<String, xai_tool_runtime::ToolError> {
            Ok("wrote".into())
        }
    }

    async fn dispatch_toolset(cwd: &Path, policy: PolicyParams) -> Arc<FinalizedToolset> {
        let toolset = Arc::new(FinalizedToolset::empty_for_test());
        toolset.update_resource(Cwd(cwd.to_path_buf())).await;
        toolset.update_resource(Params(policy)).await;
        toolset
            .register_tool(
                "write".to_string(),
                PolicyWriteStub,
                Some(json!({"type": "object"})),
            )
            .unwrap();
        toolset
    }

    #[tokio::test]
    async fn dispatch_refuses_write_to_deny_path() {
        let tmp = TempDir::new().unwrap();
        let toolset = dispatch_toolset(
            tmp.path(),
            PolicyParams {
                deny_paths: vec![".env".into()],
                ..Default::default()
            },
        )
        .await;
        let err = toolset
            .call(
                "write",
                json!({"file_path": ".env", "content": "stolen=1\n"}),
                "call-deny-path",
                None,
            )
            .await
            .expect_err("dispatcher must refuse a deny_path write");
        let msg = err.to_string();
        assert!(msg.contains("policy denied (write)"), "{msg}");
        assert!(msg.contains("deny_paths"), "{msg}");
        assert_eq!(err.details.as_ref().unwrap()["code"], POLICY_DENIED);
    }

    #[tokio::test]
    async fn dispatch_refuses_when_max_diff_lines_exceeded() {
        let tmp = TempDir::new().unwrap();
        let toolset = dispatch_toolset(
            tmp.path(),
            PolicyParams {
                max_diff_lines: Some(1),
                ..Default::default()
            },
        )
        .await;
        let err = toolset
            .call(
                "write",
                json!({"file_path": "src/lib.rs", "content": "one\ntwo\nthree\n"}),
                "call-max-diff",
                None,
            )
            .await
            .expect_err("dispatcher must refuse an oversized write");
        let msg = err.to_string();
        assert!(msg.contains("policy denied (write)"), "{msg}");
        assert!(msg.contains("max_diff_lines"), "{msg}");
        assert_eq!(err.details.as_ref().unwrap()["code"], POLICY_DENIED);
    }

    #[tokio::test]
    async fn dispatch_loads_policy_toml_from_workspace() {
        let tmp = TempDir::new().unwrap();
        write_policy_toml(tmp.path(), "deny_paths = [\".env\"]\n");
        let toolset = Arc::new(FinalizedToolset::empty_for_test());
        toolset.update_resource(Cwd(tmp.path().to_path_buf())).await;
        toolset
            .register_tool(
                "write".to_string(),
                PolicyWriteStub,
                Some(json!({"type": "object"})),
            )
            .unwrap();
        let err = toolset
            .call(
                "write",
                json!({"file_path": "config/.env.local", "content": "x\n"}),
                "call-file-policy",
                None,
            )
            .await
            .expect_err("workspace policy.toml must be enforced at dispatch");
        assert!(err.to_string().contains("deny_paths"), "{}", err);
    }
}
