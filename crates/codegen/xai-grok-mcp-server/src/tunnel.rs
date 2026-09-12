//! Tunnel helper supervision.
//!
//! A remote client reaches a loopback server only through a public HTTPS
//! endpoint. This module starts and owns that helper.
//!
//! - The child is enrolled in `xai_tty_utils::global_process_scope()` (raw
//!   spawning is clippy-banned), so the scope's `kill_all` backstop reaps it.
//! - On Linux it is also bound to this process's death with
//!   `PR_SET_PDEATHSIG`, because an abort, `SIGKILL` or panic skips every
//!   destructor and would otherwise leave a live public route behind.
//! - A missing or slow helper is a startup error. There is no fallback to a
//!   different transport.
//! - The operator's own cloudflared setup does not shape this tunnel. The helper
//!   reads an empty configuration file of its own instead of a `config.yml` from
//!   the home directory or `/etc/cloudflared`, and starts without any `TUNNEL_*`
//!   variable, in any letter case.
//!
//! `cloudflared` is given only the loopback **origin** plus
//! `--http-host-header 127.0.0.1:<port>`. The secret path segment is appended to
//! the public base afterwards, so it never appears in the helper's argv.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use xai_tty_utils::{ProcessGroup, global_process_scope};

/// How long the helper gets to report a public URL before startup fails.
pub const READY_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelKind {
    /// No helper. The operator fronts the loopback URL themselves.
    None,
    /// A Cloudflare quick tunnel (`cloudflared tunnel --url ...`).
    Cloudflare,
}

impl TunnelKind {
    fn binary_name(self) -> Option<&'static str> {
        match self {
            TunnelKind::None => None,
            TunnelKind::Cloudflare => Some("cloudflared"),
        }
    }
}

/// A running tunnel. Dropping it tears the helper down.
pub struct RunningTunnel {
    /// Public base URL with the server's secret path appended. Credential
    /// bearing: never log it.
    pub public_url: String,
    child: Child,
    group: Arc<ProcessGroup>,
    /// The helper's empty configuration file, removed with the tunnel.
    _config: tempfile::TempPath,
}

impl Drop for RunningTunnel {
    fn drop(&mut self) {
        // `ProcessGroup`'s own drop is a no-op on Unix and `kill_on_drop` reaps
        // only the parent, so kill the group explicitly.
        if let Err(e) = self.group.kill() {
            tracing::debug!(error = %e, "tunnel process group already reaped");
        }
        let _ = self.child.start_kill();
    }
}

impl std::fmt::Debug for RunningTunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The URL carries the secret path segment; never render it.
        f.debug_struct("RunningTunnel")
            .field("public_url", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Locate the helper binary. An explicit path wins and must exist; otherwise
/// the absolute directories on `PATH` are searched, never the current
/// directory.
pub fn resolve_bin(kind: TunnelKind, explicit: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    let Some(name) = kind.binary_name() else {
        return Ok(None);
    };
    if let Some(p) = explicit {
        // The file that is checked must be the file that runs: `Command::new`
        // resolves a bare name through PATH, not against the current directory.
        let p = std::path::absolute(p)
            .map_err(|e| anyhow::anyhow!("cannot resolve --tunnel-bin {}: {e}", p.display()))?;
        anyhow::ensure!(p.is_file(), "{name} not found at {}", p.display());
        return Ok(Some(p));
    }
    find_on_path(name).map(Some).ok_or_else(|| {
        anyhow::anyhow!("{name} not found on PATH; install it or pass --tunnel-bin <PATH>")
    })
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    search_dirs(&path)
        .into_iter()
        .map(|dir| dir.join(&exe))
        .find(|candidate| candidate.is_file())
}

/// The directories of a `PATH` value that are searched. Empty and relative
/// entries are skipped: they would resolve against the current directory.
fn search_dirs(path: &std::ffi::OsStr) -> Vec<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .collect()
}

/// Extract a quick-tunnel URL from one `cloudflared` log line. Every `https://`
/// occurrence is examined, because the provisioning endpoint
/// `api.trycloudflare.com` appears in the same logs and is not a tunnel.
pub fn parse_trycloudflare_url(line: &str) -> Option<String> {
    for (start, _) in line.match_indices("https://") {
        let rest = &line[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '|' || c == '"' || c == '\'')
            .unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches('\r').trim_end_matches('/');
        let Some(host) = url.strip_prefix("https://") else {
            continue;
        };
        let Some(sub) = host.strip_suffix(".trycloudflare.com") else {
            continue;
        };
        let valid = !sub.is_empty()
            && sub != "api"
            && sub.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if valid {
            return Some(url.to_string());
        }
    }
    None
}

/// Split the server's loopback URL into `(origin, host, path)`.
fn split_local_url(url: &str) -> anyhow::Result<(String, String, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow::anyhow!("local MCP URL must be http://127.0.0.1:<port>/..."))?;
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (rest, String::new()),
    };
    anyhow::ensure!(
        host.starts_with("127.0.0.1:"),
        "tunnel origin must be loopback"
    );
    Ok((format!("http://{host}"), host.to_string(), path))
}

/// An empty cloudflared configuration, so no configuration file of the
/// operator's applies to the quick tunnel.
fn empty_config_file() -> std::io::Result<tempfile::TempPath> {
    let mut file = tempfile::Builder::new()
        .prefix("turbo-cloudflared-")
        .suffix(".yml")
        .tempfile()?;
    std::io::Write::write_all(&mut file, b"{}\n")?;
    Ok(file.into_temp_path())
}

/// The quick-tunnel command for `origin`, reading only `config`.
fn cloudflared_command(
    bin: &Path,
    origin: &str,
    host: &str,
    config: &Path,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("tunnel")
        .arg("--config")
        .arg(config)
        .args([
            "--no-autoupdate",
            "--url",
            origin,
            "--http-host-header",
            host,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // cloudflared reads any flag from a `TUNNEL_*` variable. Inheriting one (a
    // token, hostname or origin setting meant for another tunnel) could change
    // what this quick tunnel publishes.
    for (key, _) in std::env::vars_os() {
        if is_tunnel_variable(&key) {
            cmd.env_remove(&key);
        }
    }
    cmd
}

/// Whether `name` is a variable cloudflared reads as a flag. Windows matches
/// variable names in any letter case.
fn is_tunnel_variable(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy()
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("TUNNEL_"))
}

/// Spawn an enrolled helper with this crate's platform flags.
fn spawn_enrolled(
    mut std_cmd: std::process::Command,
) -> anyhow::Result<(Child, Arc<ProcessGroup>)> {
    // Linux: the kernel signals the helper when this process dies, even when no
    // destructor runs. No-op on other platforms.
    xai_tty_utils::kill_on_parent_death_std(&mut std_cmd);

    let mut cmd = Command::from(std_cmd);
    cmd.kill_on_drop(true);

    let scope = global_process_scope();
    scope.prepare(&mut cmd);

    #[cfg(windows)]
    {
        // `prepare` set CREATE_NEW_PROCESS_GROUP, and `creation_flags` is a SET,
        // not an OR, so re-apply it together with CREATE_NO_WINDOW. Same pattern
        // as `computer/local/terminal.rs`.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    #[allow(clippy::disallowed_methods)] // enrolled into the process scope immediately below
    let mut child = cmd.spawn()?;
    match scope.enroll(&child) {
        Ok(group) => Ok((child, group)),
        Err(e) => {
            let _ = child.start_kill();
            Err(anyhow::anyhow!("could not enroll the tunnel helper: {e}"))
        }
    }
}

/// Start a Cloudflare quick tunnel in front of `local_url`, the server's full
/// loopback URL including its secret path.
pub async fn start_cloudflared(bin: &Path, local_url: &str) -> anyhow::Result<RunningTunnel> {
    let (origin, host, path) = split_local_url(local_url)?;
    let config = empty_config_file()
        .map_err(|e| anyhow::anyhow!("could not create the tunnel's configuration file: {e}"))?;

    let (mut child, group) = spawn_enrolled(cloudflared_command(bin, &origin, &host, &config))
        .map_err(|e| anyhow::anyhow!("could not start {}: {e}", bin.display()))?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("cloudflared stderr was not piped"))?;
    let mut lines = BufReader::new(stderr).lines();

    let found = tokio::time::timeout(READY_TIMEOUT, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(url) = parse_trycloudflare_url(&line) {
                return Some(url);
            }
        }
        None
    })
    .await;

    let base = match found {
        Ok(Some(url)) => url,
        Ok(None) => {
            let _ = group.kill();
            let _ = child.kill().await;
            anyhow::bail!("cloudflared exited before reporting a tunnel URL");
        }
        Err(_) => {
            let _ = group.kill();
            let _ = child.kill().await;
            anyhow::bail!(
                "cloudflared did not report a tunnel URL within {}s",
                READY_TIMEOUT.as_secs()
            );
        }
    };

    // Keep draining stderr so a full pipe cannot stall cloudflared's logging.
    tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "mcp_tunnel", "{line}");
        }
    });

    Ok(RunningTunnel {
        public_url: format!("{base}{path}"),
        child,
        group,
        _config: config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_search_skips_empty_and_relative_entries() {
        let absolute = tempfile::tempdir().unwrap();
        let joined = std::env::join_paths([
            PathBuf::new(),
            PathBuf::from("relative").join("bin"),
            absolute.path().to_path_buf(),
        ])
        .unwrap();
        assert_eq!(search_dirs(&joined), vec![absolute.path().to_path_buf()]);
    }

    #[test]
    fn an_explicit_binary_is_checked_where_it_will_run_and_never_falls_back() {
        let err = resolve_bin(
            TunnelKind::Cloudflare,
            Some(Path::new("no-such-dir/cloudflared")),
        )
        .unwrap_err()
        .to_string();
        // The PATH-search error also says "not found"; only the explicit-path
        // branch says "not found at", with the absolute path it checked.
        assert!(err.contains("not found at"), "{err}");
        let expected = std::path::absolute("no-such-dir/cloudflared").unwrap();
        assert!(err.contains(&expected.display().to_string()), "{err}");
    }

    #[test]
    fn tunnel_variables_are_recognised_in_any_letter_case() {
        for name in ["TUNNEL_TOKEN", "tunnel_loglevel", "Tunnel_Edge"] {
            assert!(is_tunnel_variable(std::ffi::OsStr::new(name)), "{name}");
        }
        for name in ["TUNNEL", "TUNNELX", "MY_TUNNEL_TOKEN", "PATH", ""] {
            assert!(!is_tunnel_variable(std::ffi::OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn the_tunnel_reads_only_its_own_empty_configuration() {
        let config = empty_config_file().expect("configuration file");
        assert_eq!(std::fs::read_to_string(&config).unwrap().trim(), "{}");
        let cmd = cloudflared_command(
            Path::new("cloudflared"),
            "http://127.0.0.1:1",
            "127.0.0.1:1",
            &config,
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args
            .iter()
            .position(|a| a == "--config")
            .expect("--config is passed");
        assert_eq!(args[at + 1], config.to_string_lossy(), "{args:?}");
        let path = config.to_path_buf();
        drop(config);
        assert!(!path.exists(), "the configuration file outlived its tunnel");
    }

    #[test]
    fn parses_the_boxed_url_cloudflared_prints() {
        let line = "2026-09-10T10:00:00Z INF |  https://brave-lion-fox.trycloudflare.com                                 |";
        assert_eq!(
            parse_trycloudflare_url(line).as_deref(),
            Some("https://brave-lion-fox.trycloudflare.com")
        );
    }

    #[test]
    fn ignores_the_provisioning_endpoint() {
        let line = "ERR failed to request quick Tunnel: Post \"https://api.trycloudflare.com/tunnel\": dial tcp";
        assert_eq!(parse_trycloudflare_url(line), None);
    }

    #[test]
    fn finds_the_tunnel_even_after_the_provisioning_endpoint() {
        let line =
            "INF via https://api.trycloudflare.com got https://calm-owl.trycloudflare.com ok";
        assert_eq!(
            parse_trycloudflare_url(line).as_deref(),
            Some("https://calm-owl.trycloudflare.com")
        );
    }

    #[test]
    fn rejects_lookalike_hosts() {
        for line in [
            "https://evil.com/.trycloudflare.com",
            "https://evil.trycloudflare.com.attacker.net",
            "https://.trycloudflare.com",
            "http://plain-http.trycloudflare.com",
            "no url here",
        ] {
            assert_eq!(parse_trycloudflare_url(line), None, "{line}");
        }
    }

    #[test]
    fn split_keeps_the_secret_path_out_of_the_origin() {
        let (origin, host, path) = split_local_url("http://127.0.0.1:8765/abc123/mcp").unwrap();
        assert_eq!(origin, "http://127.0.0.1:8765");
        assert_eq!(host, "127.0.0.1:8765");
        assert_eq!(path, "/abc123/mcp");
    }

    #[test]
    fn split_refuses_a_non_loopback_origin() {
        assert!(split_local_url("http://0.0.0.0:8765/x/mcp").is_err());
        assert!(split_local_url("http://192.168.1.5:8765/x/mcp").is_err());
        assert!(split_local_url("https://127.0.0.1:8765/x/mcp").is_err());
    }

    #[test]
    fn no_tunnel_needs_no_binary() {
        assert!(resolve_bin(TunnelKind::None, None).unwrap().is_none());
    }

    #[test]
    fn missing_explicit_binary_is_an_error_not_a_fallback() {
        let err = resolve_bin(
            TunnelKind::Cloudflare,
            Some(Path::new("definitely/not/a/real/cloudflared")),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    #[tokio::test]
    async fn debug_never_renders_the_public_url() {
        // A real RunningTunnel around a harmless child, so the assertion
        // exercises the actual Debug impl rather than a string literal.
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.arg("--list")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (child, group) = spawn_enrolled(cmd).expect("spawn a harmless child");
        let tunnel = RunningTunnel {
            public_url: "https://sentinel-host.trycloudflare.com/SECRET-SEGMENT-9f2c/mcp".into(),
            child,
            group,
            _config: empty_config_file().unwrap(),
        };
        let rendered = format!("{tunnel:?}");
        assert!(!rendered.contains("SECRET-SEGMENT-9f2c"), "{rendered}");
        assert!(!rendered.contains("sentinel-host"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    /// End to end through a real Cloudflare quick tunnel.
    #[tokio::test]
    #[ignore = "network: starts a real Cloudflare quick tunnel; run manually"]
    async fn live_cloudflared_quick_tunnel_reaches_the_server() {
        let root = tempfile::tempdir().unwrap();
        let ts = crate::toolset::ServedToolset::new(vec![root.path().to_path_buf()], true)
            .await
            .unwrap();
        let (handle, _join) = crate::http::serve(Arc::new(ts), None).await.unwrap();
        let bin = resolve_bin(TunnelKind::Cloudflare, None)
            .unwrap()
            .expect("cloudflared on PATH");
        let tunnel = start_cloudflared(&bin, &handle.url)
            .await
            .expect("tunnel starts");
        assert!(tunnel.public_url.starts_with("https://"));
        assert!(tunnel.public_url.ends_with("/mcp"));

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});

        let mut last = String::new();
        for _ in 0..20 {
            match client
                .post(&tunnel.public_url)
                .header("authorization", format!("Bearer {}", handle.token))
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .body(body.to_string())
                .send()
                .await
            {
                Ok(res) if res.status().is_success() => {
                    let text = res.text().await.unwrap();
                    assert!(text.contains("read_file"), "{text}");
                    let unauth = client
                        .post(&tunnel.public_url)
                        .header("content-type", "application/json")
                        .body(body.to_string())
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(unauth.status(), reqwest::StatusCode::UNAUTHORIZED);
                    handle.shutdown();
                    return;
                }
                Ok(res) => last = format!("status {}", res.status()),
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        panic!("tunnel never became reachable: {last}");
    }
}
