//! `turbo mcp serve` — expose a bounded subset of Turbo's file tools to an
//! external MCP client over loopback.
//!
//! The containment boundary lives in `xai-grok-mcp-server`. This module turns
//! the operator's intent into a server: it resolves roots, refuses to widen an
//! inherited `--confine`, optionally starts a tunnel, prints the endpoint, and
//! tears everything down on every way the command can end.

use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use xai_grok_mcp_server::http::{ServeHandle, serve, shutdown_gracefully};
use xai_grok_mcp_server::log_preview;
use xai_grok_mcp_server::toolset::{EventObserver, ServeEvent, ServedToolset};
use xai_grok_mcp_server::tunnel::{self, RunningTunnel, TunnelKind};

/// What the external client may do inside the approved roots.
///
/// There is deliberately no shell tier: a shell command's operands cannot be
/// checked against the roots, and Turbo's command tool does not confine itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum AllowTier {
    /// Read, list, and search files inside the approved roots
    #[default]
    Readonly,
    /// Also edit files inside the roots; only for a client you trust to change your code
    Edit,
}

impl AllowTier {
    fn read_only(self) -> bool {
        matches!(self, AllowTier::Readonly)
    }

    fn label(self) -> &'static str {
        match self {
            AllowTier::Readonly => "readonly",
            AllowTier::Edit => "edit",
        }
    }
}

/// Which public tunnel, if any, fronts the loopback server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum TunnelArg {
    /// No tunnel. Front the loopback URL yourself; your proxy must rewrite Host to 127.0.0.1:<port>.
    #[default]
    None,
    /// A Cloudflare quick tunnel via `cloudflared`
    Cloudflare,
}

impl TunnelArg {
    fn kind(self) -> TunnelKind {
        match self {
            TunnelArg::None => TunnelKind::None,
            TunnelArg::Cloudflare => TunnelKind::Cloudflare,
        }
    }
}

const SERVE_AFTER_HELP: &str = "\
Examples:
  # Serve read-only file tools for one project
  turbo mcp serve --root /path/to/project

  # Allow edits inside two roots, on a fixed port
  turbo mcp serve --root ./app --root ./lib --allow edit --port 8765

  # Put a Cloudflare quick tunnel in front of it
  turbo mcp serve --root /path/to/project --tunnel cloudflare

The server binds 127.0.0.1 only. Every request must carry the printed
`Authorization: Bearer` token. Paths sent by the client must be absolute and
inside an approved root; anything else is refused, and the reason is printed to
stderr. A root may not be a filesystem root, a home directory, or a directory
that contains one.

`--allow edit` is for clients you trust to change your code: edited source and
build files run the next time you build or test. A Cloudflare tunnel ends TLS
at Cloudflare, which can read the token and every file the client reads.";

#[derive(Debug, clap::Args, Clone)]
#[command(after_help = SERVE_AFTER_HELP)]
pub struct McpServeArgs {
    /// Approved root directory. Repeatable. Required: the server never adopts
    /// the current directory implicitly.
    #[arg(long = "root", value_name = "PATH", required = true)]
    pub roots: Vec<PathBuf>,

    /// What the client may do inside the roots
    #[arg(long, value_enum, default_value = "readonly")]
    pub allow: AllowTier,

    /// Loopback port to bind. Defaults to an ephemeral port.
    #[arg(long)]
    pub port: Option<u16>,

    /// Public tunnel to start in front of the loopback server
    #[arg(long, value_enum, default_value = "none")]
    pub tunnel: TunnelArg,

    /// Path to the tunnel helper binary. Defaults to searching PATH.
    #[arg(long, value_name = "PATH")]
    pub tunnel_bin: Option<PathBuf>,

    /// Print the endpoint as a single JSON object instead of text
    #[arg(long)]
    pub json: bool,
}

/// A started server plus what is needed to stop it.
pub struct StartedServer {
    pub handle: ServeHandle,
    pub toolset: Arc<ServedToolset>,
    pub join: tokio::task::JoinHandle<()>,
    pub tunnel: Option<RunningTunnel>,
    pub tools: Vec<String>,
    /// The roots as printed for the operator, in spellings the server accepts.
    pub roots: Vec<PathBuf>,
}

/// Make each root absolute against the operator's current directory. These are
/// the operator's own startup arguments; client paths are held to a stricter
/// rule inside the guard.
fn absolutize_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    roots
        .iter()
        .map(|r| {
            std::path::absolute(r).with_context(|| format!("cannot resolve --root {}", r.display()))
        })
        .collect()
}

/// Refuse any root outside an inherited `--confine`.
fn ensure_within(roots: &[PathBuf], confine: &[PathBuf]) -> Result<()> {
    if confine.is_empty() {
        return Ok(());
    }
    for r in roots {
        if !xai_grok_tools::types::resources::path_is_under_any_root(r, confine) {
            bail!(
                "--root {} is outside the inherited --confine root; refusing to widen the boundary",
                r.display()
            );
        }
    }
    Ok(())
}

/// Operator lines waiting for standard error. When the queue is full, new lines
/// are dropped and counted instead of waited for.
const NOTE_QUEUE: usize = 256;

enum Note {
    Line(String),
    Flush(std::sync::mpsc::Sender<()>),
}

/// Operator lines for standard error, written by one dedicated thread. No
/// request path or stop path writes to the stream itself: a pipe nobody reads
/// blocks its writer, and every other thread would then wait on the stream's
/// lock, including the runtime workers that deliver stop signals.
struct Notes {
    queue: SyncSender<Note>,
    dropped: AtomicU64,
}

impl Notes {
    fn start(mut write: impl FnMut(&str) + Send + 'static) -> Self {
        let (queue, pending) = sync_channel(NOTE_QUEUE);
        let spawned = std::thread::Builder::new()
            .name("mcp-serve-notes".into())
            .spawn(move || {
                for note in pending {
                    match note {
                        Note::Line(line) => write(&line),
                        Note::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not start the operator-notes writer");
        }
        Self {
            queue,
            dropped: AtomicU64::new(0),
        }
    }

    /// Queue one line. Never blocks.
    fn push(&self, line: String) {
        // Taken, not read: two threads must never both report the same drops.
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        let line = if dropped == 0 {
            line
        } else {
            format!("{line} ({dropped} earlier line(s) not shown)")
        };
        if self.queue.try_send(Note::Line(line)).is_err() {
            self.dropped.fetch_add(dropped + 1, Ordering::Relaxed);
        }
    }

    /// Wait up to `within` for the lines queued so far to be written, after a
    /// last line saying how many were dropped, if any were.
    fn flush(&self, within: Duration) {
        let deadline = Instant::now() + within;
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            let notice = Note::Line(format!("{dropped} operator line(s) were not shown"));
            if self.send_by(notice, deadline).is_err() {
                self.dropped.fetch_add(dropped, Ordering::Relaxed);
            }
        }
        let (done, written) = std::sync::mpsc::channel();
        if self.send_by(Note::Flush(done), deadline).is_ok() {
            let _ = written.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
    }

    /// Queue `note`, retrying while the queue is full, until `deadline`.
    fn send_by(&self, mut note: Note, deadline: Instant) -> Result<(), Note> {
        loop {
            match self.queue.try_send(note) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(back)) if Instant::now() < deadline => {
                    note = back;
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(TrySendError::Full(back) | TrySendError::Disconnected(back)) => {
                    return Err(back);
                }
            }
        }
    }
}

fn notes() -> &'static Notes {
    static NOTES: OnceLock<Notes> = OnceLock::new();
    NOTES.get_or_init(|| {
        Notes::start(|line| {
            // Failure is ignored: `eprintln!` would panic on a closed stream, and
            // a panic aborts a release build.
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        })
    })
}

/// Queue one line for standard error. Never blocks.
fn note(line: impl std::fmt::Display) {
    notes().push(line.to_string());
}

/// Tracing output for `turbo mcp serve`. Each event becomes one operator line on
/// the notes queue, so a log line from a request, or from teardown, never waits
/// on a standard error that nobody reads.
#[derive(Clone, Copy)]
pub struct OperatorStderr {
    notes: &'static Notes,
}

/// The writer `turbo mcp serve` gives its tracing subscriber.
pub fn tracing_writer() -> OperatorStderr {
    OperatorStderr { notes: notes() }
}

/// One tracing event on its way to the notes queue, queued when dropped.
pub struct OperatorLine {
    notes: &'static Notes,
    bytes: Vec<u8>,
}

impl std::io::Write for OperatorLine {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for OperatorLine {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.bytes);
        let line = text.trim_end_matches(['\r', '\n']);
        if !line.is_empty() {
            self.notes.push(line.to_string());
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for OperatorStderr {
    type Writer = OperatorLine;

    fn make_writer(&'a self) -> Self::Writer {
        OperatorLine {
            notes: self.notes,
            bytes: Vec::new(),
        }
    }
}

/// Lets one line through per interval and counts the lines it holds back.
struct Throttle {
    interval: Duration,
    state: Mutex<ThrottleState>,
}

#[derive(Default)]
struct ThrottleState {
    last: Option<Instant>,
    held_back: u64,
}

impl Throttle {
    const fn new(interval: Duration) -> Self {
        Self {
            interval,
            state: Mutex::new(ThrottleState {
                last: None,
                held_back: 0,
            }),
        }
    }

    /// `Some(held_back)` if a line may be printed at `now`, with the number of
    /// lines held back since the last one printed; `None` otherwise.
    fn admit(&self, now: Instant) -> Option<u64> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < self.interval)
        {
            state.held_back += 1;
            return None;
        }
        state.last = Some(now);
        Some(std::mem::take(&mut state.held_back))
    }

    /// The lines held back since the last one printed, once `interval` has
    /// passed since it, so a burst is reported when it is over; they count as
    /// reported from then.
    fn take_overdue(&self, now: Instant) -> Option<u64> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let overdue = state.held_back > 0
            && state
                .last
                .is_none_or(|last| now.saturating_duration_since(last) >= self.interval);
        if !overdue {
            return None;
        }
        state.last = Some(now);
        Some(std::mem::take(&mut state.held_back))
    }

    /// Every line held back that no line has reported yet.
    fn take_held_back(&self) -> u64 {
        std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .held_back,
        )
    }
}

/// Requests rejected for their bearer token. Anyone who can reach the port can
/// send them, so they are reported at most once a second.
static REJECTED: Throttle = Throttle::new(Duration::from_secs(1));

/// The operator line for `count` rejected requests no earlier line reported.
fn held_back_rejections_line(count: u64) -> String {
    format!("rejected: {count} more requests without a valid bearer token since the last report")
}

/// The operator line for `event`, if one should be printed at `now`.
///
/// Rejected credentials come from anyone who can reach the port, so they are
/// throttled. Refusals come only from a client holding the token, and each one
/// is part of the operator's record of what that client tried.
fn event_line(event: ServeEvent<'_>, unauthorized: &Throttle, now: Instant) -> Option<String> {
    match event {
        // The tool name is the client's text: escaped and shortened, so it
        // cannot forge lines in the operator's terminal or flood it.
        ServeEvent::Refused { tool, reason } => Some(format!(
            "refused: tool={} reason={reason:?}",
            log_preview(tool)
        )),
        ServeEvent::Unauthorized => unauthorized.admit(now).map(|held_back| {
            let line = "rejected: request without a valid bearer token";
            match held_back {
                0 => line.to_string(),
                n => format!("{line} ({n} more since the last report)"),
            }
        }),
    }
}

fn operator_observer() -> EventObserver {
    Arc::new(|event: ServeEvent<'_>| {
        if let Some(line) = event_line(event, &REJECTED, Instant::now()) {
            note(line);
        }
    })
}

/// Start the server under an explicit confine list.
async fn start_with_confine(args: &McpServeArgs, confine: &[PathBuf]) -> Result<StartedServer> {
    let roots = absolutize_roots(&args.roots)?;
    ensure_within(&roots, confine)?;

    // Resolve the helper before binding anything, so a missing binary fails
    // fast rather than after a server is already listening.
    let tunnel_bin = tunnel::resolve_bin(args.tunnel.kind(), args.tunnel_bin.as_deref())?;

    let mut toolset = ServedToolset::new(roots, args.allow.read_only())
        .await
        .context("could not build the served toolset")?;
    // Re-check against the roots the server actually adopted. The first check
    // saw the operator's spelling; a symlink in it could have changed since.
    ensure_within(&toolset.roots(), confine)?;
    toolset.set_observer(operator_observer());

    let roots = toolset.printable_roots();
    let tools = toolset.list().iter().map(|t| t.name.clone()).collect();
    let toolset = Arc::new(toolset);

    let (handle, join) = serve(toolset.clone(), args.port)
        .await
        .context("could not bind the MCP server")?;

    // Dropping `handle` stops the server, on this error path and if this
    // future is dropped while the tunnel is still starting.
    let tunnel = match (args.tunnel.kind(), tunnel_bin) {
        (TunnelKind::Cloudflare, Some(bin)) => Some(
            tunnel::start_cloudflared(&bin, &handle.url)
                .await
                .context("could not start the tunnel")?,
        ),
        _ => None,
    };

    // A token is bound to the URL the client actually used. The tunnel rewrites
    // `Host` to loopback, so the server cannot learn its public URL from a
    // request: it is told here, once, as soon as the tunnel reports one.
    if let Some(running) = &tunnel {
        handle.oauth.set_resource(running.public_url.clone());
    }

    Ok(StartedServer {
        handle,
        toolset,
        join,
        tunnel,
        tools,
        roots,
    })
}

/// Start the server under this process's inherited confine, if any.
pub async fn start(args: &McpServeArgs) -> Result<StartedServer> {
    start_with_confine(
        args,
        xai_grok_tools::types::resources::process_confine_roots(),
    )
    .await
}

/// What the operator must know about the configuration they started.
fn cautions(args: &McpServeArgs, tunneled: bool) -> Vec<&'static str> {
    let mut lines = Vec::new();
    if args.allow == AllowTier::Edit {
        lines.push(
            "edit: the client can change code that runs when you build or test; serve it only \
             to a client you trust to change your code.",
        );
    }
    if tunneled {
        lines.push(
            "tunnel: Cloudflare ends TLS for the public address and can read the token and \
             every file the client reads.",
        );
    }
    lines
}

/// What announcing the endpoint writes: `stdout` for standard output, `stderr`
/// for standard error.
struct Endpoint {
    stdout: String,
    stderr: Vec<&'static str>,
}

fn endpoint(args: &McpServeArgs, s: &StartedServer) -> Endpoint {
    use std::fmt::Write as _;

    let public_url = s.tunnel.as_ref().map(|t| t.public_url.clone());
    let cautions = cautions(args, public_url.is_some());
    let mut out = String::new();
    if args.json {
        let payload = serde_json::json!({
            "url": s.handle.url,
            "public_url": public_url,
            "bearer_token": s.handle.token,
            "oauth_metadata_url": s.handle.metadata_url(),
            "oauth_approval_code": s.handle.oauth.consent_code(),
            "allow": args.allow.label(),
            "roots": s.roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>(),
            "tools": s.tools,
        });
        // Standard output stays a single JSON object; the cautions go to stderr.
        let _ = writeln!(out, "{payload}");
        return Endpoint {
            stdout: out,
            stderr: cautions,
        };
    }
    let _ = writeln!(out, "Turbo MCP server listening (loopback only)");
    let _ = writeln!(out, "  URL:     {}", s.handle.url);
    if let Some(url) = &public_url {
        let _ = writeln!(out, "  Public:  {url}");
    }
    let _ = writeln!(out, "  Header:  Authorization: Bearer {}", s.handle.token);
    let _ = writeln!(out, "  OAuth:   {}", s.handle.metadata_url());
    let _ = writeln!(
        out,
        "  Approve: {}   (a client using OAuth asks for this)",
        s.handle.oauth.consent_code()
    );
    let _ = writeln!(out, "  Allow:   {}", args.allow.label());
    for r in &s.roots {
        let _ = writeln!(out, "  Root:    {}", r.display());
    }
    let _ = writeln!(out, "  Tools:   {}", s.tools.join(", "));
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "The URL and token together are a credential: anyone holding both can"
    );
    let _ = writeln!(out, "use these tools inside the roots above.");
    let _ = writeln!(
        out,
        "A client that cannot send that header can connect with OAuth instead: it"
    );
    let _ = writeln!(
        out,
        "opens a page that asks for the approval code above, so that code is a"
    );
    let _ = writeln!(
        out,
        "credential too. Its five minutes start when that page opens."
    );
    for line in &cautions {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out, "Press Ctrl-C to stop.");
    Endpoint {
        stdout: out,
        stderr: Vec::new(),
    }
}

/// Announce the endpoint. Fails if standard output cannot take it: a server
/// whose endpoint nobody saw is of no use to the operator.
fn print_endpoint(args: &McpServeArgs, s: &StartedServer) -> Result<()> {
    let Endpoint { stdout, stderr } = endpoint(args, s);
    let mut out = std::io::stdout().lock();
    out.write_all(stdout.as_bytes())
        .and_then(|()| out.flush())
        .context("could not print the endpoint")?;
    for line in stderr {
        note(line);
    }
    Ok(())
}

/// The stop event after which Windows ends the process within seconds, with or
/// without the handler finishing. Logoff and shutdown are not stop events here:
/// Windows sends them only to console processes that have not loaded user32,
/// and Turbo loads it.
const CONSOLE_CLOSED: &str = "console close";

fn ends_the_process_soon(signal: &str) -> bool {
    signal == CONSOLE_CLOSED
}

/// Something that resolves when the server should stop.
trait StopSource {
    async fn recv(&mut self) -> &'static str;
}

/// Every signal that should stop the server.
struct StopSignals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    hangup: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    ctrl_close: tokio::signal::windows::CtrlClose,
}

impl StopSignals {
    fn register() -> Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt()).context("register SIGINT")?,
                terminate: signal(SignalKind::terminate()).context("register SIGTERM")?,
                hangup: signal(SignalKind::hangup()).context("register SIGHUP")?,
            })
        }
        #[cfg(windows)]
        {
            use tokio::signal::windows;
            Ok(Self {
                ctrl_c: windows::ctrl_c().context("register Ctrl-C")?,
                ctrl_break: windows::ctrl_break().context("register Ctrl-Break")?,
                // Unhandled, it ends the process with no teardown at all.
                ctrl_close: windows::ctrl_close().context("register console close")?,
            })
        }
    }
}

impl StopSource for StopSignals {
    async fn recv(&mut self) -> &'static str {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.interrupt.recv() => "SIGINT",
                _ = self.terminate.recv() => "SIGTERM",
                _ = self.hangup.recv() => "SIGHUP",
            }
        }
        #[cfg(windows)]
        {
            tokio::select! {
                _ = self.ctrl_c.recv() => "Ctrl-C",
                _ = self.ctrl_break.recv() => "Ctrl-Break",
                _ = self.ctrl_close.recv() => CONSOLE_CLOSED,
            }
        }
    }
}

fn install_reaping_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The release profile aborts on panic, which skips destructors. Reap the
        // tunnel helper first so a crash cannot leave a public route behind, then
        // remove the session folders.
        xai_tty_utils::global_process_scope().kill_all();
        xai_grok_mcp_server::remove_session_dirs_for_abort();
        previous(info);
    }));
}

async fn teardown(server: StartedServer) {
    let StartedServer {
        handle,
        toolset,
        join,
        tunnel,
        ..
    } = server;
    // Stopping ends the public exposure at once. Local calls still drain, so an
    // edit in progress is not cut off part-way.
    drop(tunnel);
    shutdown_gracefully(&handle, &toolset, join).await;
}

/// Teardown for an event after which Windows ends the process a few seconds
/// later, too soon for the drain: stop the tunnel and remove the session
/// folder first, then stop as usual for as long as the process lasts.
async fn teardown_before_exit(server: StartedServer) {
    let StartedServer {
        handle,
        toolset,
        join,
        tunnel,
        ..
    } = server;
    drop(tunnel);
    toolset.begin_shutdown();
    if let Err(e) = std::fs::remove_dir_all(toolset.session_dir()) {
        tracing::debug!(error = %e, "session folder was not removed before exit");
    }
    shutdown_gracefully(&handle, &toolset, join).await;
}

/// Run `start`, then serve until `stop` fires. A stop that arrives while `start`
/// is still running ends the command without waiting for startup to finish. A
/// stop that arrives while the server is stopping ends it at once, unless it is
/// the first signal repeated or a hangup; a console close removes the session
/// folder first and stopping goes on.
async fn serve_until_stopped<S, F>(
    stop: &mut S,
    start: F,
    on_started: impl FnOnce(&StartedServer) -> Result<()>,
) -> Result<()>
where
    S: StopSource,
    F: Future<Output = Result<StartedServer>>,
{
    let server = tokio::select! {
        started = start => started?,
        signal = stop.recv() => {
            note(format_args!("Received {signal} during startup; stopping."));
            return Ok(());
        }
    };
    if let Err(error) = on_started(&server) {
        teardown(server).await;
        return Err(error);
    }

    let signal = loop {
        tokio::select! {
            signal = stop.recv() => break signal,
            () = tokio::time::sleep(Duration::from_secs(1)) => {
                // A burst of rejected requests is reported once it is over.
                if let Some(count) = REJECTED.take_overdue(Instant::now()) {
                    note(held_back_rejections_line(count));
                }
            }
        }
    };
    note(format_args!("Received {signal}; stopping MCP server..."));
    let session_dir = server.toolset.session_dir().to_path_buf();
    let stopping = async move {
        if ends_the_process_soon(signal) {
            teardown_before_exit(server).await;
        } else {
            teardown(server).await;
        }
    };
    tokio::pin!(stopping);
    let began_stopping = Instant::now();
    // Stopping waits for running calls, and for as long as it takes for an edit
    // that has begun writing. Another signal meanwhile counts by its kind.
    loop {
        tokio::select! {
            () = &mut stopping => break,
            again = stop.recv() => {
                match again_while_stopping(signal, began_stopping.elapsed(), again) {
                    AgainWhileStopping::Ignore => {}
                    AgainWhileStopping::RemoveSessionFolder => {
                        note(format_args!(
                            "Received {again}; removing the session folder while stopping."
                        ));
                        remove_session_folder(&session_dir);
                    }
                    AgainWhileStopping::StopNow => {
                        note(format_args!(
                            "Received {again}; stopping now without waiting for running calls."
                        ));
                        // The process ends before the teardown that would remove it.
                        remove_session_folder(&session_dir);
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

/// What another stop signal does while the server is stopping.
#[derive(Debug, PartialEq, Eq)]
enum AgainWhileStopping {
    /// Nothing: stopping goes on as before.
    Ignore,
    /// Windows ends the process a few seconds after a console close, too soon for
    /// the rest of the stop: remove the session folder now, then go on stopping
    /// for as long as the process lasts.
    RemoveSessionFolder,
    /// The operator asked again: stop at once.
    StopNow,
}

/// A signal of the same kind within this long of the first is the same request
/// delivered twice.
const REPEATED_SIGNAL_WINDOW: Duration = Duration::from_secs(1);

/// For this long after a hangup began the stop, nobody is left to ask for
/// anything: a session manager tearing down the closed session sends its own
/// signals meanwhile.
const HANGUP_WINDOW: Duration = Duration::from_secs(5);

/// What `again` does, arriving `since_first` after the `first` signal that began
/// the stop.
fn again_while_stopping(first: &str, since_first: Duration, again: &str) -> AgainWhileStopping {
    if ends_the_process_soon(again) {
        AgainWhileStopping::RemoveSessionFolder
    } else if again == "SIGHUP"
        || (first == "SIGHUP" && since_first < HANGUP_WINDOW)
        || (again == first && since_first < REPEATED_SIGNAL_WINDOW)
    {
        // A closed terminal delivers SIGHUP more than once, and logind follows it
        // with SIGTERM when it stops the session.
        AgainWhileStopping::Ignore
    } else {
        AgainWhileStopping::StopNow
    }
}

/// Remove the session folder, which a teardown that runs to its end removes
/// later.
fn remove_session_folder(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir) {
        tracing::debug!(error = %e, "session folder was not removed");
    }
}

/// Register the stop signals, then start and serve until stopped. Registration
/// comes first, so a signal that arrives while the tunnel is still coming up is
/// not lost.
async fn run_with<S, F>(
    register: impl FnOnce() -> Result<S>,
    start: impl FnOnce() -> F,
    on_started: impl FnOnce(&StartedServer) -> Result<()>,
) -> Result<()>
where
    S: StopSource,
    F: Future<Output = Result<StartedServer>>,
{
    let mut stop = register()?;
    serve_until_stopped(&mut stop, start(), on_started).await
}

pub async fn run(args: McpServeArgs) -> Result<()> {
    install_reaping_panic_hook();
    let result = run_with(
        StopSignals::register,
        || start(&args),
        |server| print_endpoint(&args, server),
    )
    .await;
    // Backstop for every exit path, including errors after the tunnel started:
    // nothing this command spawned may outlive it.
    xai_tty_utils::global_process_scope().kill_all();
    let held_back = REJECTED.take_held_back();
    if held_back > 0 {
        note(held_back_rejections_line(held_back));
    }
    // Give the last operator lines a moment to reach standard error.
    notes().flush(Duration::from_millis(500));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Command, PagerArgs};
    use crate::mcp_cmd::{McpArgs, McpCommand};
    use clap::Parser as _;
    use std::sync::atomic::AtomicBool;
    use xai_grok_mcp_server::Reason;

    fn parse(argv: &[&str]) -> McpServeArgs {
        let args = PagerArgs::try_parse_from(argv).expect("args should parse");
        match args.command {
            Some(Command::Mcp(McpArgs {
                command: McpCommand::Serve(s),
            })) => s,
            other => panic!("expected mcp serve, got {other:?}"),
        }
    }

    fn serve_args(root: &std::path::Path, extra: &[&str]) -> McpServeArgs {
        let root = root.to_string_lossy();
        let mut argv = vec!["turbo", "mcp", "serve", "--root", &root];
        argv.extend_from_slice(extra);
        parse(&argv)
    }

    /// Fires as soon as it is polled.
    struct StopNow;

    impl StopSource for StopNow {
        async fn recv(&mut self) -> &'static str {
            "test stop"
        }
    }

    /// Never fires.
    struct StopNever;

    impl StopSource for StopNever {
        async fn recv(&mut self) -> &'static str {
            std::future::pending().await
        }
    }

    /// Fires with `signal` each time the test sends on the channel. Like a real
    /// signal source, it never fires again once nothing can send.
    struct StopWhenSent(&'static str, tokio::sync::mpsc::UnboundedReceiver<()>);

    impl StopSource for StopWhenSent {
        async fn recv(&mut self) -> &'static str {
            match self.1.recv().await {
                Some(()) => self.0,
                None => std::future::pending().await,
            }
        }
    }

    #[test]
    fn serve_defaults_to_readonly_ephemeral_port_and_no_tunnel() {
        let s = parse(&["turbo", "mcp", "serve", "--root", "/tmp/a"]);
        assert_eq!(s.allow, AllowTier::Readonly);
        assert_eq!(s.roots, vec![PathBuf::from("/tmp/a")]);
        assert!(s.port.is_none());
        assert_eq!(s.tunnel, TunnelArg::None);
        assert!(s.tunnel_bin.is_none());
        assert!(!s.json);
    }

    #[test]
    fn serve_accepts_repeated_roots_edit_tier_port_and_json() {
        let s = parse(&[
            "turbo", "mcp", "serve", "--root", "/a", "--root", "/b", "--allow", "edit", "--port",
            "8765", "--json",
        ]);
        assert_eq!(s.roots, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(s.allow, AllowTier::Edit);
        assert_eq!(s.port, Some(8765));
        assert!(s.json);
    }

    #[test]
    fn serve_parses_tunnel_flags() {
        let s = parse(&[
            "turbo",
            "mcp",
            "serve",
            "--root",
            "/a",
            "--tunnel",
            "cloudflare",
            "--tunnel-bin",
            "/opt/cloudflared",
        ]);
        assert_eq!(s.tunnel, TunnelArg::Cloudflare);
        assert_eq!(s.tunnel_bin, Some(PathBuf::from("/opt/cloudflared")));
    }

    #[test]
    fn serve_requires_a_root() {
        assert!(PagerArgs::try_parse_from(["turbo", "mcp", "serve"]).is_err());
    }

    #[test]
    fn serve_has_no_shell_tier() {
        for tier in ["full", "shell", "exec"] {
            assert!(
                PagerArgs::try_parse_from([
                    "turbo", "mcp", "serve", "--root", "/tmp", "--allow", tier
                ])
                .is_err(),
                "--allow {tier} must not exist"
            );
        }
    }

    #[test]
    fn edit_tier_and_tunnel_carry_their_cautions() {
        let readonly = parse(&["turbo", "mcp", "serve", "--root", "/a"]);
        assert!(cautions(&readonly, false).is_empty());
        let edit = parse(&["turbo", "mcp", "serve", "--root", "/a", "--allow", "edit"]);
        let lines = cautions(&edit, true);
        assert!(lines.iter().any(|l| l.contains("trust")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("Cloudflare")), "{lines:?}");
    }

    #[test]
    fn relative_roots_are_absolutized() {
        let out = absolutize_roots(&[PathBuf::from("some/relative/dir")]).unwrap();
        assert!(out[0].is_absolute(), "got {:?}", out[0]);
    }

    #[test]
    fn ensure_within_is_a_no_op_when_unconfined() {
        let d = tempfile::tempdir().unwrap();
        assert!(ensure_within(&[d.path().to_path_buf()], &[]).is_ok());
    }

    #[test]
    fn ensure_within_allows_nested_and_refuses_outside() {
        let confine = tempfile::tempdir().unwrap();
        let nested = confine.path().join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let c = vec![confine.path().to_path_buf()];

        assert!(ensure_within(&[nested], &c).is_ok());
        let err = ensure_within(&[outside.path().to_path_buf()], &c).unwrap_err();
        assert!(err.to_string().contains("refusing to widen"), "got: {err}");
    }

    #[test]
    fn operator_lines_escape_and_shorten_the_clients_tool_name() {
        let throttle = Throttle::new(Duration::from_secs(1));
        let hostile = format!(
            "evil\n\u{1b}[2Jrefused: tool=\"read_file\"\r{}",
            "x".repeat(4096)
        );
        let line = event_line(
            ServeEvent::Refused {
                tool: &hostile,
                reason: Reason::OutsideRoots,
            },
            &throttle,
            Instant::now(),
        )
        .expect("every refusal is reported");
        assert!(
            !line.contains(['\n', '\r', '\u{1b}']),
            "raw control characters reached the terminal: {line:?}"
        );
        assert!(line.len() < 400, "{} bytes: {line}", line.len());
        assert!(line.contains("OutsideRoots"), "{line}");
    }

    #[test]
    fn rejected_credentials_are_reported_at_most_once_a_second() {
        let throttle = Throttle::new(Duration::from_secs(1));
        let t0 = Instant::now();
        let line = |ms| {
            event_line(
                ServeEvent::Unauthorized,
                &throttle,
                t0 + Duration::from_millis(ms),
            )
        };
        assert!(line(0).is_some());
        assert!(line(100).is_none());
        assert!(line(900).is_none());
        let next = line(1000).expect("a line once the interval has passed");
        assert!(next.contains("2 more"), "{next}");
        assert!(line(1500).is_none());
        let after = line(2000).expect("a line once the interval has passed");
        assert!(after.contains("1 more"), "{after}");
    }

    #[test]
    fn refusals_are_never_throttled() {
        let throttle = Throttle::new(Duration::from_secs(1));
        let now = Instant::now();
        for _ in 0..5 {
            let refused = ServeEvent::Refused {
                tool: "read_file",
                reason: Reason::OutsideRoots,
            };
            assert!(event_line(refused, &throttle, now).is_some());
        }
    }

    #[test]
    fn operator_lines_never_wait_for_a_stuck_stderr() {
        let (release, stuck) = std::sync::mpsc::channel::<()>();
        let written = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = written.clone();
        // Each write waits until the test releases it, like a pipe nobody reads.
        let notes = Notes::start(move |line| {
            let _ = stuck.recv();
            sink.lock().unwrap().push(line.to_string());
        });
        let started = Instant::now();
        for i in 0..NOTE_QUEUE * 4 {
            notes.push(format!("line {i}"));
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "queuing operator lines waited for the writer"
        );
        assert!(
            notes.dropped.load(Ordering::Relaxed) > 0,
            "a full queue must drop lines, not wait"
        );

        drop(release);
        // Stopping says how many lines were lost, even when no line follows.
        notes.flush(Duration::from_secs(10));
        notes.push("after".to_string());
        notes.flush(Duration::from_secs(10));
        let written = written.lock().unwrap();
        assert!(
            written
                .iter()
                .any(|line| line.ends_with("operator line(s) were not shown")),
            "{:?}",
            &written[written.len().saturating_sub(3)..]
        );
        assert_eq!(written.last().map(String::as_str), Some("after"));
    }

    /// The dropped lines a written operator line reports.
    fn reported_drops(line: &str) -> u64 {
        if let Some(count) = line.strip_suffix(" operator line(s) were not shown") {
            return count.parse().unwrap();
        }
        line.strip_suffix(" earlier line(s) not shown)")
            .and_then(|rest| rest.rsplit_once('('))
            .map_or(0, |(_, count)| count.parse().unwrap())
    }

    #[test]
    fn every_dropped_operator_line_is_reported_exactly_once() {
        let (release, stuck) = std::sync::mpsc::channel::<()>();
        let written = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = written.clone();
        let notes = Arc::new(Notes::start(move |line| {
            let _ = stuck.recv();
            sink.lock().unwrap().push(line.to_string());
        }));
        let pushers: Vec<_> = (0..4)
            .map(|thread| {
                let notes = notes.clone();
                std::thread::spawn(move || {
                    for i in 0..NOTE_QUEUE {
                        notes.push(format!("thread {thread} line {i}"));
                    }
                })
            })
            .collect();
        for pusher in pushers {
            pusher.join().unwrap();
        }
        drop(release);
        // Lines that find room while the writer drains carry the count, which a
        // count read and later subtracted would report twice.
        let late: Vec<_> = (0..4)
            .map(|thread| {
                let notes = notes.clone();
                std::thread::spawn(move || {
                    for i in 0..NOTE_QUEUE {
                        notes.push(format!("late {thread} line {i}"));
                    }
                })
            })
            .collect();
        for pusher in late {
            pusher.join().unwrap();
        }
        notes.flush(Duration::from_secs(10));
        let written = written.lock().unwrap();
        let shown = written
            .iter()
            .filter(|line| !line.ends_with("operator line(s) were not shown"))
            .count() as u64;
        let reported: u64 = written.iter().map(|line| reported_drops(line)).sum();
        assert_eq!(
            shown + reported,
            (NOTE_QUEUE * 8) as u64,
            "{:?}",
            &written[written.len().saturating_sub(3)..]
        );
    }

    #[test]
    fn tracing_output_never_waits_for_a_stuck_stderr() {
        use tracing_subscriber::layer::SubscriberExt as _;
        let (release, stuck) = std::sync::mpsc::channel::<()>();
        let notes: &'static Notes = Box::leak(Box::new(Notes::start(move |_line| {
            let _ = stuck.recv();
        })));
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(OperatorStderr { notes }),
        );
        let started = Instant::now();
        tracing::subscriber::with_default(subscriber, || {
            for i in 0..NOTE_QUEUE * 4 {
                tracing::error!("tool calls did not stop after cancellation ({i})");
            }
        });
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a tracing event waited for the writer"
        );
        assert!(notes.dropped.load(Ordering::Relaxed) > 0);
        drop(release);
    }

    /// Fires once for each signal the test sends.
    struct StopEachTime(tokio::sync::mpsc::UnboundedReceiver<&'static str>);

    impl StopSource for StopEachTime {
        async fn recv(&mut self) -> &'static str {
            match self.0.recv().await {
                Some(signal) => signal,
                None => std::future::pending().await,
            }
        }
    }

    /// An authenticated request whose body never finishes, which holds a request
    /// slot: stopping waits for request slots.
    async fn hold_a_request_slot(
        addr: std::net::SocketAddr,
        url: &str,
        token: &str,
    ) -> tokio::net::TcpStream {
        use tokio::io::AsyncWriteExt as _;
        let path = url
            .split_once(&addr.to_string())
            .map(|(_, path)| path.to_string())
            .expect("the URL names the address");
        let mut request = tokio::net::TcpStream::connect(addr).await.unwrap();
        request
            .write_all(
                format!(
                    "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\n\
                     Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
                     Content-Length: 64\r\n\r\n{{"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        request
    }

    #[tokio::test]
    async fn another_stop_signal_ends_a_stop_that_is_still_waiting() {
        let root = tempfile::tempdir().unwrap();
        let args = serve_args(root.path(), &[]);
        let (signals, received) = tokio::sync::mpsc::unbounded_channel();
        let mut stop = StopEachTime(received);
        let (started, on_start) = tokio::sync::oneshot::channel();
        let run = serve_until_stopped(&mut stop, start_with_confine(&args, &[]), move |server| {
            let _ = started.send((
                server.handle.addr,
                server.handle.url.clone(),
                server.handle.token.clone(),
                server.toolset.clone(),
            ));
            Ok(())
        });
        tokio::pin!(run);
        let (addr, url, token, toolset) = tokio::select! {
            outcome = &mut run => panic!("the command ended before the server started: {outcome:?}"),
            up = on_start => up.expect("the server started"),
        };
        let request = hold_a_request_slot(addr, &url, &token).await;
        signals.send("SIGTERM").unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(700), &mut run)
                .await
                .is_err(),
            "the first stop did not wait for the unfinished request"
        );
        signals.send("SIGINT").unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), &mut run)
            .await
            .expect("another signal ends the stop at once");
        assert!(outcome.is_ok(), "{outcome:?}");
        assert!(
            !toolset.session_dir().exists(),
            "the session folder outlived the stop"
        );
        drop(request);
    }

    #[tokio::test]
    async fn a_hangup_or_a_quick_repeat_does_not_end_a_stop_that_is_still_waiting() {
        let root = tempfile::tempdir().unwrap();
        let args = serve_args(root.path(), &[]);
        let (signals, received) = tokio::sync::mpsc::unbounded_channel();
        let mut stop = StopEachTime(received);
        let (started, on_start) = tokio::sync::oneshot::channel();
        let run = serve_until_stopped(&mut stop, start_with_confine(&args, &[]), move |server| {
            let _ = started.send((
                server.handle.addr,
                server.handle.url.clone(),
                server.handle.token.clone(),
            ));
            Ok(())
        });
        tokio::pin!(run);
        let (addr, url, token) = tokio::select! {
            outcome = &mut run => panic!("the command ended before the server started: {outcome:?}"),
            up = on_start => up.expect("the server started"),
        };
        let request = hold_a_request_slot(addr, &url, &token).await;
        // A closed terminal delivers SIGHUP twice, and a signal can arrive twice
        // in quick succession.
        signals.send("SIGTERM").unwrap();
        signals.send("SIGTERM").unwrap();
        signals.send("SIGHUP").unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(700), &mut run)
                .await
                .is_err(),
            "a repeated signal or a hangup ended the stop"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
        // The same kind again, well after the first, is a request of its own.
        signals.send("SIGTERM").unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), &mut run)
            .await
            .expect("a later signal ends the stop");
        assert!(outcome.is_ok(), "{outcome:?}");
        drop(request);
    }

    #[tokio::test]
    async fn a_console_close_while_stopping_removes_the_session_folder_and_stopping_goes_on() {
        let root = tempfile::tempdir().unwrap();
        let args = serve_args(root.path(), &[]);
        let (signals, received) = tokio::sync::mpsc::unbounded_channel();
        let mut stop = StopEachTime(received);
        let (started, on_start) = tokio::sync::oneshot::channel();
        let run = serve_until_stopped(&mut stop, start_with_confine(&args, &[]), move |server| {
            let _ = started.send((
                server.handle.addr,
                server.handle.url.clone(),
                server.handle.token.clone(),
                server.toolset.clone(),
            ));
            Ok(())
        });
        tokio::pin!(run);
        let (addr, url, token, toolset) = tokio::select! {
            outcome = &mut run => panic!("the command ended before the server started: {outcome:?}"),
            up = on_start => up.expect("the server started"),
        };
        let request = hold_a_request_slot(addr, &url, &token).await;
        signals.send("Ctrl-C").unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut run)
                .await
                .is_err()
        );
        signals.send(CONSOLE_CLOSED).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(700), &mut run)
                .await
                .is_err(),
            "a console close ended the stop"
        );
        assert!(
            !toolset.session_dir().exists(),
            "the session folder outlived a console close"
        );
        signals.send("Ctrl-Break").unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), &mut run)
            .await
            .expect("another signal ends the stop");
        assert!(outcome.is_ok(), "{outcome:?}");
        drop(request);
    }

    #[test]
    fn another_signal_while_stopping_counts_by_its_kind() {
        let soon = Duration::from_millis(100);
        let later = Duration::from_secs(3);
        let much_later = Duration::from_secs(6);
        let cases = [
            ("SIGTERM", soon, "SIGINT", AgainWhileStopping::StopNow),
            ("SIGTERM", soon, "SIGTERM", AgainWhileStopping::Ignore),
            ("SIGTERM", later, "SIGTERM", AgainWhileStopping::StopNow),
            ("SIGHUP", later, "SIGHUP", AgainWhileStopping::Ignore),
            ("SIGHUP", soon, "SIGTERM", AgainWhileStopping::Ignore),
            ("SIGHUP", later, "SIGINT", AgainWhileStopping::Ignore),
            ("SIGHUP", much_later, "SIGTERM", AgainWhileStopping::StopNow),
            ("Ctrl-C", later, "SIGHUP", AgainWhileStopping::Ignore),
            ("Ctrl-C", later, "Ctrl-C", AgainWhileStopping::StopNow),
            (
                "Ctrl-C",
                soon,
                CONSOLE_CLOSED,
                AgainWhileStopping::RemoveSessionFolder,
            ),
        ];
        for (first, since_first, again, expected) in cases {
            assert_eq!(
                again_while_stopping(first, since_first, again),
                expected,
                "{first} then {again} after {since_first:?}"
            );
        }
    }

    #[test]
    fn rejections_held_back_are_reported_once_a_burst_is_over_and_at_exit() {
        let throttle = Throttle::new(Duration::from_secs(1));
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        assert_eq!(throttle.admit(at(0)), Some(0));
        assert_eq!(throttle.admit(at(100)), None);
        assert_eq!(throttle.admit(at(200)), None);
        // Still within the interval, the burst may go on.
        assert_eq!(throttle.take_overdue(at(500)), None);
        assert_eq!(throttle.take_overdue(at(1100)), Some(2));
        assert_eq!(throttle.take_overdue(at(3000)), None);
        assert_eq!(throttle.admit(at(3100)), Some(0));
        assert_eq!(throttle.admit(at(3200)), None);
        assert_eq!(throttle.take_held_back(), 1);
        assert_eq!(throttle.take_held_back(), 0);
    }

    #[test]
    fn events_windows_ends_the_process_after_are_recognised() {
        assert!(ends_the_process_soon(CONSOLE_CLOSED));
        for signal in ["Ctrl-C", "Ctrl-Break", "SIGINT", "SIGTERM", "SIGHUP"] {
            assert!(!ends_the_process_soon(signal), "{signal}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_signals_capture_a_real_sighup() {
        let mut stop = StopSignals::register().expect("signal handlers register");
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGHUP).expect("raise SIGHUP");
        let got = tokio::time::timeout(Duration::from_secs(5), stop.recv())
            .await
            .expect("SIGHUP reaches the registered handler");
        assert_eq!(got, "SIGHUP");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn stop_signals_register_on_windows() {
        StopSignals::register().expect("Ctrl-C, Ctrl-Break and console close handlers register");
    }

    #[tokio::test]
    async fn stop_signals_are_registered_before_startup_begins() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let (on_register, on_start) = (order.clone(), order.clone());
        let outcome = run_with(
            move || {
                on_register.lock().unwrap().push("register");
                Ok(StopNever)
            },
            move || {
                on_start.lock().unwrap().push("start");
                async { Err::<StartedServer, _>(anyhow::anyhow!("startup failed")) }
            },
            |_| panic!("startup never finished"),
        )
        .await;
        assert!(outcome.is_err());
        assert_eq!(*order.lock().unwrap(), ["register", "start"]);
    }

    #[tokio::test]
    async fn a_failed_registration_never_starts_the_server() {
        let started = Arc::new(AtomicBool::new(false));
        let flag = started.clone();
        let outcome = run_with(
            || Err::<StopNever, _>(anyhow::anyhow!("no signal handlers")),
            move || {
                flag.store(true, Ordering::SeqCst);
                std::future::pending::<Result<StartedServer>>()
            },
            |_| Ok(()),
        )
        .await;
        assert!(outcome.is_err());
        assert!(!started.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_stop_during_startup_ends_without_waiting_for_startup() {
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            serve_until_stopped(
                &mut StopNow,
                std::future::pending::<Result<StartedServer>>(),
                |_| panic!("startup never finished"),
            ),
        )
        .await;
        assert!(matches!(outcome, Ok(Ok(()))), "{outcome:?}");
    }

    /// Start a server, stop it with `signal` once it is up, and return what the
    /// test needs to check afterwards.
    async fn serve_then_stop(signal: &'static str) -> (std::net::SocketAddr, Arc<ServedToolset>) {
        let root = tempfile::tempdir().unwrap();
        let args = serve_args(root.path(), &[]);
        let (stop_now, stop_rx) = tokio::sync::mpsc::unbounded_channel();
        let bound = Arc::new(Mutex::new(None));
        let seen = bound.clone();
        let mut stop = StopWhenSent(signal, stop_rx);
        let run = serve_until_stopped(&mut stop, start_with_confine(&args, &[]), move |server| {
            *seen.lock().unwrap() = Some((server.handle.addr, server.toolset.clone()));
            stop_now.send(()).unwrap();
            Ok(())
        });
        tokio::time::timeout(Duration::from_secs(60), run)
            .await
            .expect("teardown finishes")
            .expect("the server starts and stops");
        bound.lock().unwrap().take().expect("the server started")
    }

    #[tokio::test]
    async fn a_stop_after_startup_tears_the_server_down() {
        let (addr, toolset) = serve_then_stop("test stop").await;
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "{addr} still accepts connections after teardown"
        );
        assert!(toolset.is_shutting_down(), "the toolset still takes calls");
    }

    #[tokio::test]
    async fn a_console_close_removes_the_session_folder_and_stops_the_server() {
        let (addr, toolset) = serve_then_stop(CONSOLE_CLOSED).await;
        // The toolset is still alive here, so only the early removal can have
        // deleted its folder.
        assert!(
            !toolset.session_dir().exists(),
            "the session folder outlived a console close"
        );
        assert!(tokio::net::TcpStream::connect(addr).await.is_err());
        assert!(toolset.is_shutting_down());
    }

    #[tokio::test]
    async fn an_endpoint_that_cannot_be_printed_stops_the_server() {
        let root = tempfile::tempdir().unwrap();
        let args = serve_args(root.path(), &[]);
        let bound = Arc::new(Mutex::new(None));
        let seen = bound.clone();
        let mut stop = StopNever;
        let run = serve_until_stopped(&mut stop, start_with_confine(&args, &[]), move |server| {
            *seen.lock().unwrap() = Some(server.handle.addr);
            Err(anyhow::anyhow!("standard output is closed"))
        });
        let outcome = tokio::time::timeout(Duration::from_secs(60), run)
            .await
            .expect("teardown finishes");
        assert!(
            format!("{:#}", outcome.unwrap_err()).contains("standard output is closed"),
            "the print failure is the error returned"
        );
        let addr = bound.lock().unwrap().expect("the server started");
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "{addr} still accepts connections"
        );
    }

    #[tokio::test]
    async fn the_json_endpoint_is_one_object_and_its_cautions_go_to_stderr() {
        let root = tempfile::tempdir().unwrap();
        let args = serve_args(root.path(), &["--allow", "edit", "--json"]);
        let s = start_with_confine(&args, &[]).await.expect("server starts");
        let Endpoint { stdout, stderr } = endpoint(&args, &s);
        assert_eq!(stdout.lines().count(), 1, "{stdout}");
        let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
        assert_eq!(v["url"], s.handle.url.as_str());
        assert_eq!(v["bearer_token"], s.handle.token.as_str());
        assert_eq!(v["allow"], "edit");
        assert!(v["public_url"].is_null(), "{v}");
        assert_eq!(v["roots"].as_array().map(Vec::len), Some(1), "{v}");
        assert!(
            v["tools"]
                .as_array()
                .is_some_and(|t| t.iter().any(|n| n == "search_replace")),
            "{v}"
        );
        assert!(stderr.iter().any(|l| l.contains("trust")), "{stderr:?}");
    }

    #[tokio::test]
    async fn start_binds_loopback_and_readonly_serves_no_edit_tool() {
        let root = tempfile::tempdir().unwrap();
        let args = serve_args(root.path(), &[]);
        let s = start_with_confine(&args, &[]).await.expect("server starts");
        assert!(s.handle.addr.ip().is_loopback(), "{}", s.handle.addr);
        assert!(s.tunnel.is_none());
        assert!(s.tools.iter().any(|t| t == "read_file"), "{:?}", s.tools);
        assert!(
            !s.tools.iter().any(|t| t == "search_replace"),
            "{:?}",
            s.tools
        );
    }

    #[tokio::test]
    async fn start_edit_tier_serves_search_replace() {
        let root = tempfile::tempdir().unwrap();
        let args = serve_args(root.path(), &["--allow", "edit"]);
        let s = start_with_confine(&args, &[]).await.expect("server starts");
        assert!(
            s.tools.iter().any(|t| t == "search_replace"),
            "{:?}",
            s.tools
        );
    }

    #[tokio::test]
    async fn start_refuses_the_home_directory_as_a_root() {
        let home = dirs::home_dir().expect("a home directory");
        let args = serve_args(&home, &[]);
        match start_with_confine(&args, &[]).await {
            Ok(_) => panic!("the home directory must be refused as a root"),
            Err(e) => assert!(format!("{e:#}").contains("home directory"), "{e:#}"),
        }
    }

    #[tokio::test]
    async fn start_refuses_a_root_outside_the_inherited_confine() {
        let confine = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let args = serve_args(outside.path(), &[]);
        match start_with_confine(&args, &[confine.path().to_path_buf()]).await {
            Ok(_) => panic!("a root outside --confine must be refused"),
            Err(e) => assert!(
                format!("{e:#}").contains("refusing to widen"),
                "refused for the wrong reason: {e:#}"
            ),
        }
    }

    #[tokio::test]
    async fn start_fails_fast_when_the_tunnel_binary_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .and_then(|probe| probe.local_addr())
            .expect("an ephemeral port")
            .port()
            .to_string();
        let args = serve_args(
            root.path(),
            &[
                "--port",
                &port,
                "--tunnel",
                "cloudflare",
                "--tunnel-bin",
                "definitely/not/a/real/cloudflared",
            ],
        );
        match start_with_confine(&args, &[]).await {
            Ok(_) => panic!("a missing tunnel binary must fail startup, not fall back"),
            // Only the explicit-path check says "not found at".
            Err(e) => assert!(format!("{e:#}").contains("not found at"), "got: {e:#}"),
        }
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", port.parse::<u16>().unwrap())).is_ok(),
            "port {port} was bound before the missing binary was noticed"
        );
    }

    /// The server crate proves its folder-trust marker table covers every kind
    /// folder trust gates on and that each sample is write-refused. This proves
    /// the other half: every sample really is a marker folder trust detects.
    #[test]
    fn folder_trust_marker_samples_are_real_markers() {
        use xai_grok_mcp_server::guard::FOLDER_TRUST_MARKER_SAMPLES;

        fn body(kind: &str, rel: &str) -> &'static str {
            match (kind, rel) {
                ("mcp", ".grok/config.toml") => "[mcp_servers.example]\ncommand = \"example\"\n",
                ("plugins", ".grok/config.toml") => "[plugins]\npaths = [\"plugins/example\"]\n",
                ("permission", ".grok/config.toml") => "[permission]\nallow = [\"read_file\"]\n",
                (_, r) if r.ends_with(".json") => "{}\n",
                _ => "example\n",
            }
        }

        for (kind, paths) in FOLDER_TRUST_MARKER_SAMPLES {
            for rel in *paths {
                let dir = tempfile::tempdir().unwrap();
                // An empty `.git` bounds the settings walk at this directory.
                std::fs::create_dir(dir.path().join(".git")).unwrap();
                let file = dir.path().join(rel);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(&file, body(kind, rel)).unwrap();
                let found = xai_grok_workspace::folder_trust::repo_config_kinds(dir.path());
                assert!(
                    found.contains(kind),
                    "{rel} should be a {kind:?} marker; folder trust found {found:?}"
                );
            }
        }
    }
}
