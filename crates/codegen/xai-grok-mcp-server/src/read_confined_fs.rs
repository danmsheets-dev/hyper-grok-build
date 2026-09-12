//! Filesystem-layer re-check of every read and write against the path the tool
//! actually opens.
//!
//! # Why the pre-dispatch guard is not enough on its own
//!
//! [`PathGuard::check_call`](crate::guard::PathGuard::check_call) judges the
//! argument string. The tools then transform that string before touching the
//! disk: `sanitize_model_path_arg` trims and unquotes it, `try_canonicalize`
//! follows symlinks, and a Unicode filename fallback can swap in a *different*
//! directory entry when the requested one does not exist. An audit showed each
//! of those producing an out-of-root read in the readonly tier with no race.
//!
//! This decorator sits underneath the tools, so it sees the resolved path that
//! is really about to be read or written, and re-runs the same containment and
//! hard-deny rules on it. `read_file` and `search_replace` do all of their file
//! content I/O through `AsyncFileSystem`, so this is the check that cannot be
//! talked around by spelling.
//!
//! The same position makes it the right place for refusals that depend on what
//! is really being opened:
//!
//! - **Documents.** `read_file` hands PDF and PowerPoint bytes to parsers whose
//!   panic containment the release profile's `panic = "abort"` defeats. The
//!   decision uses `read_file`'s own PDF predicate on the resolved path and its
//!   leading bytes, so a symlink or the Unicode fallback cannot route around it.
//! - **Size.** Whole-file reads and writes are capped at
//!   [`MAX_WHOLE_FILE_BYTES`], a line window at [`MAX_LINE_WINDOW_BYTES`], and a
//!   read-only call reads text whole only up to the line-window cap, so neither a
//!   newline-free file nor a large skill file is buffered whole.
//! - **Special files.** Opening a FIFO blocks until a writer appears.
//!
//! It wraps `ConfinedFs` rather than replacing it: writes still go through
//! `ConfinedFs`'s own choke point afterwards. Errors from the layers beneath are
//! reduced to their kind before a tool sees them, because their text can name
//! the approved roots and the resolved target.
//!
//! # Reporting
//!
//! Tools rewrite these errors into their own output ("Permission denied:
//! <path>"), so the toolset cannot tell a refusal from an ordinary failure by
//! reading tool text. Each check that fires records its decision in the running
//! call's [`CallScope`] instead. Every `AsyncFileSystem` call the served tools
//! make targets that call's own resolved path (instruction-file discovery and
//! rule loading use `tokio::fs` directly, and a served edit keeps no receipt),
//! so a recorded decision is always about the call's target.
//!
//! # What it does not cover
//!
//! `grep` runs ripgrep and `list_dir` walks with `std::fs`; neither reads
//! through this trait. They are bounded by the pre-dispatch guard's walk rules,
//! the `DenyReadGlobs` excludes, and, for grep, a per-result filter that applies
//! [`PathGuard::check_resolved`](crate::guard::PathGuard::check_resolved) to
//! every matched file.

use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::io::AsyncBufReadExt;
use xai_grok_tools::computer::types::{AsyncFileSystem, ComputerError};
use xai_grok_tools::implementations::read_file::is_pdf_file;

use crate::guard::{Access, PathGuard, REFUSAL_TEXT, Reason};

/// Largest file read whole into memory or written: images and `search_replace`
/// targets.
pub const MAX_WHOLE_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Largest line window one read returns, and the largest text a read-only call
/// reads whole (skill markdown).
pub const MAX_LINE_WINDOW_BYTES: usize = 8 * 1024 * 1024;

/// Leading bytes examined for a PDF signature: the window `read_file` probes
/// before it chooses a parser.
const DOCUMENT_PROBE_BYTES: usize = 64 * 1024;

/// What the filesystem layer stopped during a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Blocked {
    /// The resolved path failed containment or a hard-deny rule.
    Refused(Reason),
    /// A PDF or PowerPoint file was about to reach a document parser.
    Document,
    /// A whole-file read, a write or a line window over its size limit.
    TooLarge,
    /// A FIFO, socket or device.
    NotARegularFile,
    /// Shutdown stopped the call before it began to write.
    Stopped,
}

#[derive(Debug, Default)]
struct ScopeState {
    blocked: Option<Blocked>,
    /// A write or delete has begun, so the file may already have changed.
    writing: bool,
    wrote: bool,
}

/// What the filesystem layer decided during one call.
#[derive(Debug, Default)]
pub(crate) struct CallScope {
    state: Mutex<ScopeState>,
    /// The call's tool only reads.
    read_only: bool,
    /// Cancelled when shutdown stops running calls. No write begins after it.
    stopped: tokio_util::sync::CancellationToken,
}

impl CallScope {
    /// The scope of one call; `read_only` when its tool only reads, and `stopped`
    /// the token shutdown cancels.
    pub(crate) fn for_call(read_only: bool, stopped: tokio_util::sync::CancellationToken) -> Self {
        Self {
            state: Mutex::default(),
            read_only,
            stopped,
        }
    }

    fn record(&self, blocked: Blocked) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // Once a write has landed, a later document or size refusal (the tool
        // re-reading what it wrote) must not turn the completed edit into a
        // reported failure. A boundary refusal is still recorded.
        if state.wrote && !matches!(blocked, Blocked::Refused(_)) {
            return;
        }
        if state.blocked.is_none() {
            state.blocked = Some(blocked);
        }
    }

    /// Mark that a write or delete begins, unless shutdown stopped the call
    /// before one had. The mark is set under the lock shutdown takes to read it,
    /// so a write either shows up there or never begins.
    fn begin_write(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.stopped.is_cancelled() && !state.writing {
            return false;
        }
        state.writing = true;
        true
    }

    fn mark_written(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).wrote = true;
    }

    /// Whether a write or delete in this call has begun. From then on the file
    /// may have changed, even if the call is stopped.
    pub(crate) fn has_started_writing(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).writing
    }

    pub(crate) fn blocked(&self) -> Option<Blocked> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).blocked
    }
}

tokio::task_local! {
    static CALL_SCOPE: Arc<CallScope>;
}

/// Run `fut` so that the filesystem checks it triggers report to `scope`.
pub(crate) fn in_call_scope<F: std::future::Future>(
    scope: Arc<CallScope>,
    fut: F,
) -> impl std::future::Future<Output = F::Output> {
    CALL_SCOPE.scope(scope, fut)
}

fn call_already_blocked() -> bool {
    CALL_SCOPE
        .try_with(|scope| scope.blocked().is_some())
        .unwrap_or(false)
}

/// Whether the running call's tool only reads.
fn call_is_read_only() -> bool {
    CALL_SCOPE
        .try_with(|scope| scope.read_only)
        .unwrap_or(false)
}

/// Refuse with `message`, recording `blocked` for the running call. Outside a
/// call (toolset construction, direct use in tests) the refusal still stands;
/// there is only nobody to report it to.
fn refuse(blocked: Blocked, message: &str) -> ComputerError {
    let _ = CALL_SCOPE.try_with(|scope| scope.record(blocked));
    ComputerError::io_with_kind(message, std::io::ErrorKind::PermissionDenied)
}

/// Test-only controls over every operation.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct TestHooks {
    /// Every operation waits, yielding, until this holds `true`.
    pub gate: Option<tokio::sync::watch::Receiver<bool>>,
    /// Counts operations that reached the gates.
    pub gate_entered: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Every operation blocks its thread, without yielding, until released.
    pub sync_gate: Option<Arc<SyncGate>>,
    /// The argument check blocks its thread until released.
    pub check_gate: Option<Arc<SyncGate>>,
    /// Building the guard blocks its thread until released.
    pub build_gate: Option<Arc<SyncGate>>,
    /// A write or delete that has begun waits, yielding, until this holds
    /// `true`, before the file is touched.
    pub write_gate: Option<tokio::sync::watch::Receiver<bool>>,
    /// Counts writes and deletes that have begun.
    pub writes_begun: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// A write or delete blocks its thread, without yielding, past its checks
    /// and before it begins, until released.
    pub pre_write_gate: Option<Arc<SyncGate>>,
}

/// A gate that blocks the calling thread, the way synchronous tool work does.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct SyncGate {
    open: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
    /// Threads waiting at the gate.
    waiting: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl SyncGate {
    pub(crate) fn set(&self, open: bool) {
        *self.open.lock().unwrap() = open;
        self.changed.notify_all();
    }

    pub(crate) fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        self.waiting
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
        self.waiting
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Threads waiting at the gate now.
    pub(crate) fn waiting(&self) -> usize {
        self.waiting.load(std::sync::atomic::Ordering::SeqCst)
    }
}

pub struct ReadConfinedFs {
    inner: Arc<dyn AsyncFileSystem>,
    guard: PathGuard,
    #[cfg(test)]
    hooks: TestHooks,
}

impl ReadConfinedFs {
    pub fn new(inner: Arc<dyn AsyncFileSystem>, guard: PathGuard) -> Self {
        Self {
            inner,
            guard,
            #[cfg(test)]
            hooks: TestHooks::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_hooks(mut self, hooks: TestHooks) -> Self {
        self.hooks = hooks;
        self
    }

    async fn before_io(&self) {
        #[cfg(test)]
        {
            if let Some(entered) = &self.hooks.gate_entered {
                entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if let Some(gate) = &self.hooks.sync_gate {
                gate.wait();
            }
            if let Some(gate) = &self.hooks.gate {
                let mut gate = gate.clone();
                let _ = gate.wait_for(|open| *open).await;
            }
        }
    }

    /// Record that the running call is about to change a file. From here the
    /// file may change even if the call is stopped, so shutdown lets it finish. A
    /// call shutdown stopped before this point is refused instead.
    async fn begin_write(&self) -> Result<(), ComputerError> {
        let allowed = CALL_SCOPE
            .try_with(|scope| scope.begin_write())
            .unwrap_or(true);
        if !allowed {
            return Err(refuse(Blocked::Stopped, "the server is shutting down"));
        }
        #[cfg(test)]
        {
            if let Some(begun) = &self.hooks.writes_begun {
                begun.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if let Some(gate) = &self.hooks.write_gate {
                let mut gate = gate.clone();
                let _ = gate.wait_for(|open| *open).await;
            }
        }
        Ok(())
    }

    /// Test hook: a write or delete blocks its thread here, past its checks and
    /// before it begins.
    fn before_write(&self) {
        #[cfg(test)]
        {
            if let Some(gate) = &self.hooks.pre_write_gate {
                gate.wait();
            }
        }
    }

    fn check(&self, path: &Path, access: Access) -> Result<(), ComputerError> {
        self.guard
            .check_resolved(path, access)
            .map_err(|d| refuse(Blocked::Refused(d.reason), &d.to_string()))?;
        if let Ok(metadata) = std::fs::metadata(path)
            && !metadata.is_file()
            && !metadata.is_dir()
        {
            return Err(refuse(Blocked::NotARegularFile, "not a regular file"));
        }
        Ok(())
    }

    fn check_write(&self, path: &Path) -> Result<(), ComputerError> {
        // Something this call tried to read was stopped. `search_replace` takes
        // an unreadable file for an absent one and would recreate it from
        // scratch, overwriting the content it was not allowed to see.
        if call_already_blocked() {
            return Err(ComputerError::io_with_kind(
                "refused: an earlier check in this call failed",
                std::io::ErrorKind::PermissionDenied,
            ));
        }
        // With every root gone, the layer beneath would refuse with a message
        // naming the missing root.
        if !self.guard.roots().iter().any(|root| root.is_dir()) {
            return Err(refuse(Blocked::Refused(Reason::Unresolvable), REFUSAL_TEXT));
        }
        self.check(path, Access::Write)
    }

    fn check_document(path: &Path, head: &[u8]) -> Result<(), ComputerError> {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let head = &head[..head.len().min(DOCUMENT_PROBE_BYTES)];
        if extension == "pptx" || is_pdf_file(head, &extension) {
            return Err(refuse(Blocked::Document, "document formats are not served"));
        }
        Ok(())
    }

    /// Reduce an error from the layers beneath to its kind. Its text can name
    /// the approved roots or the resolved target; the operator log keeps it.
    fn scrub(error: ComputerError, op: &'static str) -> ComputerError {
        let kind = error.io_error_kind().unwrap_or(std::io::ErrorKind::Other);
        tracing::warn!(op, error = %error, "filesystem operation failed");
        ComputerError::io_with_kind(format!("{op} failed ({kind})"), kind)
    }
}

#[async_trait::async_trait]
impl AsyncFileSystem for ReadConfinedFs {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, ComputerError> {
        self.before_io().await;
        self.check(path, Access::Read)?;
        Self::check_document(path, &[])?;
        if let Ok(metadata) = std::fs::metadata(path) {
            if metadata.len() > MAX_WHOLE_FILE_BYTES {
                return Err(refuse(Blocked::TooLarge, "file too large to read whole"));
            }
            // A read-only call reads a file whole to decode an image or to return
            // skill markdown. Text read whole gets no more room than a line
            // window, because every copy the tool makes of it is that large.
            // Whether it is an image is decided the way `read_file` decides.
            if call_is_read_only() && metadata.len() > MAX_LINE_WINDOW_BYTES as u64 {
                let head = self
                    .inner
                    .read_file_prefix(path, DOCUMENT_PROBE_BYTES)
                    .await
                    .map_err(|e| Self::scrub(e, "read"))?;
                let image = xai_grok_tools::implementations::read_file::bytes_to_metadata(&head)
                    .is_ok_and(|metadata| metadata.is_image());
                if !image {
                    return Err(refuse(Blocked::TooLarge, "text too large to read whole"));
                }
            }
        }
        let bytes = self
            .inner
            .read_file(path)
            .await
            .map_err(|e| Self::scrub(e, "read"))?;
        Self::check_document(path, &bytes)?;
        Ok(bytes)
    }

    async fn read_file_prefix(
        &self,
        path: &Path,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ComputerError> {
        self.before_io().await;
        self.check(path, Access::Read)?;
        Self::check_document(path, &[])?;
        let bytes = self
            .inner
            .read_file_prefix(path, max_bytes)
            .await
            .map_err(|e| Self::scrub(e, "read"))?;
        Self::check_document(path, &bytes)?;
        Ok(bytes)
    }

    async fn read_file_line_count(&self, path: &Path) -> Result<usize, ComputerError> {
        self.before_io().await;
        self.check(path, Access::Read)?;
        self.inner
            .read_file_line_count(path)
            .await
            .map_err(|e| Self::scrub(e, "read"))
    }

    async fn read_file_ends_with_newline(&self, path: &Path) -> Result<bool, ComputerError> {
        self.before_io().await;
        self.check(path, Access::Read)?;
        self.inner
            .read_file_ends_with_newline(path)
            .await
            .map_err(|e| Self::scrub(e, "read"))
    }

    /// Lines `start_line..start_line + limit` (1-based), each with its newline,
    /// like `LocalFs::read_file_lines`, but lines before the window are skipped
    /// without being buffered and the window is capped at
    /// [`MAX_LINE_WINDOW_BYTES`].
    async fn read_file_lines(
        &self,
        path: &Path,
        start_line: usize,
        limit: usize,
    ) -> Result<Vec<u8>, ComputerError> {
        self.before_io().await;
        self.check(path, Access::Read)?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|e| Self::scrub(e.into(), "read"))?;
        let mut reader = tokio::io::BufReader::new(file);
        let start = start_line.max(1);
        let end = start.saturating_add(limit);
        let mut current_line = 1usize;
        let mut output = Vec::new();
        while current_line < end {
            let buf = reader
                .fill_buf()
                .await
                .map_err(|e| Self::scrub(e.into(), "read"))?;
            if buf.is_empty() {
                break;
            }
            let (take, line_done) = match buf.iter().position(|&b| b == b'\n') {
                Some(newline) => (newline + 1, true),
                None => (buf.len(), false),
            };
            if current_line >= start {
                if output.len() + take > MAX_LINE_WINDOW_BYTES {
                    return Err(refuse(Blocked::TooLarge, "line window too large"));
                }
                output.extend_from_slice(&buf[..take]);
            }
            reader.consume(take);
            if line_done {
                current_line += 1;
            }
        }
        Ok(output)
    }

    async fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), ComputerError> {
        self.before_io().await;
        self.check_write(path)?;
        if data.len() as u64 > MAX_WHOLE_FILE_BYTES {
            return Err(refuse(Blocked::TooLarge, "file too large to write"));
        }
        self.before_write();
        self.begin_write().await?;
        self.inner
            .write_file(path, data)
            .await
            .map_err(|e| Self::scrub(e, "write"))?;
        let _ = CALL_SCOPE.try_with(|scope| scope.mark_written());
        Ok(())
    }

    async fn delete_file(&self, path: &Path) -> Result<(), ComputerError> {
        self.before_io().await;
        self.check_write(path)?;
        self.before_write();
        self.begin_write().await?;
        self.inner
            .delete_file(path)
            .await
            .map_err(|e| Self::scrub(e, "delete"))?;
        let _ = CALL_SCOPE.try_with(|scope| scope.mark_written());
        Ok(())
    }
}
