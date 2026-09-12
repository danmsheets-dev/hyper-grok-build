//! The served toolset: a `ToolBridge` over a confined filesystem, with every
//! dispatch gated by [`crate::guard::PathGuard`].
//!
//! # Layering
//!
//! - A call may name only the arguments its tool advertises.
//! - [`PathGuard`] checks every path argument before dispatch, and each walk
//!   target is then rewritten to its canonical spelling. The check touches the
//!   disk, so it runs on the call's own thread, under its permit and timeout.
//! - [`ReadConfinedFs`] re-checks the *resolved* path of every read and write the
//!   tools perform, after their own sanitizing, symlink following and Unicode
//!   filename fallback. It also refuses documents, oversized reads and writes,
//!   and special files there, where the real target is known.
//! - `ConfinedFs` re-checks writes once more at the filesystem layer.
//! - For grep, which reads through neither: `DenyReadGlobs` excludes denied
//!   names during the walk, and a [`ServedGrepPolicy`] makes ripgrep ignore the
//!   operator's config, never follow symlinks, and drop every result from a file
//!   the guard refuses, without letting the reply show that it did.
//! - A [`ServedEditPolicy`] bounds the file an edit may produce and the work
//!   `search_replace` does after writing, and keeps it from saving a copy of
//!   the file it changes.
//!
//! # Reporting
//!
//! A tool rewrites a filesystem-layer refusal into its own text. The toolset
//! never reads tool text to find refusals: the filesystem layer records its
//! decision in the call's scope, and that record decides what the client is
//! told. A refusal by Turbo's workspace policy is recognised by its error code
//! and reported as the same opaque refusal, because its text can quote the
//! policy files.
//!
//! # Tool admission
//!
//! A tool is served only if [`validate_tool_schema`] accepts its live advertised
//! schema, so a renamed argument drops the tool instead of silently disabling a
//! check.
//!
//! # Resource bounds and shutdown
//!
//! At most [`MAX_CONCURRENT_CALLS`] calls run at once. Each runs on a
//! blocking-pool thread that owns its permit until the tool really finishes.
//! `list_dir` walks its tree synchronously inside an async body, and a tokio
//! timeout cannot interrupt synchronous work, so on an async worker such a call
//! would both overrun its deadline and occupy the worker. On its own thread the
//! client gets the timeout answer after [`CALL_TIMEOUT`], and the permit keeps
//! the concurrency bound honest while the work finishes.
//!
//! Shutdown refuses new calls, gives running ones a drain window, then cancels
//! what is left: each call's tool future is dropped, which kills a served grep's
//! ripgrep, and shutdown waits a bounded time for the permits to come back. An
//! edit that has begun writing is not cancelled, and shutdown waits for it to
//! end however long that takes.
//!
//! Session state lives in a private per-process directory that is removed when
//! the toolset is dropped.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use xai_grok_tools::bridge::ToolBridge;
use xai_grok_tools::computer::local::{ConfinedFs, LocalFs, LocalTerminalBackend};
use xai_grok_tools::computer::types::{AsyncFileSystem, TerminalBackend};
use xai_grok_tools::implementations::grok_build;
use xai_grok_tools::implementations::grok_build::scheduler::types::SchedulerHandle;
use xai_grok_tools::notification::ToolNotificationHandle;
use xai_grok_tools::registry::types::{SessionContext, ToolServerConfig};
use xai_grok_tools::types::resources::{
    DenyReadGlobs, ServedEditPolicy, ServedGrepPolicy, SystemRemindersEnabled,
};
use xai_grok_tools::types::tool::ToolKind;

use crate::guard::{Access, Denial, PathGuard, Reason, log_preview, validate_tool_schema};
use crate::private_dir::SessionDir;
use crate::read_confined_fs::{
    Blocked, CallScope, MAX_WHOLE_FILE_BYTES, ReadConfinedFs, in_call_scope,
};

/// Tool calls allowed to run at once. Further calls are refused, not queued.
pub const MAX_CONCURRENT_CALLS: usize = 8;
/// How long the server waits for one call. Longer than any served tool's own
/// limit (grep allows 60 s under WSL), so a tool's own timeout, which also stops
/// its child process, normally fires first.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(90);
/// How long shutdown waits for running calls before cancelling them.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(10);
/// How long shutdown waits for cancelled calls to hand back their permits.
const CANCEL_GRACE: Duration = Duration::from_secs(5);
/// Replacements per served edit whose details are built. Each detail scans and
/// copies from the whole file, and the client never sees them.
const MAX_DETAILED_EDITS: usize = 1;

/// The error code Turbo's workspace policy puts on a refusal
/// (`grok_build::policy::POLICY_DENIED`).
const POLICY_DENIED: &str = "policy_denied";

const DOCUMENT_REFUSAL: &str =
    "document formats (PDF, PowerPoint) are not available over this MCP server";
const TOO_LARGE: &str = "the file is too large to read or change over this MCP server";
const SHUTTING_DOWN: &str = "the server is shutting down";

/// Construction settings the tests change.
#[derive(Clone)]
pub(crate) struct ToolsetOptions {
    pub call_timeout: Duration,
    pub shutdown_drain: Duration,
    pub cancel_grace: Duration,
    #[cfg(test)]
    pub hooks: crate::read_confined_fs::TestHooks,
}

impl Default for ToolsetOptions {
    fn default() -> Self {
        Self {
            call_timeout: CALL_TIMEOUT,
            shutdown_drain: SHUTDOWN_DRAIN,
            cancel_grace: CANCEL_GRACE,
            #[cfg(test)]
            hooks: Default::default(),
        }
    }
}

/// One tool as advertised to an external client.
#[derive(Debug, Clone)]
pub struct ServedTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
    pub kind: ToolKind,
}

/// Why a call did not produce a result.
#[derive(Debug)]
pub enum CallFailure {
    /// The boundary refused it. Carries the opaque wire message.
    Refused(Denial),
    /// The tool ran, or could not be run, and failed on its own terms.
    Failed(String),
}

/// Something the operator may want to see. Never sent to the client.
#[derive(Debug, Clone, Copy)]
pub enum ServeEvent<'a> {
    Refused { tool: &'a str, reason: Reason },
    Unauthorized,
}

pub type EventObserver = Arc<dyn for<'a> Fn(ServeEvent<'a>) + Send + Sync>;

/// A tool's own failure, as a call's thread hands it back.
struct ToolFailure {
    text: String,
    /// Turbo's workspace policy refused the call.
    policy_denied: bool,
}

/// What a call's thread hands back.
enum Work {
    /// The argument check, or its repeat after a failed edit, refused the call.
    Refused(Denial),
    /// Shutdown cancelled the call before it began to write.
    Stopped,
    /// The tool ran.
    Done(Result<String, ToolFailure>),
}

pub struct ServedToolset {
    bridge: ToolBridge,
    guard: PathGuard,
    served: Vec<ServedTool>,
    next_id: AtomicU64,
    permits: Arc<Semaphore>,
    call_timeout: Duration,
    shutdown_drain: Duration,
    cancel_grace: Duration,
    shutting_down: AtomicBool,
    /// Every running call's scope, so shutdown can find calls that have begun
    /// to write.
    scopes: std::sync::Mutex<Vec<std::sync::Weak<CallScope>>>,
    /// Cancelled by [`Self::shutdown`] after the drain window.
    cancel: CancellationToken,
    observer: Option<EventObserver>,
    /// Private per-process session folder (see `private_dir`). Declared last so
    /// it is removed after the bridge that writes into it.
    session_dir: SessionDir,
}

impl ServedToolset {
    /// Build the served toolset over `roots`.
    pub async fn new(roots: Vec<PathBuf>, read_only: bool) -> anyhow::Result<Self> {
        Self::with_options(roots, read_only, ToolsetOptions::default()).await
    }

    pub(crate) async fn with_options(
        roots: Vec<PathBuf>,
        read_only: bool,
        options: ToolsetOptions,
    ) -> anyhow::Result<Self> {
        #[cfg(test)]
        let build_gate = options.hooks.build_gate.clone();
        // Building the guard reads the disk: home folders, mounts and every
        // configuration file under the roots. It runs off the runtime, so a stop
        // signal can still end startup while it does.
        let guard = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            {
                if let Some(gate) = build_gate {
                    gate.wait();
                }
            }
            PathGuard::new(roots, Vec::new(), read_only)
        })
        .await
        .context("the root check stopped unexpectedly")?
        .map_err(|d| match d.reason {
            Reason::OverBroadRoot => anyhow::anyhow!(
                "invalid roots: a root may not be a filesystem root, a home directory, \
                 or a directory that contains a home directory; approve a project \
                 directory instead"
            ),
            Reason::TooManyDeclarations => anyhow::anyhow!(
                "invalid roots: the configuration under these roots declares more plugin, \
                 skill and marketplace locations than the edit tier can check every write \
                 against; serve these roots read-only, or trim the declarations"
            ),
            Reason::Inadmissible => anyhow::anyhow!(
                "invalid roots: no client path could name a file under this root: its path \
                 has a quote character, a colon inside a name, a reserved device name such \
                 as `aux` or `con`, a name ending in a dot or space, an invisible \
                 character, or a name that is not valid Unicode, or it is a UNC or \
                 `\\\\?\\` path; the cause is on standard error"
            ),
            reason => anyhow::anyhow!(
                "invalid roots: each root must be an existing absolute directory ({reason:?})"
            ),
        })?;
        #[cfg(test)]
        let guard = guard.with_check_gate(options.hooks.check_gate.clone());
        let roots = guard.roots();

        let session_dir =
            crate::private_dir::create().context("could not create the private session folder")?;

        let confined = ReadConfinedFs::new(
            Arc::new(ConfinedFs::with_roots(Arc::new(LocalFs), roots.clone())),
            guard.clone(),
        );
        #[cfg(test)]
        let confined = confined.with_hooks(options.hooks.clone());
        let fs: Arc<dyn AsyncFileSystem> = Arc::new(confined);
        let backend: Arc<dyn TerminalBackend> = Arc::new(LocalTerminalBackend::new());

        // Turbo's scheduler runs `/schedule` jobs, and records each due fire in
        // `.grok/schedules.json` in the first root even with nothing to run the
        // job. A served toolset runs none: its handle leads nowhere, so no
        // scheduler starts.
        let (scheduler, _) = tokio::sync::mpsc::unbounded_channel();
        let ctx = SessionContext {
            backend,
            fs,
            cwd: roots[0].clone(),
            session_folder: session_dir.path().to_path_buf(),
            session_env: Arc::new(HashMap::new()),
            notification_handle: ToolNotificationHandle::noop(),
            owner_session_id: None,
            subagent: None,
            parent_scheduler_handle: Some(SchedulerHandle(scheduler)),
            skills: vec![],
            state_path: session_dir.path().join("state.json"),
            memory_backend: None,
            web_search_config: Default::default(),
            web_fetch_config: Default::default(),
            lsp: None,
            image_gen_config: Default::default(),
            video_gen_config: Default::default(),
            app_builder_deployer_config: Default::default(),
            api_key_provider: None,
            auth_provider: None,
            attribution_callback: None,
            system_reminder_tag: xai_grok_tools::reminders::DEFAULT_REMINDER_TAG,
        };

        // No shell: a shell command's operands cannot be checked.
        let mut tools = vec![
            (&grok_build::ReadFileTool).into(),
            (&grok_build::ListDirTool).into(),
            (&grok_build::GrepTool).into(),
        ];
        if !read_only {
            tools.push((&grok_build::SearchReplaceTool).into());
        }
        let cfg = ToolServerConfig {
            tools,
            behavior_preset: None,
        };

        let bridge = ToolBridge::finalize_builder(ToolBridge::get_builder(), cfg, ctx)
            .await
            .map_err(|e| anyhow::anyhow!("toolset finalize failed: {e}"))?;

        bridge.set_confine_root(roots[0].clone()).await;
        if roots.len() > 1 {
            bridge.set_additional_directories(roots[1..].to_vec()).await;
        }
        // grep appends these after any caller glob, so they win over a caller
        // glob and over `.ignore` whitelists alike.
        bridge
            .update_resource(DenyReadGlobs(guard.deny_read_globs()))
            .await;
        // Globs match names; this matches the file each result really came from,
        // including through links, unusual spellings and repositories named
        // anything.
        let result_guard = guard.clone();
        bridge
            .update_resource(ServedGrepPolicy {
                allow_file: Arc::new(move |path: &Path| {
                    match result_guard.check_resolved(path, Access::Read) {
                        Ok(()) => true,
                        Err(denial) => {
                            tracing::debug!(
                                file = ?path,
                                reason = ?denial.reason,
                                "dropped a grep result from a refused file"
                            );
                            false
                        }
                    }
                }),
            })
            .await;
        // Every replacement copies `new_string`, so what an edit builds is
        // bounded here, not only what it reads. A served client has no undo, so
        // an edit keeps no copy of the file's previous content.
        bridge
            .update_resource(ServedEditPolicy {
                max_result_bytes: MAX_WHOLE_FILE_BYTES as usize,
                max_detailed_edits: MAX_DETAILED_EDITS,
                record_receipts: false,
            })
            .await;
        // Reminders run after every call, outside the guard: skill discovery walks
        // up from each file a tool touched, past a root and into `.grok` folders,
        // and reads every skill file it finds. A served client needs none of them.
        bridge.update_resource(SystemRemindersEnabled(false)).await;

        let mut served = Vec::new();
        let mut seen = BTreeSet::new();
        for def in bridge.tool_definitions().await {
            let name = def.function.name.clone();
            if !seen.insert(name.clone()) {
                anyhow::bail!("duplicate tool name in served config: {name}");
            }
            if let Err(d) = validate_tool_schema(&name, &def.function.parameters) {
                tracing::warn!(
                    tool = %name,
                    reason = ?d.reason,
                    "not served: advertised schema failed declaration validation"
                );
                continue;
            }
            let Some(kind) = bridge.tool_kind(&name) else {
                tracing::warn!(tool = %name, "not served: no ToolKind");
                continue;
            };
            served.push(ServedTool {
                name,
                description: def.function.description.unwrap_or_default(),
                schema: def.function.parameters,
                kind,
            });
        }

        anyhow::ensure!(
            !served.is_empty(),
            "no tool survived schema validation; the declaration table is stale"
        );

        Ok(Self {
            bridge,
            guard,
            served,
            next_id: AtomicU64::new(0),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_CALLS)),
            call_timeout: options.call_timeout,
            shutdown_drain: options.shutdown_drain,
            cancel_grace: options.cancel_grace,
            shutting_down: AtomicBool::new(false),
            scopes: std::sync::Mutex::default(),
            cancel: CancellationToken::new(),
            observer: None,
            session_dir,
        })
    }

    /// Receive operator-facing events (refusals, rejected credentials).
    pub fn set_observer(&mut self, observer: EventObserver) {
        self.observer = Some(observer);
    }

    pub fn notify(&self, event: ServeEvent<'_>) {
        if let Some(observer) = &self.observer {
            observer(event);
        }
    }

    pub fn list(&self) -> &[ServedTool] {
        &self.served
    }

    pub fn is_read_only(&self) -> bool {
        self.guard.is_read_only()
    }

    /// The canonicalized approved roots, as the guard settled them.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.guard.roots()
    }

    /// The approved roots as the operator should see them: spellings a client
    /// can use. They differ from [`Self::roots`] only where the canonical form
    /// carries a verbatim prefix, which no client path may use.
    pub fn printable_roots(&self) -> Vec<PathBuf> {
        self.guard.printable_roots()
    }

    /// This instance's private session folder.
    pub fn session_dir(&self) -> &Path {
        self.session_dir.path()
    }

    /// Calls whose tool work is still running, including calls the client has
    /// already stopped waiting for.
    pub fn in_flight(&self) -> usize {
        MAX_CONCURRENT_CALLS - self.permits.available_permits()
    }

    fn refused(&self, tool: &str, denial: Denial) -> CallFailure {
        self.notify(ServeEvent::Refused {
            tool,
            reason: denial.reason,
        });
        CallFailure::Refused(denial)
    }

    /// Whether new calls are being refused.
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    /// Dispatch one call. The guard runs before the bridge, always.
    pub async fn call(&self, name: &str, args: Value) -> Result<String, CallFailure> {
        if self.is_shutting_down() {
            return Err(CallFailure::Failed(SHUTTING_DOWN.to_string()));
        }

        let Some(tool) = self.served.iter().find(|t| t.name == name) else {
            tracing::warn!(tool = %log_preview(name), "refused: not on the served list");
            return Err(self.refused(name, Denial::for_reason(Reason::UndeclaredTool)));
        };
        let kind = tool.kind;
        // Only arguments the tool advertises are accepted. Another one could
        // satisfy a workspace policy the operator set (`confirm: true`), or carry
        // work for the argument check to do for nothing.
        if let Some(unknown) = unknown_argument(&tool.schema, &args) {
            return Err(CallFailure::Failed(format!(
                "unknown argument {} for {name}; it accepts: {}",
                log_preview(unknown),
                advertised_arguments(&tool.schema).join(", ")
            )));
        }

        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            return Err(CallFailure::Failed(format!(
                "the server is busy: at most {MAX_CONCURRENT_CALLS} tool calls run at once; retry shortly"
            )));
        };
        // Shutdown may have finished draining between the first check and the
        // permit; a call admitted now would run unsupervised.
        if self.is_shutting_down() {
            return Err(CallFailure::Failed(SHUTTING_DOWN.to_string()));
        }

        let id = format!("mcp-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let scope = Arc::new(CallScope::for_call(
            kind.is_read_only(),
            self.cancel.clone(),
        ));
        self.track(&scope);
        let work = {
            let bridge = self.bridge.clone();
            let guard = self.guard.clone();
            let scope = scope.clone();
            let cancel = self.cancel.clone();
            let name = name.to_string();
            let runtime = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                // Released when the tool really finishes, not when the client
                // stops waiting, so the concurrency bound covers the real work.
                let _permit = permit;
                // The argument check touches the disk. It runs here, under the
                // permit and the client's timeout, never on a runtime worker.
                if let Err(denial) = guard.check_call(&name, kind, &args) {
                    return Work::Refused(denial);
                }
                let dispatch_args = guard.canonical_walk_args(&name, args.clone());
                let written = scope.clone();
                let outcome = runtime.block_on(in_call_scope(scope, async {
                    let stop = async {
                        cancel.cancelled().await;
                        // An edit that has begun writing runs to its end: the file
                        // may already have changed, and the client must learn how.
                        if written.has_started_writing() {
                            std::future::pending::<()>().await;
                        }
                    };
                    tokio::select! {
                        biased;
                        () = stop => None,
                        outcome = bridge.call(&name, dispatch_args, &id) => Some(outcome),
                    }
                }));
                match outcome {
                    None => Work::Stopped,
                    Some(Ok(result)) => Work::Done(Ok(result.prompt_text)),
                    Some(Err(error)) => {
                        // search_replace resolves and confines its own target
                        // before any filesystem call. It can only disagree with
                        // the guard if the path changed since the check: if the
                        // guard now refuses, report that refusal rather than the
                        // tool's text.
                        if !kind.is_read_only()
                            && let Err(denial) = guard.check_call(&name, kind, &args)
                        {
                            return Work::Refused(denial);
                        }
                        let policy_denied = error
                            .details
                            .as_ref()
                            .and_then(|details| details.get("code"))
                            .and_then(Value::as_str)
                            == Some(POLICY_DENIED);
                        Work::Done(Err(ToolFailure {
                            text: error.to_string(),
                            policy_denied,
                        }))
                    }
                }
            })
        };

        let work = match tokio::time::timeout(self.call_timeout, work).await {
            Err(_) => {
                tracing::warn!(
                    tool = name,
                    "tool call timed out; its permit is held until the tool finishes"
                );
                let secs = self.call_timeout.as_secs();
                return Err(CallFailure::Failed(if kind.is_read_only() {
                    format!("the tool call timed out after {secs}s")
                } else {
                    format!(
                        "the tool call timed out after {secs}s; the edit may still be applied, \
                         so read the file before retrying"
                    )
                }));
            }
            Ok(Err(join_error)) => {
                tracing::error!(tool = name, error = %join_error, "tool call task failed");
                return Err(CallFailure::Failed(
                    "the tool call failed unexpectedly".to_string(),
                ));
            }
            Ok(Ok(work)) => work,
        };

        let outcome = match work {
            Work::Refused(denial) => return Err(self.refused(name, denial)),
            Work::Stopped => {
                return Err(CallFailure::Failed(format!(
                    "{SHUTTING_DOWN}; the call was stopped before it changed any file"
                )));
            }
            Work::Done(outcome) => outcome,
        };

        match scope.blocked() {
            Some(Blocked::Refused(reason)) => Err(self.refused(name, Denial::for_reason(reason))),
            Some(Blocked::Document) => Err(CallFailure::Failed(DOCUMENT_REFUSAL.to_string())),
            Some(Blocked::TooLarge) => Err(CallFailure::Failed(TOO_LARGE.to_string())),
            Some(Blocked::NotARegularFile) => Err(CallFailure::Failed(
                "only regular files and directories can be opened".to_string(),
            )),
            Some(Blocked::Stopped) => Err(CallFailure::Failed(format!(
                "{SHUTTING_DOWN}; the call was stopped before it changed any file"
            ))),
            None => match outcome {
                Ok(text) => Ok(text),
                Err(failure) if failure.policy_denied => {
                    // Turbo's workspace policy refused the call. Its text can
                    // quote the policy files, which are always refused; the
                    // operator log keeps it.
                    tracing::warn!(
                        tool = name,
                        detail = %failure.text,
                        "refused by the workspace policy"
                    );
                    Err(self.refused(name, Denial::for_reason(Reason::WorkspacePolicy)))
                }
                Err(failure) => Err(CallFailure::Failed(failure.text)),
            },
        }
    }

    /// Remember a running call's scope, and forget calls that have ended.
    fn track(&self, scope: &Arc<CallScope>) {
        let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
        scopes.retain(|tracked| tracked.strong_count() > 0);
        scopes.push(Arc::downgrade(scope));
    }

    /// Running calls that have begun to write or delete a file.
    pub(crate) fn calls_writing(&self) -> usize {
        self.scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .filter(|scope| scope.has_started_writing())
            .count()
    }

    /// Whether shutdown has cancelled the calls still running.
    #[cfg(test)]
    pub(crate) fn calls_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Names of every skill Turbo's skill tracking holds.
    #[cfg(test)]
    pub(crate) async fn known_skill_names(&self) -> Vec<String> {
        self.bridge
            .slash_skills()
            .await
            .into_iter()
            .map(|skill| skill.name)
            .collect()
    }

    /// Refuse new calls from now on. Synchronous, so a server can stop admitting
    /// calls before it closes connections.
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    /// Refuse new calls, give running ones a bounded window to finish, cancel
    /// what is left, wait however long it takes for an edit that has begun
    /// writing, then stop anything the tools started.
    pub async fn shutdown(&self) {
        self.begin_shutdown();
        let drain_deadline = tokio::time::Instant::now() + self.shutdown_drain;
        while self.in_flight() > 0 && tokio::time::Instant::now() < drain_deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if self.in_flight() > 0 {
            tracing::warn!(
                in_flight = self.in_flight(),
                "cancelling tool calls still running after the drain window"
            );
            self.cancel.cancel();
            let cancel_deadline = tokio::time::Instant::now() + self.cancel_grace;
            while self.in_flight() > 0 && tokio::time::Instant::now() < cancel_deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if self.in_flight() > 0 {
                tracing::error!(
                    in_flight = self.in_flight(),
                    "tool calls did not stop after cancellation"
                );
            }
        }
        // An edit that has begun writing is never cut off, which could leave its
        // file part-written: wait for it however long it takes. No write begins
        // once the calls are cancelled, so none can start after this check. A
        // later stop signal can end the command without waiting.
        if self.calls_writing() > 0 {
            tracing::error!(
                writing = self.calls_writing(),
                "waiting for edits that are still writing a file"
            );
            while self.calls_writing() > 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        self.bridge.kill_foreground_commands().await;
        self.bridge.kill_all_background_tasks().await;
    }
}

/// The first argument in `args` that `schema` does not advertise.
fn unknown_argument<'a>(schema: &Value, args: &'a Value) -> Option<&'a str> {
    let advertised = schema.get("properties").and_then(Value::as_object);
    args.as_object()?
        .keys()
        .map(String::as_str)
        .find(|key| !advertised.is_some_and(|properties| properties.contains_key(*key)))
}

/// The argument names `schema` advertises.
fn advertised_arguments(schema: &Value) -> Vec<&str> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().map(String::as_str).collect())
        .unwrap_or_default()
}
