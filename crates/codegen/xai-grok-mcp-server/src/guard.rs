//! The containment boundary for externally-served tool calls.
//!
//! # Why this exists
//!
//! `FinalizedToolset::call` runs exactly one gate (`enforce_session_policy` ->
//! `PolicyParams`). Everything that looks like a security layer in this repo —
//! `CompiledPolicy`, `confine_access_outside_root`, `edit_target_protection` —
//! is driven from ACP session setup and is **unreachable** from that call path.
//! `ConfinedFs` guards `write_file`/`delete_file` only. The bash tool
//! self-enforces nothing. On Windows `xai-grok-sandbox` is a compiled-out no-op.
//!
//! So when Turbo's toolset is exposed to a third party over a tunnel, this module
//! and [`crate::read_confined_fs`] *are* the boundary.
//!
//! # Design rules, and the audit findings that produced them
//!
//! 1. **Judge the string the tool will use.** The tools do not act on the raw
//!    argument. They act on `sanitize_model_path_arg(raw)`, which trims Unicode
//!    whitespace and strips quotes. An audit showed `<root>/grok.toml"` and a
//!    trailing U+00A0 passing every name-based rule while the tool opened the
//!    trimmed path. [`admit`] therefore refuses any argument the sanitiser would
//!    change, and the filesystem layer re-checks the resolved path as well.
//!
//! 2. **Admit before touching the disk.** [`admit`] rejects every spelling we are
//!    not prepared to reason about before any filesystem call: relative paths,
//!    UNC and verbatim `\\?\`, `..` and `.` segments, empty segments and
//!    trailing separators, NTFS alternate data streams, reserved DOS device
//!    names, trailing dots or spaces, control and invisible formatting
//!    characters. A path spelled outside every root is refused before any
//!    filesystem call too, so how long a refusal takes says nothing about what
//!    exists outside the roots.
//!
//! 3. **Fail closed everywhere.** Empty roots deny; an unresolvable path denies;
//!    an unknown tool denies; a malformed argument denies.
//!
//! 4. **Walks are not reads.** `grep` and `list_dir` recurse. A rule keyed on the
//!    *named* path cannot stop a walk from reaching `.git/config` two levels
//!    down, so walk targets get their own rules here, the toolset rewrites each
//!    walk target to its canonical spelling, and grep drops every result from a
//!    file this guard refuses. A grep with no named target walks the first root
//!    and is checked as such.
//!
//! 5. **Denials are opaque.** One message crosses the wire for every cause.
//!    Detail goes to the operator through `tracing` and the toolset's observer.
//!
//! 6. **Borrow the hardened predicate, then tighten it.**
//!    [`path_is_under_confine_root`] canonicalizes both sides, fails closed on
//!    unresolvable paths and compares existing components in their on-disk
//!    spelling. Where neither side exists it still ignores letter case if the
//!    directory they share is case-insensitive. Containment here also requires
//!    each root's spelling as captured at startup, so a root removed later is
//!    matched only as it was spelled.
//!
//! 7. **Find git metadata by content.** Git opens any directory holding `HEAD`,
//!    `objects` and `refs` as a repository, whatever it is called. A bare clone
//!    named `.bare` or `mirror.git` has hooks and a credential-bearing config
//!    just as `.git` does, so the git rules follow the content, not the name.
//!
//! 8. **Refuse over-broad roots.** A filesystem root, any user's home directory,
//!    or a directory containing one would expose every other tool's credential
//!    files at once. Homes are compared by file identity, so aliases do not
//!    slip through, and a home that looking at could mount is compared by its
//!    path, so the check never mounts one.
//!
//! 9. **Edit-tier refusals are best effort.** Writes to files Turbo, git or
//!    common tools run with no prompt are refused by name (the name a client
//!    uses, and the name of any link found leading to a file) and, where
//!    configuration names them (plugin roots, the git hooks directory, files an
//!    `.envrc` sources), by location; a relative location counts wherever Turbo
//!    could run, and so does what a link on the way to one leads to. Ordinary
//!    source files are still code.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use serde_json::Value;
use xai_grok_tools::types::compat::CompatConfig;
use xai_grok_tools::types::resources::{
    canonicalize_for_permission, path_is_under_confine_root, sanitize_model_path_arg,
};
use xai_grok_tools::types::tool::ToolKind;

/// Shared root list. Deliberately **not** the process-wide
/// `PROCESS_CONFINE_ROOTS` `OnceLock`: that is stamped by `apply_process_confine`
/// before the subcommand match and silently discards a second write.
type RootsInner = Arc<RwLock<Vec<PathBuf>>>;

/// Longest path spelling admitted, in bytes.
const MAX_PATH_BYTES: usize = 4096;
/// Most separator-delimited segments admitted in one path.
const MAX_PATH_SEGMENTS: usize = 256;

/// What a call wants to do with a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read one file.
    Read,
    /// Create, modify, or delete one file.
    Write,
    /// Recursively list or search a directory (`grep`, `list_dir`).
    Walk,
}

/// Why a call was refused, for the operator only. Never sent to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    NoRoots,
    /// A root that is a filesystem root or is or contains a home directory.
    OverBroadRoot,
    Inadmissible,
    OutsideRoots,
    Unresolvable,
    HardDenied,
    SymlinkComponent,
    /// A walk target that is a FIFO, socket or device.
    SpecialFile,
    UndeclaredTool,
    UndeclaredProperty,
    ReadOnlyServer,
    MalformedArgument,
    /// Turbo's workspace policy (`grok.toml`, `.grok/policy.toml`) refused it.
    WorkspacePolicy,
    /// More locations are declared than the edit tier can check a write against.
    TooManyDeclarations,
}

/// The exact text of every refusal the client sees.
pub const REFUSAL_TEXT: &str =
    "refused: the request is not permitted by this server's configuration";

/// The single refusal an untrusted caller ever observes.
///
/// Identical for every cause. Distinguishable denials are a filesystem oracle:
/// they tell a caller whether a path exists and where the home directory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("refused: the request is not permitted by this server's configuration")]
pub struct Denial {
    /// Operator-side detail. Never rendered into the wire message.
    pub reason: Reason,
}

impl Denial {
    fn new(reason: Reason) -> Self {
        Self { reason }
    }

    /// Construct a denial for a refusal decided outside [`PathGuard`].
    pub fn for_reason(reason: Reason) -> Self {
        Self::new(reason)
    }
}

/// A bounded, escaped rendering of client-supplied text for logs and the
/// operator's terminal: control characters escaped, at most 120 characters.
pub fn log_preview(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{head:?}... ({} bytes)", text.len())
    } else {
        format!("{head:?}")
    }
}

/// Properties known **not** to carry a filesystem path.
///
/// [`validate_tool_schema`] uses this to be exhaustive, and the sweep uses it to
/// skip free text. `glob` is here because it is a pattern, not a path; it is
/// validated separately by [`PathGuard::check_call`].
fn is_known_path_free(property: &str) -> bool {
    matches!(
        property,
        "offset"
            | "limit"
            | "pages"
            | "query"
            | "pattern"
            | "glob"
            | "case_sensitive"
            | "is_regex"
            | "explanation"
            | "recursive"
            | "max_results"
            | "max_output_chars"
            | "instructions"
            | "old_string"
            | "new_string"
            | "replace_all"
            | "skip_read_before_edit"
            | "empty_old_string_does_not_override"
            | "contents"
            | "content"
            | "output_mode"
            | "head_limit"
            | "multiline"
            | "format"
            | "type"
            | "-A"
            | "-B"
            | "-C"
            | "-i"
    )
}

#[derive(Debug, Clone, Copy)]
pub struct PathField {
    pub name: &'static str,
    pub multi: bool,
    pub access: Access,
}

const fn read(name: &'static str) -> PathField {
    PathField {
        name,
        multi: false,
        access: Access::Read,
    }
}

const fn write(name: &'static str) -> PathField {
    PathField {
        name,
        multi: false,
        access: Access::Write,
    }
}

const fn walk(name: &'static str) -> PathField {
    PathField {
        name,
        multi: false,
        access: Access::Walk,
    }
}

// Wire names verified against the live advertised schemas (see the drift guard
// in toolset_tests), not copied from memory.
const F_READ_FILE: &[PathField] = &[read("target_file"), read("path"), read("file_path")];
const F_LIST_DIR: &[PathField] = &[walk("target_directory"), walk("path")];
const F_GREP: &[PathField] = &[walk("path")];
const F_GLOB: &[PathField] = &[walk("path")];
const F_WRITE_TARGET: &[PathField] = &[write("file_path"), write("target_file"), write("path")];

/// The servable surface. Every entry is a promise that its path arguments are
/// fully enumerated. `run_terminal_cmd` and every other shell surface is absent
/// by design: a shell command's operands cannot be enumerated.
pub fn declared_path_fields(tool: &str) -> Option<&'static [PathField]> {
    let bare = tool.rsplit(':').next().unwrap_or(tool);
    Some(match bare {
        "read_file" => F_READ_FILE,
        "list_dir" => F_LIST_DIR,
        "grep" | "grep_search" => F_GREP,
        "glob" | "file_search" => F_GLOB,
        "search_replace" => F_WRITE_TARGET,
        _ => return None,
    })
}

/// Startup gate: every property the tool actually advertises must be declared
/// as a path or known to be path-free. The server calls this for every tool
/// before serving it and drops any tool that fails.
pub fn validate_tool_schema(tool: &str, input_schema: &Value) -> Result<(), Denial> {
    let Some(fields) = declared_path_fields(tool) else {
        tracing::warn!(tool, "refusing to serve: no path-argument declaration");
        return Err(Denial::new(Reason::UndeclaredTool));
    };
    let Some(props) = input_schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };
    for name in props.keys() {
        if !fields.iter().any(|f| f.name == name) && !is_known_path_free(name) {
            tracing::warn!(tool, property = ?name, "refusing to serve: undeclared property");
            return Err(Denial::new(Reason::UndeclaredProperty));
        }
    }
    Ok(())
}

/// Reserved DOS device names, including the console and clock aliases and the
/// superscript-digit port names Windows also treats as devices.
const DOS_DEVICES: &[&str] = &[
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "CONIN$",
    "CONOUT$",
    "CLOCK$",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
];

/// Zero-width, bidirectional and other invisible formatting characters. They
/// make a path look different to the operator reading a log than it is.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// Whether a path spelling is one we are prepared to reason about. Runs before
/// any filesystem call.
pub fn admit(raw: &str) -> Result<(), Denial> {
    let Some(what) = inadmissible(raw) else {
        return Ok(());
    };
    if raw.len() > MAX_PATH_BYTES {
        tracing::warn!(bytes = raw.len(), "refusing an over-long path spelling");
    } else {
        tracing::warn!(
            path = %log_preview(raw),
            reason = what,
            "refusing inadmissible path spelling"
        );
    }
    Err(Denial::new(Reason::Inadmissible))
}

/// Why [`admit`] refuses `raw`, or `None` when it does not.
fn inadmissible(raw: &str) -> Option<&'static str> {
    if raw.is_empty() {
        return Some("empty");
    }
    if raw.len() > MAX_PATH_BYTES {
        return Some("over-long path");
    }
    if raw.chars().any(char::is_control) {
        return Some("control character");
    }
    if raw.chars().any(is_invisible_format) {
        return Some("invisible formatting character");
    }
    // The tools resolve `sanitize_model_path_arg(raw)`, not `raw`. If the two
    // differ, every name-based rule below would judge a different string from
    // the one that gets opened.
    if sanitize_model_path_arg(raw) != raw {
        return Some("surrounding whitespace or quotes");
    }
    if raw.contains(['"', '\'']) {
        return Some("quote character");
    }
    if raw.replace('\\', "/").starts_with("//") {
        return Some("UNC or verbatim path");
    }

    let p = Path::new(raw);
    // Also refuses `~` spellings, which are relative.
    if !p.is_absolute() {
        return Some("relative path");
    }

    // `Path::components` silently drops `.` segments, repeated separators and a
    // trailing separator, but a walker builds child paths from the spelling it
    // was given: a walk of `<root>/.aws/.` yields `.aws/./credentials`, which no
    // name-based exclude matches. Refuse every such spelling outright.
    let segments: Vec<&str> = raw.split(['/', '\\']).collect();
    if segments.len() > MAX_PATH_SEGMENTS {
        return Some("too many path segments");
    }
    for (index, segment) in segments.iter().enumerate() {
        if *segment == "." {
            return Some("dot segment");
        }
        if segment.is_empty() && index != 0 {
            return Some("empty segment or trailing separator");
        }
    }

    for comp in p.components() {
        let Component::Normal(os) = comp else {
            if matches!(comp, Component::ParentDir) {
                return Some("parent-dir segment");
            }
            continue;
        };
        let Some(s) = os.to_str() else {
            return Some("non-UTF-8 component");
        };
        if s.contains(':') {
            return Some("alternate data stream");
        }
        if s.ends_with('.') || s.ends_with(' ') {
            return Some("trailing dot or space");
        }
        // Windows drops the extension, then trailing spaces, before matching a
        // device name, so `CON .txt` opens the console.
        let stem = s
            .split('.')
            .next()
            .unwrap_or(s)
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        if DOS_DEVICES.contains(&stem.as_str()) {
            return Some("reserved device name");
        }
    }
    None
}

/// A grep `glob` is a filename filter, but ripgrep resolves it against the
/// search root, so an absolute, home-relative or `..` pattern could reach
/// outside it. Name-based exclusions are handled by the deny globs, which the
/// tool appends after the caller's glob so they win.
fn validate_glob(glob: &str) -> Result<(), Denial> {
    if glob.is_empty() {
        return Ok(());
    }
    let bad = glob.chars().any(char::is_control)
        || glob.starts_with('/')
        || glob.starts_with('\\')
        || glob.starts_with('~')
        || glob.contains(':')
        || glob.split(['/', '\\']).any(|seg| seg == "..");
    if bad {
        tracing::warn!(
            glob = %log_preview(glob),
            "refusing grep glob that could leave the search root"
        );
        return Err(Denial::new(Reason::Inadmissible));
    }
    Ok(())
}

/// Most bytes a search pattern or file type may hold. ripgrep takes both as
/// command-line arguments, and Windows caps a whole command line at about
/// 32,000 characters.
const MAX_SEARCH_ARGUMENT_BYTES: usize = 8 * 1024;

/// A grep `pattern` or `type` reaches ripgrep as a command-line argument, which
/// can hold no NUL on any platform and cannot be arbitrarily long. Refused here
/// rather than at the spawn, whose failure is about the operator's installation
/// and named where ripgrep is kept.
fn validate_search_argument(name: &str, text: &str) -> Result<(), Denial> {
    if text.len() > MAX_SEARCH_ARGUMENT_BYTES || text.contains('\0') {
        tracing::warn!(
            argument = name,
            bytes = text.len(),
            "refusing a search argument ripgrep could not be given"
        );
        return Err(Denial::new(Reason::MalformedArgument));
    }
    Ok(())
}

fn normalize_spaces(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '\u{00A0}' || c == '\u{202F}' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Most directory entries examined for the Unicode filename fallback. A missing
/// name in a larger directory is refused rather than scanned on every call.
pub(crate) const MAX_SIBLING_SCAN: usize = 4096;

/// Directory entries the tools' Unicode filename fallback could substitute for
/// a non-existent `path`: siblings whose name matches after mapping U+00A0 and
/// U+202F to a space. `None` when the directory is too large to scan.
fn unicode_siblings(path: &Path) -> Option<Vec<PathBuf>> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return Some(Vec::new());
    };
    if !name.contains([' ', '\u{00A0}', '\u{202F}']) {
        return Some(Vec::new());
    }
    let want = normalize_spaces(name);
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Some(Vec::new());
    };
    let mut siblings = Vec::new();
    for (index, entry) in entries.flatten().enumerate() {
        if index >= MAX_SIBLING_SCAN {
            return None;
        }
        let entry_name = entry.file_name();
        if let Some(s) = entry_name.to_str()
            && s != name
            && normalize_spaces(s) == want
        {
            siblings.push(entry.path());
        }
    }
    Some(siblings)
}

fn is_dotenv(name: &str) -> bool {
    name == ".env"
        || (name.starts_with(".env.")
            && !matches!(
                name,
                ".env.example" | ".env.sample" | ".env.template" | ".env.dist"
            ))
}

/// Remove `.` and resolve `..` without touching the disk.
pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Whether `path` starts with `prefix`, component by component; letter case is
/// ignored on Windows, where the filesystem usually ignores it too.
fn starts_with_components(path: &Path, prefix: &Path) -> bool {
    let mut remaining = path.components();
    prefix.components().all(|want| {
        remaining.next().is_some_and(|got| {
            if cfg!(windows) {
                got.as_os_str().to_string_lossy().to_lowercase()
                    == want.as_os_str().to_string_lossy().to_lowercase()
            } else {
                got == want
            }
        })
    })
}

/// Whether `root` is too broad to approve: a filesystem or share root, a
/// Windows drive mounted under WSL, or a directory that is or contains one of
/// `homes`. The root is compared with every ancestor of every home by directory
/// identity, so a symlink, junction, bind mount or other spelling of one is
/// recognised. A home also counts as contained when its real path lies under
/// the root's, or when it is reachable below the root under part of its own
/// path.
pub fn is_over_broad_root(root: &Path, homes: &[PathBuf]) -> bool {
    is_over_broad_root_with(root, homes, &Automounts::default())
}

/// [`is_over_broad_root`] where a home that looking at could mount (see
/// [`Automounts::could_mount`]) is judged by its path alone.
pub(crate) fn is_over_broad_root_with(
    root: &Path,
    homes: &[PathBuf],
    automounts: &Automounts,
) -> bool {
    if !root.is_absolute() {
        // Rejected on its own terms when the guard is built.
        return false;
    }
    let canon = canonicalize_for_permission(root).display;
    if canon.parent().is_none() || is_wsl_drive_mount(&canon) {
        return true;
    }
    // A root whose identity cannot be read (one that cannot be opened, on
    // Windows) is still judged by its path.
    let same_as_an_ancestor = DirIdentity::of(root).is_some_and(|root_id| {
        homes
            .iter()
            .flat_map(|home| home.ancestors())
            .collect::<BTreeSet<&Path>>()
            .into_iter()
            .filter(|ancestor| !automounts.could_mount(ancestor))
            .any(|ancestor| DirIdentity::of(ancestor).as_ref() == Some(&root_id))
    });
    same_as_an_ancestor
        || homes.iter().any(|home| {
            if automounts.could_mount(home) {
                home.starts_with(&canon) || home.starts_with(root)
            } else {
                canonicalize_for_permission(home)
                    .display
                    .starts_with(&canon)
                    || home_reached_below(root, home)
            }
        })
}

/// Automount triggers nothing is mounted on yet, from `/proc/self/mountinfo`.
/// Below one, looking a name up can mount a filesystem, whatever the lookup
/// asks, so the over-broad root check judges such a path without touching it.
#[derive(Debug, Default, Clone)]
pub(crate) struct Automounts {
    /// Mount points of `autofs` mounts with no later mount at the same point.
    pub points: Vec<PathBuf>,
    /// Mount points of other filesystems below one of `points`: the keys of an
    /// indirect map that are mounted, and so can be looked at.
    pub mounted: Vec<PathBuf>,
}

impl Automounts {
    /// Whether looking `path` up could mount a filesystem: it lies below an
    /// automount point, as spelled or once the links above it are followed, and
    /// no mount below that point holds it.
    pub(crate) fn could_mount(&self, path: &Path) -> bool {
        if self.points.is_empty() {
            return false;
        }
        self.below_a_point(path)
            || spelled_through_links(path, &self.points)
                .is_some_and(|real| self.below_a_point(&real))
    }

    /// [`Self::could_mount`] for a path already spelled as mountinfo spells it.
    fn below_a_point(&self, path: &Path) -> bool {
        self.points.iter().any(|point| {
            path != point.as_path()
                && path.starts_with(point)
                && !self
                    .mounted
                    .iter()
                    .any(|mount| mount.starts_with(point) && path.starts_with(mount))
        })
    }
}

/// How many links [`spelled_through_links`] follows before giving up.
const MAX_LINK_HOPS: usize = 40;

/// `path` with the links in the folders above it followed, which is how the
/// kernel reaches it; `None` when no link was followed. A name below one of
/// `points` is never looked at: looking it up is what would mount a filesystem.
fn spelled_through_links(path: &Path, points: &[PathBuf]) -> Option<PathBuf> {
    let mut real = PathBuf::new();
    let mut rest: Vec<std::ffi::OsString> = path
        .components()
        .rev()
        .map(|component| component.as_os_str().to_os_string())
        .collect();
    let mut followed = 0;
    while let Some(name) = rest.pop() {
        let kind = Path::new(&name).components().next();
        match kind {
            Some(Component::CurDir) => continue,
            Some(Component::ParentDir) => {
                real.pop();
                continue;
            }
            _ => real.push(&name),
        }
        // The last name is the path itself, which is never looked up here, and
        // neither is a name below a point: the rest counts as spelled.
        if rest.is_empty() || points.iter().any(|point| real.starts_with(point)) {
            break;
        }
        if !matches!(kind, Some(Component::Normal(_))) || followed == MAX_LINK_HOPS {
            continue;
        }
        let is_link = std::fs::symlink_metadata(&real).is_ok_and(|m| m.file_type().is_symlink());
        let Some(target) = is_link.then(|| std::fs::read_link(&real).ok()).flatten() else {
            continue;
        };
        followed += 1;
        real.pop();
        rest.extend(
            target
                .components()
                .rev()
                .map(|component| component.as_os_str().to_os_string()),
        );
    }
    for name in rest.into_iter().rev() {
        real.push(name);
    }
    (followed > 0).then(|| lexical_normalize(&real))
}

/// Whether `root` is a folder directly inside one of `crowded`, folders holding
/// too many accounts' homes to list one by one, and so is taken for a home.
pub(crate) fn is_in_a_crowded_folder(root: &Path, crowded: &[PathBuf]) -> bool {
    if !root.is_absolute() {
        return false;
    }
    let canon = canonicalize_for_permission(root).display;
    let Some(parent) = canon.parent() else {
        return false;
    };
    let parent_id = DirIdentity::of(parent);
    crowded.iter().any(|folder| {
        canonicalize_for_permission(folder).display == parent
            || (parent_id.is_some() && DirIdentity::of(folder) == parent_id)
    })
}
/// Whether `home` is reachable below `root` under a different path: through a
/// second path to the volume that holds it (`/System/Volumes/Data/Users/<u>` on
/// macOS), or a bind mount, junction or link to a folder above it.
fn home_reached_below(root: &Path, home: &Path) -> bool {
    DirIdentity::of(home).is_some_and(|home_id| reached_below(root, home, &home_id))
}

/// [`home_reached_below`] for a home whose identity is already known.
fn reached_below(root: &Path, home: &Path, home_id: &DirIdentity) -> bool {
    let names: Vec<&std::ffi::OsStr> = home
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    (0..names.len()).any(|skip| {
        let candidate = names[skip..]
            .iter()
            .fold(root.to_path_buf(), |path, name| path.join(name));
        candidate != home && DirIdentity::of(&candidate).as_ref() == Some(home_id)
    })
}

/// A directory's identity. On Unix it comes from metadata, without opening the
/// directory, so probing a name can neither block on a FIFO nor, on Linux, mount
/// an automounted home.
#[derive(PartialEq, Eq)]
struct DirIdentity(#[cfg(unix)] (u64, u64), #[cfg(not(unix))] same_file::Handle);

impl DirIdentity {
    fn of(path: &Path) -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            let (device, inode, is_dir) = stat_without_automount(path)?;
            is_dir.then_some(Self((device, inode)))
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(path).ok()?;
            metadata
                .is_dir()
                .then(|| Self((metadata.dev(), metadata.ino())))
        }
        #[cfg(not(unix))]
        {
            if !std::fs::metadata(path).ok()?.is_dir() {
                return None;
            }
            // A directory, so opening it cannot block.
            same_file::Handle::from_path(path).ok().map(Self)
        }
    }
}

/// Whether `path` is an existing directory, checked the way [`DirIdentity`]
/// reads one.
fn is_existing_dir(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        stat_without_automount(path).is_some_and(|(_, _, is_dir)| is_dir)
    }
    #[cfg(not(target_os = "linux"))]
    {
        path.is_dir()
    }
}

/// `stat` of `path`, following links, that leaves an automount point unmounted:
/// its device, its inode and whether it is a directory. `std::fs::metadata`
/// uses `statx` without `AT_NO_AUTOMOUNT`, which mounts an automounted home.
#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_cast)] // `dev_t` and `ino_t` differ between targets.
fn stat_without_automount(path: &Path) -> Option<(u64, u64, bool)> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `name` is a NUL-terminated path, and `stat` points to writable
    // memory of the right size, which a successful call fills.
    let status = unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_NO_AUTOMOUNT,
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: the call succeeded, so it filled `stat`.
    let stat = unsafe { stat.assume_init() };
    Some((
        stat.st_dev as u64,
        stat.st_ino as u64,
        (stat.st_mode & libc::S_IFMT) == libc::S_IFDIR,
    ))
}

/// How much of a Windows drive a mount in `/proc/self/mountinfo` exposes.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowsMount {
    /// A whole drive (`C:\`). It counts as a home, and so does every profile in
    /// its `Users` folder.
    Drive,
    /// A drive's `Users` folder. Every profile inside it counts as a home.
    Profiles,
    /// One profile (`C:\Users\<name>`). It counts as a home.
    Profile,
}

/// Mounts of Windows drives, of their `Users` folders and of single profiles,
/// listed in `/proc/self/mountinfo` text, wherever WSL or a bind mount put them
/// (`[automount] root` can put `C:` at `/c`). A mount of any other Windows
/// folder is an ordinary directory. A line that is not UTF-8 is skipped, not
/// taken to end the list.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_windows_mounts(mountinfo: &[u8]) -> Vec<(PathBuf, WindowsMount)> {
    mountinfo
        .split(|byte| *byte == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .filter_map(|line| {
            let (mount, filesystem) = line.split_once(" - ")?;
            let mut mount_fields = mount.split(' ');
            let subtree = unescape_mountinfo(mount_fields.nth(3)?);
            let mount_point = unescape_mountinfo(mount_fields.next()?);
            // The superblock options come last and are printed unescaped, so a
            // `path=` holding a space runs past the next space.
            let mut fields = filesystem.splitn(3, ' ');
            let fstype = fields.next()?;
            let source = unescape_mountinfo(fields.next().unwrap_or_default());
            let options: Vec<&str> = fields
                .next()
                .unwrap_or_default()
                .split([',', ';'])
                .collect();
            let windows = fstype == "drvfs"
                || options.contains(&"aname=drvfs")
                || (matches!(fstype, "9p" | "virtiofs") && starts_with_drive(&source));
            if !windows {
                return None;
            }
            // drvfs names the Windows folder it mounts in `path=`; otherwise the
            // source does. The mount shows the `subtree` of that folder.
            let folder = options
                .iter()
                .find_map(|option| option.strip_prefix("path="))
                // Printed raw: a folder named `1234` is not an escape.
                .map(str::to_string)
                .unwrap_or(source);
            let location = format!("{folder}/{subtree}").replace('/', "\\");
            let parts: Vec<&str> = location
                .split('\\')
                .filter(|part| !part.is_empty())
                .collect();
            let kind = match parts.as_slice() {
                [drive] if is_drive(drive) => WindowsMount::Drive,
                [drive, users] if is_drive(drive) && users.eq_ignore_ascii_case("users") => {
                    WindowsMount::Profiles
                }
                [drive, users, _] if is_drive(drive) && users.eq_ignore_ascii_case("users") => {
                    WindowsMount::Profile
                }
                _ => return None,
            };
            Some((PathBuf::from(mount_point), kind))
        })
        .collect()
}

/// Whether `text` starts with a drive letter and a colon.
#[cfg(any(target_os = "linux", test))]
fn starts_with_drive(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    )
}

/// Whether `text` is a bare drive, such as `C:`.
#[cfg(any(target_os = "linux", test))]
fn is_drive(text: &str) -> bool {
    text.len() == 2 && starts_with_drive(text)
}

/// One line of `/proc/self/mountinfo`.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MountEntry {
    /// `major:minor` of the mounted filesystem.
    pub device: String,
    /// The folder of that filesystem the mount shows. NFS prints `/` for every
    /// mount, so for it this is the folder its source names instead.
    pub root: PathBuf,
    /// Where it is mounted.
    pub mount_point: PathBuf,
    /// The filesystem type, such as `ext4` or `autofs`.
    pub fstype: String,
}

/// Every mount listed in `/proc/self/mountinfo` text, in order. A line that is
/// not UTF-8 is skipped.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_mounts(mountinfo: &[u8]) -> Vec<MountEntry> {
    mountinfo
        .split(|byte| *byte == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .filter_map(|line| {
            let (mount, filesystem) = line.split_once(" - ")?;
            let mut fields = mount.split(' ');
            let device = fields.nth(2)?;
            let root = fields.next()?;
            let mount_point = fields.next()?;
            let mut filesystem = filesystem.split(' ');
            let fstype = filesystem.next().unwrap_or_default();
            let source = unescape_mountinfo(filesystem.next().unwrap_or_default());
            let root = PathBuf::from(unescape_mountinfo(root));
            // NFS prints `/` as every mount's root, so two mounts of one
            // server's filesystem would look like mounts of the same folder.
            // The source names the folder the export starts at; the root field
            // still says which subtree of it a bind mount shows.
            let root = match nfs_export_folder(fstype, &source) {
                Some(exported) => {
                    PathBuf::from(exported).join(root.strip_prefix("/").unwrap_or(&root))
                }
                None => root,
            };
            Some(MountEntry {
                device: device.to_string(),
                root,
                mount_point: PathBuf::from(unescape_mountinfo(mount_point)),
                fstype: fstype.to_string(),
            })
        })
        .collect()
}

/// The folder an NFS mount's source (`server:/path`, `[address]:/path`) names,
/// or `None` for another filesystem. NFS prints `/` as every mount's root, so
/// without this two mounts of one server's filesystem look like mounts of the
/// same folder, and every folder in one is taken for a folder in the other.
#[cfg(any(target_os = "linux", test))]
fn nfs_export_folder(fstype: &str, source: &str) -> Option<String> {
    if !fstype.starts_with("nfs") {
        return None;
    }
    let exported = match source.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once("]:")?.1,
        None => source.split_once(':')?.1,
    };
    exported.starts_with('/').then(|| exported.to_string())
}

/// The automount triggers in `mounts` with nothing mounted on them yet, and the
/// mounts below them (see [`Automounts`]).
#[cfg(any(target_os = "linux", test))]
pub(crate) fn unmounted_automounts(mounts: &[MountEntry]) -> Automounts {
    let points: Vec<PathBuf> = mounts
        .iter()
        .enumerate()
        .filter(|(index, mount)| {
            mount.fstype == "autofs"
                && !mounts[index + 1..]
                    .iter()
                    .any(|later| later.mount_point == mount.mount_point)
        })
        .map(|(_, mount)| mount.mount_point.clone())
        .collect();
    let mounted = mounts
        .iter()
        .filter(|mount| mount.fstype != "autofs")
        .filter(|mount| {
            points
                .iter()
                .any(|point| mount.mount_point != *point && mount.mount_point.starts_with(point))
        })
        .map(|mount| mount.mount_point.clone())
        .collect();
    Automounts { points, mounted }
}

/// Homes shown by a mount of one folder directly inside `crowded`, the real
/// path of a folder holding too many homes to list: a bind mount of a single
/// account's home, as an SFTP chroot sets up, is that home under another name.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn crowded_folder_homes(crowded: &Path, mounts: &[MountEntry]) -> Vec<PathBuf> {
    let Some(holder) = holding_mount(crowded, mounts) else {
        return Vec::new();
    };
    let Ok(rest) = crowded.strip_prefix(&holder.mount_point) else {
        return Vec::new();
    };
    let within = holder.root.join(rest);
    mounts
        .iter()
        .filter(|mount| mount.device == holder.device && !std::ptr::eq(*mount, holder))
        .filter(|mount| {
            mount
                .root
                .strip_prefix(&within)
                .is_ok_and(|below| below.components().count() == 1)
        })
        .map(|mount| mount.mount_point.clone())
        .collect()
}

/// The mount that holds `path`: the longest mount point at or above it, and of
/// several at the same point, the last mounted.
#[cfg(any(target_os = "linux", test))]
fn holding_mount<'a>(path: &Path, mounts: &'a [MountEntry]) -> Option<&'a MountEntry> {
    mounts
        .iter()
        .enumerate()
        .filter(|(_, mount)| path.starts_with(&mount.mount_point))
        .max_by_key(|(index, mount)| (mount.mount_point.components().count(), *index))
        .map(|(_, mount)| mount)
}

/// Other paths at which `home`, a real path, is reachable because the
/// filesystem holding it is mounted again: a bind mount of the home or of a
/// folder above it under any name, or a second mount of the same filesystem or
/// subvolume. Worked out from mountinfo alone, without touching any mount.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn home_aliases(home: &Path, mounts: &[MountEntry]) -> Vec<PathBuf> {
    let Some(holder) = holding_mount(home, mounts) else {
        return Vec::new();
    };
    let Ok(rest) = home.strip_prefix(&holder.mount_point) else {
        return Vec::new();
    };
    let within = holder.root.join(rest);
    mounts
        .iter()
        .filter(|mount| mount.device == holder.device && !std::ptr::eq(*mount, holder))
        .filter_map(|mount| {
            let below = within.strip_prefix(&mount.root).ok()?;
            let alias = mount.mount_point.join(below);
            (alias != home).then_some(alias)
        })
        .collect()
}
/// Undo the octal escapes `/proc/self/mountinfo` uses for a space, tab,
/// newline or backslash in a path.
#[cfg(any(target_os = "linux", test))]
fn unescape_mountinfo(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut rest = field;
    while let Some(index) = rest.find('\\') {
        out.push_str(&rest[..index]);
        let byte = rest
            .get(index + 1..index + 4)
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        match byte {
            Some(byte) => {
                out.push(char::from(byte));
                rest = &rest[index + 4..];
            }
            None => {
                out.push('\\');
                rest = &rest[index + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Most folders in one place taken for accounts' homes. Past this, the folder
/// holding them counts as one home instead: it, every folder above it and every
/// folder directly inside it are still refused, but the accounts inside it are
/// not listed one by one.
pub(crate) const MAX_ACCOUNT_FOLDERS: usize = 256;

/// The directories directly inside `dir`, taken for accounts' homes. Past
/// [`MAX_ACCOUNT_FOLDERS`], `dir` itself is taken instead and recorded in
/// `crowded`. Entries are typed from the listing itself, so only a link is
/// looked through.
fn subdirectories(dir: &Path, crowded: &mut Vec<PathBuf>) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut folders = Vec::new();
    for entry in entries.flatten() {
        let is_dir = match entry.file_type() {
            Ok(kind) if kind.is_symlink() => is_existing_dir(&entry.path()),
            Ok(kind) => kind.is_dir(),
            Err(_) => false,
        };
        if !is_dir {
            continue;
        }
        if folders.len() == MAX_ACCOUNT_FOLDERS {
            tracing::warn!(
                folder = ?dir,
                "too many folders to take each for a home; taking the folder itself"
            );
            crowded.push(dir.to_path_buf());
            return vec![dir.to_path_buf()];
        }
        folders.push(entry.path());
    }
    folders
}

/// `/mnt/<drive letter>`, where WSL mounts a whole Windows drive.
fn is_wsl_drive_mount(path: &Path) -> bool {
    let parts: Vec<Component<'_>> = path.components().collect();
    matches!(
        parts.as_slice(),
        [Component::RootDir, Component::Normal(mnt), Component::Normal(drive)]
            if *mnt == "mnt"
                && drive.to_str().is_some_and(|d| d.len() == 1 && d.chars().all(|c| c.is_ascii_alphabetic()))
    )
}

/// Folders beside `home`, taken for other accounts' homes (`C:\Users\*`,
/// `/home/*`, `/Users/*`). None when `home` sits at the top level, as `/root`
/// does in a container, where its neighbours are `/usr` and project folders.
#[cfg(test)]
pub(crate) fn folders_beside_home(home: &Path) -> Vec<PathBuf> {
    folders_beside_home_into(home, &mut Vec::new())
}

/// The folders beside `home` (see `folders_beside_home`), recording a folder
/// too crowded to list in `crowded`.
fn folders_beside_home_into(home: &Path, crowded: &mut Vec<PathBuf>) -> Vec<PathBuf> {
    match home.parent().filter(|dir| dir.parent().is_some()) {
        Some(profiles_dir) => subdirectories(profiles_dir, crowded),
        None => Vec::new(),
    }
}

/// Whether `path` starts with a Windows verbatim prefix, which [`admit`]
/// refuses in every client path.
fn has_verbatim_prefix(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix)) if prefix.kind().is_verbatim()
    )
}

/// Strings examined in one call's arguments. A call with more is refused
/// instead of checked at length.
pub(crate) const MAX_ARGUMENT_STRINGS: usize = 64;

/// Account homes, with every folder holding more than [`MAX_ACCOUNT_FOLDERS`]
/// of them counted once, as a home, and recorded in `crowded`, instead of
/// account by account. Homes in smaller folders count only if `present` says
/// they are there.
#[cfg(any(unix, test))]
pub(crate) fn cap_account_homes(
    accounts: Vec<PathBuf>,
    present: impl Fn(&Path) -> bool,
    crowded: &mut Vec<PathBuf>,
) -> Vec<PathBuf> {
    let mut by_folder: std::collections::BTreeMap<PathBuf, Vec<PathBuf>> =
        std::collections::BTreeMap::new();
    for home in accounts {
        let folder = home.parent().map(Path::to_path_buf).unwrap_or_default();
        by_folder.entry(folder).or_default().push(home);
    }
    let mut out = Vec::new();
    for (folder, homes) in by_folder {
        if homes.len() > MAX_ACCOUNT_FOLDERS {
            crowded.push(folder.clone());
            out.push(folder);
        } else {
            out.extend(homes.into_iter().filter(|home| present(home)));
        }
    }
    out
}

/// The homes of the login accounts in `/etc/passwd` text: user ID 0 or at least
/// 1000, and not `/`. Read as bytes, so a line that is not UTF-8 hides no other.
#[cfg(any(unix, test))]
pub(crate) fn passwd_homes(passwd: &[u8]) -> Vec<PathBuf> {
    passwd
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let fields: Vec<&[u8]> = line.split(|byte| *byte == b':').collect();
            let [_, _, uid, _, _, home, ..] = fields.as_slice() else {
                return None;
            };
            let uid = std::str::from_utf8(uid).ok()?.parse::<u32>().ok()?;
            let home = path_from_bytes(home);
            let login = uid == 0 || uid >= 1000;
            (login && !home.as_os_str().is_empty() && home != Path::new("/")).then_some(home)
        })
        .collect()
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(all(test, not(unix)))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// What the over-broad root check compares a root with.
#[derive(Debug, Default)]
pub(crate) struct CandidateHomes {
    /// Home directories a root may not be or contain.
    pub homes: Vec<PathBuf>,
    /// Folders holding too many accounts' homes to list. A folder directly
    /// inside one is taken for a home too.
    pub crowded: Vec<PathBuf>,
    /// Automount triggers nothing is mounted on yet. A home below one is judged
    /// by its path alone.
    pub automounts: Automounts,
}

/// Home directories a root may not be or contain: the current home and every
/// other account's home this process can see.
pub fn candidate_homes() -> Vec<PathBuf> {
    find_candidate_homes().homes
}

/// [`candidate_homes`], with the folders too crowded to list and the automount
/// triggers left alone.
pub(crate) fn find_candidate_homes() -> CandidateHomes {
    let mut found = CandidateHomes::default();
    #[cfg(target_os = "linux")]
    let mountinfo = std::fs::read("/proc/self/mountinfo").unwrap_or_default();
    #[cfg(target_os = "linux")]
    let mounts = parse_mounts(&mountinfo);
    #[cfg(target_os = "linux")]
    {
        found.automounts = unmounted_automounts(&mounts);
    }
    if let Some(home) = dirs::home_dir() {
        let beside = folders_beside_home_into(&home, &mut found.crowded);
        found.homes.extend(beside);
        found.homes.push(home);
    }
    #[cfg(unix)]
    {
        // Every login account, so another user's home, or the operator's own
        // under a different $HOME, is refused too. One that looking at could
        // mount counts without being looked at.
        if let Ok(passwd) = std::fs::read("/etc/passwd") {
            let automounts = found.automounts.clone();
            let present = |home: &Path| automounts.could_mount(home) || is_existing_dir(home);
            let accounts = cap_account_homes(passwd_homes(&passwd), present, &mut found.crowded);
            found.homes.extend(accounts);
        }
    }
    #[cfg(target_os = "linux")]
    {
        // WSL can mount a Windows drive, its profiles or one profile anywhere.
        for (mount, kind) in parse_windows_mounts(&mountinfo) {
            match kind {
                WindowsMount::Drive => {
                    let profiles = subdirectories(&mount.join("Users"), &mut found.crowded);
                    found.homes.extend(profiles);
                    found.homes.push(mount);
                }
                WindowsMount::Profiles => {
                    let profiles = subdirectories(&mount, &mut found.crowded);
                    found.homes.extend(profiles);
                }
                WindowsMount::Profile => found.homes.push(mount),
            }
        }
        let automounts = &found.automounts;
        let real = |path: &PathBuf| {
            if automounts.could_mount(path) {
                path.clone()
            } else {
                canonicalize_for_permission(path).display
            }
        };
        // A home is also reachable wherever the filesystem that holds it is
        // mounted again, under any name.
        let real_homes: Vec<PathBuf> = found.homes.iter().map(real).collect();
        let real_crowded: Vec<PathBuf> = found.crowded.iter().map(real).collect();
        for home in real_homes {
            found.homes.extend(home_aliases(&home, &mounts));
        }
        // So is a folder too crowded to list, and a mount of one home inside it
        // is that home.
        for folder in real_crowded {
            let aliases = home_aliases(&folder, &mounts);
            found.homes.extend(aliases.iter().cloned());
            found.crowded.extend(aliases);
            found.homes.extend(crowded_folder_homes(&folder, &mounts));
        }
    }
    #[cfg(target_os = "macos")]
    {
        // macOS lists its accounts outside /etc/passwd too, and its data volume
        // holds every home under a second path.
        let users = subdirectories(Path::new("/Users"), &mut found.crowded);
        found.homes.extend(users);
        found.homes.push(PathBuf::from("/System/Volumes/Data"));
    }
    found.homes.sort();
    found.homes.dedup();
    found.crowded.sort();
    found.crowded.dedup();
    found
}
/// Grok homes the guard refuses for every access: the configured `$GROK_HOME`
/// and the default `~/.grok`.
pub fn grok_homes() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Some(home) = xai_grok_config::user_grok_home() {
        homes.push(home);
    }
    homes.push(xai_grok_config::default_grok_home());
    homes
}

/// A directory git would open as a repository.
fn is_git_dir(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

/// Git metadata rules for `path`, applied under every directory named `.git`
/// and every directory git would open as a repository (rule 7).
fn git_metadata_reason(path: &Path, access: Access) -> Option<&'static str> {
    git_metadata_reason_with(path, access, &mut is_git_dir)
}

/// [`git_metadata_reason`], asking `is_repository` whether a folder is one git
/// would open.
fn git_metadata_reason_with(
    path: &Path,
    access: Access,
    is_repository: &mut dyn FnMut(&Path) -> bool,
) -> Option<&'static str> {
    for dir in path.ancestors() {
        let named_git = dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(".git"));
        if !named_git && !is_repository(dir) {
            continue;
        }
        // Writing anywhere in git metadata is execution by proxy (hooks, and
        // `core.hooksPath` in config). Walking it reaches config.
        if access != Access::Read {
            return Some("git metadata");
        }
        let Ok(rel) = path.strip_prefix(dir) else {
            continue;
        };
        let rel: Vec<String> = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().to_ascii_lowercase()),
                _ => None,
            })
            .collect();
        if rel.first().map(String::as_str) == Some("hooks") {
            return Some("git hooks");
        }
        // Remote URLs in any git config can carry credentials: the repository
        // config, submodule configs and worktree configs.
        if matches!(
            rel.last().map(String::as_str),
            Some("config" | "config.worktree")
        ) {
            return Some("git config");
        }
    }
    None
}

/// Directories a walk may not target: credential stores.
const WALK_DENIED_DIRS: &[&str] = &[".aws", ".docker", ".gnupg", ".kube", ".ssh"];

/// File names refused for every access: other tools' credentials.
const CREDENTIAL_NAMES: &[&str] = &[
    ".npmrc",
    ".pypirc",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "id_rsa",
];

/// Directories whose contents Turbo, another agent, an editor, a plugin loader
/// or a git hook manager loads or runs with no action from the operator. The
/// edit tier refuses writes anywhere inside them. `.grok` is refused for every
/// access.
const AUTO_RUN_DIRS: &[&str] = &[
    ".agents",
    ".claude",
    ".claude-plugin",
    ".cursor",
    ".git-hooks",
    ".githooks",
    ".grok-plugin",
    ".hooks",
    ".husky",
    ".idea",
    ".vscode",
];

/// File names, compared case-insensitively, with the same property. `HEAD` is
/// here because writing one next to `objects` and `refs` turns a directory
/// into a repository whose config git would honour.
const AUTO_RUN_NAMES: &[&str] = &[
    ".cursorrules",
    ".gitignore",
    ".ignore",
    ".lsp.json",
    ".mcp.json",
    ".pre-commit-config.yaml",
    ".rgignore",
    "agent.md",
    "agents.md",
    "claude.local.md",
    "claude.md",
    "extension.wasm",
    "head",
    "plugin.json",
];

/// lefthook's configuration and local override, in every format it reads.
pub(crate) fn is_lefthook_config(name: &str) -> bool {
    let name = name.strip_prefix('.').unwrap_or(name);
    let Some(rest) = name.strip_prefix("lefthook") else {
        return false;
    };
    let rest = rest.strip_prefix("-local").unwrap_or(rest);
    matches!(rest, ".yml" | ".yaml" | ".toml" | ".json" | ".jsonc")
}

struct AutoRun {
    dirs: BTreeSet<String>,
    names: BTreeSet<String>,
}

/// The lists above plus everything Turbo's own instruction, rules and skill
/// discovery reads, taken from `CompatConfig`, so a name added there is refused
/// here without an edit.
static AUTO_RUN: LazyLock<AutoRun> = LazyLock::new(|| {
    let compat = CompatConfig::default();
    let mut dirs: BTreeSet<String> = AUTO_RUN_DIRS.iter().map(|d| (*d).to_string()).collect();
    let mut names: BTreeSet<String> = AUTO_RUN_NAMES.iter().map(|n| (*n).to_string()).collect();
    for entry in compat.agent_filenames() {
        match entry.split_once('/') {
            Some((dir, _)) => dirs.insert(dir.to_ascii_lowercase()),
            None => names.insert(entry.to_ascii_lowercase()),
        };
    }
    for entry in compat
        .rules_dirs()
        .into_iter()
        .chain(compat.skill_config_dirs())
    {
        let dir = entry.split('/').next().unwrap_or(entry);
        dirs.insert(dir.to_ascii_lowercase());
    }
    AutoRun { dirs, names }
});

/// For every kind of repository content folder trust gates on (the `hit!`
/// kinds in `xai-grok-workspace`'s `folder_trust.rs`), sample paths that create
/// it. Each is code or configuration Turbo runs in an already-trusted folder
/// with no prompt, so the edit tier must refuse to write every one. A test keeps
/// this table in step with folder trust and with the write rules.
pub const FOLDER_TRUST_MARKER_SAMPLES: &[(&str, &[&str])] = &[
    (
        "mcp",
        &[".mcp.json", ".cursor/mcp.json", ".grok/config.toml"],
    ),
    (
        "plugins",
        &[
            ".grok/config.toml",
            ".grok/plugins/p/plugin.json",
            ".claude/plugins/p/plugin.json",
        ],
    ),
    ("permission", &[".grok/config.toml"]),
    ("lsp", &[".grok/lsp.json"]),
    ("cursor-rules", &[".cursor/rules/r.mdc"]),
    ("envrc", &[".envrc"]),
    (
        "claude",
        &[".claude/settings.json", ".claude/settings.local.json"],
    ),
    ("hooks", &[".grok/hooks/h.json", ".cursor/hooks.json"]),
    ("agents", &[".grok/agents/a.md", ".claude/agents/a.md"]),
    (
        "skills",
        &[
            ".grok/skills/s/SKILL.md",
            ".agents/skills/s/SKILL.md",
            ".claude/skills/s/SKILL.md",
            ".cursor/skills/s/SKILL.md",
            ".grok/commands/c.md",
            ".agents/commands/c.md",
            ".claude/commands/c.md",
            ".cursor/commands/c.md",
        ],
    ),
    ("roles", &[".grok/roles/r.md"]),
    ("personas", &[".grok/personas/p.md"]),
    ("workflows", &[".grok/workflows/w.md"]),
];

/// Homes a `~` in a configuration file can stand for: the profile folder,
/// `%USERPROFILE%` (which Turbo's skills loader tries first, on every platform),
/// and on Windows also the folders Git for Windows uses when they are set,
/// `%HOME%` or else `%HOMEDRIVE%%HOMEPATH%`.
fn tilde_homes() -> Vec<PathBuf> {
    #[cfg(test)]
    if let Some(homes) = test_homes() {
        return homes;
    }
    #[cfg(windows)]
    let mut homes = tilde_homes_from(
        dirs::home_dir(),
        std::env::var_os("HOME"),
        std::env::var_os("HOMEDRIVE"),
        std::env::var_os("HOMEPATH"),
    );
    // `dirs` already honours `$HOME` here.
    #[cfg(not(windows))]
    let mut homes: Vec<PathBuf> = dirs::home_dir().into_iter().collect();
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from)
        && profile.is_absolute()
        && !homes.contains(&profile)
    {
        homes.push(profile);
    }
    homes
}

#[cfg(test)]
thread_local! {
    /// The homes a guard built on this thread should see (see
    /// [`testing::with_test_homes`]).
    static TEST_HOMES: std::cell::RefCell<Option<Vec<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn test_homes() -> Option<Vec<PathBuf>> {
    TEST_HOMES.with(|homes| homes.borrow().clone())
}

/// [`tilde_homes`] from its inputs. Every absolute candidate counts: reading a
/// git configuration git does not use only refuses more.
#[cfg(any(windows, test))]
pub(crate) fn tilde_homes_from(
    profile: Option<PathBuf>,
    home: Option<std::ffi::OsString>,
    home_drive: Option<std::ffi::OsString>,
    home_path: Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    let drive_home = home_drive.zip(home_path).map(|(mut drive, path)| {
        drive.push(path);
        PathBuf::from(drive)
    });
    let mut homes: Vec<PathBuf> = Vec::new();
    for candidate in [profile, home.map(PathBuf::from), drive_home]
        .into_iter()
        .flatten()
        .filter(|candidate| candidate.is_absolute())
    {
        if !homes.contains(&candidate) {
            homes.push(candidate);
        }
    }
    homes
}

/// Resolve a path written in a configuration file in `base`: `~/` against each
/// of [`tilde_homes`], a relative path against `base`.
fn resolve_declared(base: &Path, raw: &str) -> Vec<PathBuf> {
    resolve_declared_from(&[base.to_path_buf()], raw)
}

/// [`resolve_declared`] where a relative path could be relative to any of
/// `bases`.
fn resolve_declared_from(bases: &[PathBuf], raw: &str) -> Vec<PathBuf> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return tilde_homes()
            .iter()
            .map(|home| lexical_normalize(&home.join(rest)))
            .collect();
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        vec![lexical_normalize(path)]
    } else {
        bases
            .iter()
            .map(|base| lexical_normalize(&base.join(path)))
            .collect()
    }
}

/// Most bytes of one configuration file read when the guard is built.
const MAX_DECLARATION_BYTES: u64 = 1024 * 1024;

/// A configuration file read when the guard is built. Only a regular file (a
/// link to one is followed) of at most [`MAX_DECLARATION_BYTES`] is read, so a
/// link to a device or a FIFO in a repository can neither exhaust nor hang
/// startup. bash and git read their files as bytes, so a byte that is not UTF-8
/// is replaced rather than taken to hide the whole file.
fn read_declaration(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    if metadata.len() > MAX_DECLARATION_BYTES {
        tracing::warn!(
            file = ?path,
            "configuration file too large to read for declared locations"
        );
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_DECLARATION_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// `(section, key, value)` from a git config file, section and key lowercased.
/// A subsection (`[includeIf "gitdir:..."]`) keeps only its section name.
pub(crate) fn read_git_config(path: &Path) -> Vec<(String, String, String)> {
    match read_declaration(path) {
        Some(text) => parse_git_config(&text),
        None => Vec::new(),
    }
}

/// [`read_git_config`] over one file's text, read the way git reads it: a key
/// can follow its section header on the same line, a value ends at an unquoted
/// `#` or `;`, quotes and backslash escapes are resolved, and a backslash
/// before a line break joins the next line onto the value.
pub(crate) fn parse_git_config(text: &str) -> Vec<(String, String, String)> {
    let mut entries = Vec::new();
    let mut section = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            _ if c.is_whitespace() => {}
            '#' | ';' => skip_to_line_end(&mut chars),
            '[' => section = git_config_section(&mut chars),
            // A key starts with a letter; git refuses a line that does not.
            _ if c.is_ascii_alphabetic() => {
                let mut key = String::from(c);
                while let Some(c) = chars.next_if(|c| c.is_ascii_alphanumeric() || *c == '-') {
                    key.push(c);
                }
                while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
                if chars.next_if_eq(&'=').is_some() {
                    entries.push((
                        section.clone(),
                        key.to_ascii_lowercase(),
                        git_config_value(&mut chars),
                    ));
                } else {
                    // A key on its own is a boolean, and names no location.
                    skip_to_line_end(&mut chars);
                }
            }
            _ => skip_to_line_end(&mut chars),
        }
    }
    entries
}

/// The section a header names, lowercased and without its subsection, read up
/// to the `]` that closes it. A `]` inside the quoted subsection of, say,
/// `[includeIf "gitdir:~/work/[ab]/"]` does not close it.
fn git_config_section(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut header = String::new();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '\n' => break,
            '\\' if quoted => {
                chars.next();
            }
            '"' => quoted = !quoted,
            ']' if !quoted => break,
            _ => header.push(c),
        }
    }
    header
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// One value, as git reads it.
fn git_config_value(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut value = String::new();
    // How much of `value` ends in something other than the unquoted whitespace
    // git discards at both ends.
    let mut kept = 0;
    let mut quoted = false;
    while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
    while let Some(c) = chars.next() {
        match c {
            '\n' => break,
            '\r' if chars.peek() == Some(&'\n') => {}
            '"' => quoted = !quoted,
            '\\' => {
                match chars.next() {
                    // A line break after a backslash continues the value.
                    Some('\n') => {}
                    Some('\r') => {
                        chars.next_if_eq(&'\n');
                    }
                    Some('n') => value.push('\n'),
                    Some('t') => value.push('\t'),
                    // Git's own escape for a backspace character, not an
                    // instruction to drop the character before it.
                    Some('b') => value.push('\u{8}'),
                    Some(escaped) => value.push(escaped),
                    None => {}
                }
                kept = value.len();
            }
            '#' | ';' if !quoted => {
                skip_to_line_end(chars);
                break;
            }
            ' ' | '\t' if !quoted => value.push(c),
            _ => {
                value.push(c);
                kept = value.len();
            }
        }
    }
    value.truncate(kept);
    value
}

/// Step past the rest of a line.
fn skip_to_line_end(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for c in chars.by_ref() {
        if c == '\n' {
            break;
        }
    }
}

/// A repository git runs hooks for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GitRepository {
    /// A worktree, named by its top: the folder holding `.git`.
    Worktree(PathBuf),
    /// A bare repository, named by the git directory itself.
    Bare(PathBuf),
}

impl GitRepository {
    /// The folder git resolves a relative `core.hooksPath` against, which is
    /// the folder it runs the hooks in.
    fn base(&self) -> &Path {
        match self {
            Self::Worktree(top) => top,
            Self::Bare(git_dir) => git_dir,
        }
    }

    /// The repository's git directory.
    fn git_dir(&self) -> Option<PathBuf> {
        match self {
            Self::Worktree(top) => git_dir_of(top),
            Self::Bare(git_dir) => Some(git_dir.clone()),
        }
    }
}

/// The git directory of a worktree at `root`, following a `.git` file.
fn git_dir_of(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = read_declaration(&dot_git)?;
    let target = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?;
    resolve_declared(root, target).into_iter().next()
}

/// The worktree holding `path`: the nearest folder at or above it with a `.git`
/// folder or file. On Unix, where a filesystem boundary can be seen cheaply, the
/// search stops at one as git does, unless `GIT_DISCOVERY_ACROSS_FILESYSTEM`
/// says otherwise; that also keeps it from looking a name up inside an
/// automount point above the root.
fn enclosing_worktree(path: &Path) -> Option<GitRepository> {
    #[cfg(unix)]
    let device = {
        use std::os::unix::fs::MetadataExt;
        let across = std::env::var_os("GIT_DISCOVERY_ACROSS_FILESYSTEM")
            .is_some_and(|value| git_config_true(&value.to_string_lossy()));
        (!across)
            .then(|| std::fs::metadata(path).ok().map(|metadata| metadata.dev()))
            .flatten()
    };
    for dir in path.ancestors() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Some(device) = device
                && std::fs::metadata(dir).ok().map(|m| m.dev()) != Some(device)
            {
                return None;
            }
        }
        if is_worktree_top(dir) {
            return Some(GitRepository::Worktree(dir.to_path_buf()));
        }
    }
    None
}

/// Whether git would open the folder holding `dir/.git` as a worktree: a
/// `.git` directory it would open as a repository, or a `.git` file naming one.
/// A stray `.git` that is neither does not stop git's own upward search, so it
/// must not stop this one either.
fn is_worktree_top(dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    if dot_git.is_dir() {
        return is_git_dir(&dot_git);
    }
    git_dir_of(dir).is_some()
}

/// Whether a git configuration value means true.
#[cfg(unix)]
fn git_config_true(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// The folders git takes hooks from for the repository whose git directory is
/// `git_dir`: its own `hooks`, and a linked worktree's common directory's.
fn hooks_folders(git_dir: &Path) -> Vec<PathBuf> {
    let mut folders = vec![git_dir.join("hooks")];
    if let Some(common) = read_declaration(&git_dir.join("commondir")) {
        folders.extend(
            resolve_declared(git_dir, common.trim())
                .into_iter()
                .map(|common| common.join("hooks")),
        );
    }
    folders
}
/// How many levels of `include.path` git follows.
const MAX_GIT_INCLUDE_DEPTH: usize = 10;

/// Locations git runs or reads with no prompt for `repositories`: every hooks
/// folder, the folders `core.hooksPath` names, and every file `include.path` or
/// `includeIf.*.path` pulls in. Read from each repository's configuration, the
/// user's and the system's, and from whatever those include. `includeIf`
/// conditions are not evaluated: every one counts. What a link in a hooks
/// folder leads to counts too.
fn git_locations(repositories: &[GitRepository]) -> Vec<PathBuf> {
    let configs = global_git_configs();
    let global = git_config_locations(configs.clone());
    let mut out = global.includes;
    // A global configuration file that lies inside a root is as writable as the
    // files it includes; only paths inside a root survive the prefix check.
    out.extend(configs);
    let bases: Vec<PathBuf> = repositories
        .iter()
        .map(|repository| repository.base().to_path_buf())
        .collect();
    let mut hooks = Vec::new();
    // An absolute hooks path in the global configuration counts even where
    // there is no repository under a root at all.
    for raw in &global.hooks_paths {
        hooks.extend(resolve_declared_from(&[], raw));
    }
    for repository in repositories {
        let mut configs = Vec::new();
        if let Some(git_dir) = repository.git_dir() {
            // A linked worktree keeps its shared config in the common directory.
            if let Some(common) = read_declaration(&git_dir.join("commondir")) {
                for common_dir in resolve_declared(&git_dir, common.trim()) {
                    configs.push(common_dir.join("config"));
                }
            }
            configs.push(git_dir.join("config"));
            configs.push(git_dir.join("config.worktree"));
            hooks.extend(hooks_folders(&git_dir));
        }
        let repository_configs = git_config_locations(configs);
        out.extend(repository_configs.includes);
        // A relative hooks path counts from the folder git runs the hooks in.
        for raw in global
            .hooks_paths
            .iter()
            .chain(&repository_configs.hooks_paths)
        {
            hooks.extend(resolve_declared(repository.base(), raw));
        }
    }
    for folder in hooks {
        // An empty or `.` hooks path resolves to the folder git runs in. Git
        // runs no hook from a worktree top, and refusing it would refuse every
        // write in the worktree.
        if bases.iter().any(|base| base.starts_with(&folder)) {
            tracing::debug!(folder = ?folder, "ignoring a hooks path that names a worktree");
            continue;
        }
        out.extend(folder_links(&folder));
        out.push(folder);
    }
    out
}

/// What a set of git configuration files names.
#[derive(Default)]
struct GitConfigLocations {
    /// `core.hooksPath` values, as written: a relative one counts from the
    /// folder git runs the hooks in, which differs per repository.
    hooks_paths: Vec<String>,
    /// Files pulled in by `include.path` or `includeIf.*.path`.
    includes: Vec<PathBuf>,
}

/// What `configs`, and everything they include to [`MAX_GIT_INCLUDE_DEPTH`]
/// levels, name. Each file is read once, so a cycle ends.
fn git_config_locations(configs: Vec<PathBuf>) -> GitConfigLocations {
    let mut found = GitConfigLocations::default();
    let mut seen = BTreeSet::new();
    let mut pending: Vec<(PathBuf, usize)> =
        configs.into_iter().map(|config| (config, 0)).collect();
    while let Some((config, depth)) = pending.pop() {
        if depth > MAX_GIT_INCLUDE_DEPTH || !seen.insert(lexical_normalize(&config)) {
            continue;
        }
        // A relative include is relative to the file that includes it.
        let config_dir = config.parent().map(Path::to_path_buf).unwrap_or_default();
        for (section, key, value) in read_git_config(&config) {
            match (section.as_str(), key.as_str()) {
                ("core", "hookspath") => found.hooks_paths.push(value),
                ("include" | "includeif", "path") => {
                    for included in resolve_declared(&config_dir, &value) {
                        found.includes.push(included.clone());
                        pending.push((included, depth + 1));
                    }
                }
                _ => {}
            }
        }
    }
    found
}

/// The git configuration files git reads whatever repository it runs in.
fn global_git_configs() -> Vec<PathBuf> {
    let mut configs = Vec::new();
    if let Some(global) = std::env::var_os("GIT_CONFIG_GLOBAL") {
        configs.push(PathBuf::from(global));
    }
    let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    for home in tilde_homes() {
        configs.push(home.join(".gitconfig"));
        let xdg = xdg_config.clone().unwrap_or_else(|| home.join(".config"));
        configs.push(xdg.join("git").join("config"));
    }
    if let Some(system) = std::env::var_os("GIT_CONFIG_SYSTEM") {
        configs.push(PathBuf::from(system));
    }
    if cfg!(unix) {
        configs.push(PathBuf::from("/etc/gitconfig"));
    }
    if cfg!(windows) {
        for base in ["ProgramFiles", "ProgramData"] {
            if let Some(dir) = std::env::var_os(base) {
                let dir = PathBuf::from(dir).join("Git");
                configs.push(dir.join("etc").join("gitconfig"));
                configs.push(dir.join("config"));
            }
        }
    }
    configs
}
/// Most locations the edit tier keeps from configuration. Past this the guard
/// refuses to serve that tier instead of checking every write against a list
/// without bound.
pub(crate) const MAX_DECLARED_LOCATIONS: usize = 4096;

/// Locations configuration declares: absolute ones, and ones relative to the
/// folder Turbo runs in. A location declared twice is kept once.
#[derive(Debug, Default)]
pub(crate) struct Declared {
    pub paths: Vec<PathBuf>,
    pub relative: Vec<RelativeDeclaration>,
    /// Set once more than [`MAX_DECLARED_LOCATIONS`] distinct locations were
    /// declared; the ones past that are not kept.
    pub overflowed: bool,
    seen_paths: BTreeSet<PathBuf>,
    /// Keyed on the names as spelled, not as compared: two spellings of one
    /// location match the same paths, but a link along either is followed to
    /// where that spelling leads.
    seen_relative: BTreeSet<(usize, Option<PathBuf>, Vec<String>)>,
}

impl Declared {
    fn len(&self) -> usize {
        self.paths.len() + self.relative.len()
    }

    fn push_path(&mut self, path: PathBuf) {
        if !self.seen_paths.insert(path.clone()) {
            return;
        }
        if self.len() >= MAX_DECLARED_LOCATIONS {
            self.overflowed = true;
            return;
        }
        self.paths.push(path);
    }

    fn push_relative(&mut self, declaration: RelativeDeclaration) {
        let key = (
            declaration.up,
            declaration.anchor.clone(),
            declaration.spelled.clone(),
        );
        if !self.seen_relative.insert(key) {
            return;
        }
        if self.len() >= MAX_DECLARED_LOCATIONS {
            self.overflowed = true;
            return;
        }
        self.relative.push(declaration);
    }

    fn extend(&mut self, other: Declared) {
        self.overflowed |= other.overflowed;
        for path in other.paths {
            self.push_path(path);
        }
        for relative in other.relative {
            self.push_relative(relative);
        }
    }
}

/// A location a configuration file names relative to the folder Turbo runs in.
/// Turbo can run in any folder, so it names every folder reached by `names`
/// from `up` levels above a folder Turbo could run in: one at or below the
/// folder of a project configuration, or any folder at all for a file Turbo
/// reads wherever it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelativeDeclaration {
    /// Leading `..` steps.
    pub up: usize,
    /// The names that follow, lowercase.
    pub names: Vec<String>,
    /// The same names as the configuration spells them, for joining onto a path.
    pub spelled: Vec<String>,
    /// The highest folder those names can start in: `up` levels above the
    /// folder of a project configuration, canonical and in compare form. `None`
    /// for a file Turbo reads wherever it runs.
    pub anchor: Option<PathBuf>,
    /// How many names `anchor` holds, worked out once when this is built.
    pub floor: usize,
}

impl RelativeDeclaration {
    /// A location `names` names, `up` levels above a folder Turbo could run in:
    /// one at or below `base`, the canonical compare form of the folder holding
    /// the configuration that declares it, or any folder when it is `None`.
    pub(crate) fn new(base: Option<&Path>, up: usize, spelled: Vec<String>) -> Self {
        let anchor = base.map(|base| {
            base.ancestors()
                .nth(up)
                .unwrap_or(Path::new(""))
                .to_path_buf()
        });
        let floor = anchor
            .as_deref()
            .map_or(0, |anchor| lowercase_names(anchor).len());
        Self {
            up,
            names: spelled.iter().map(|name| name.to_lowercase()).collect(),
            spelled,
            anchor,
            floor,
        }
    }

    /// Whether `compare`, a canonical path in compare form whose names are
    /// `path_names` (see [`lowercase_names`]), is this location or lies inside
    /// it, for some folder Turbo could run in.
    pub(crate) fn covers(&self, compare: &Path, path_names: &[String]) -> bool {
        let Some(floor) = self.start_floor(compare) else {
            return false;
        };
        let Some(last_start) = path_names.len().checked_sub(self.names.len()) else {
            return false;
        };
        (floor..=last_start)
            .any(|start| path_names[start..start + self.names.len()] == self.names[..])
    }

    /// The names still to follow after a path that is the first `taken` names
    /// of this location, reached from a folder Turbo could run in; `None` when
    /// it is not. A link at such a path is one a loader follows on its way here.
    fn rest_after(&self, compare: &Path, path_names: &[String], taken: usize) -> Option<&[String]> {
        let floor = self.start_floor(compare)?;
        let start = path_names.len().checked_sub(taken)?;
        (start >= floor && path_names[start..] == self.names[..taken])
            .then(|| &self.spelled[taken..])
    }

    /// Where a match can start in `compare`; `None` when that path is outside
    /// every folder this location can name.
    fn start_floor(&self, compare: &Path) -> Option<usize> {
        match &self.anchor {
            None => Some(0),
            Some(anchor) => compare.starts_with(anchor).then_some(self.floor),
        }
    }
}

/// The names in `path`, lowercase.
pub(crate) fn lowercase_names(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect()
}

/// Record where a location written in a configuration file can be.
///
/// Turbo expands `$VAR` and `${VAR}` first. An absolute entry names one place.
/// A relative one counts from the folder Turbo runs in (see
/// [`RelativeDeclaration`]); `base` is the canonical compare form of a project
/// configuration's folder, and `None` for a file Turbo reads wherever it runs.
/// A loader that does not expand `~/` takes it for a folder named `~`, so such
/// an entry counts that way, and also against every home a loader could expand
/// it to. A variable can hold something else when Turbo reads the file, so the
/// names after the last one count from anywhere too.
fn declare(raw: &str, base: Option<&Path>, out: &mut Declared) {
    let expanded = xai_grok_config::expand_env_vars_in_string(raw.trim());
    declare_expanded(raw, expanded.trim(), base, out);
}

/// [`declare`] with the expansion already worked out, so a test can pass what a
/// variable that is set but empty expands to.
pub(crate) fn declare_expanded(raw: &str, expanded: &str, base: Option<&Path>, out: &mut Declared) {
    let raw = raw.trim();
    if raw.is_empty() {
        return;
    }
    let parts: Vec<&str> = raw.split(['/', '\\']).collect();
    if let Some(last_variable) = parts.iter().rposition(|part| part.contains('$')) {
        let tail = parts[last_variable + 1..].join("/");
        if tail.split('/').any(|part| !matches!(part, "" | "." | "..")) {
            declare_relative(&tail, None, out);
        }
    }
    // A variable that is set but empty names nothing, and Turbo loads nothing
    // from it. Recording it would leave a location of no names, which is every
    // location.
    if expanded.is_empty() {
        return;
    }
    let after_tilde = if expanded == "~" {
        Some("")
    } else {
        expanded
            .strip_prefix("~/")
            .or_else(|| expanded.strip_prefix("~\\"))
    };
    if let Some(rest) = after_tilde {
        for home in tilde_homes() {
            out.push_path(lexical_normalize(&home.join(rest)));
        }
        declare_relative(expanded, base, out);
    } else if Path::new(expanded).is_absolute() {
        out.push_path(lexical_normalize(Path::new(expanded)));
    } else {
        declare_relative(expanded, base, out);
    }
}

/// Record `text`, a relative location, keeping the `..` steps it starts with
/// once collapsed.
fn declare_relative(text: &str, base: Option<&Path>, out: &mut Declared) {
    let mut names: Vec<String> = Vec::new();
    let mut up = 0;
    for part in text.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                if names.pop().is_none() {
                    up += 1;
                }
            }
            name => names.push(name.to_string()),
        }
    }
    out.push_relative(RelativeDeclaration::new(base, up, names));
}

/// The configuration lists that name plugin and skill folders.
const DECLARED_LISTS: &[(&str, &str)] = &[
    ("plugins", "paths"),
    ("skills", "paths"),
    ("skills", "server_skill_dirs"),
    ("skills", "bundled_skill_dirs"),
];

/// The configuration files Turbo reads whatever folder it runs in.
const GLOBAL_CONFIG_NAMES: &[&str] = &["config.toml", "managed_config.toml", "requirements.toml"];

/// Plugin and skill folders the Grok configuration file at `path` declares:
/// `[plugins] paths`, and `[skills] paths`, `server_skill_dirs` and
/// `bundled_skill_dirs`, in its top-level tables and in every
/// `[[version_overrides]]` and `[[campaigns]]` patch, whatever version or
/// campaign it names. `base` is the folder holding the file, for a project
/// configuration; it is canonicalized once here, however many entries the file
/// declares.
pub(crate) fn grok_config_paths(path: &Path, base: Option<&Path>) -> Declared {
    let mut out = Declared::default();
    let Some(text) = read_declaration(path) else {
        return out;
    };
    let Ok(config) = text.parse::<toml::Table>() else {
        return out;
    };
    let base = base.map(|base| canonicalize_for_permission(base).compare);
    let patches = ["version_overrides", "campaigns"]
        .iter()
        .filter_map(|key| config.get(*key))
        .filter_map(toml::Value::as_array)
        .flatten()
        .filter_map(toml::Value::as_table);
    for table in std::iter::once(&config).chain(patches) {
        declare_table_paths(table, base.as_deref(), &mut out);
    }
    out
}

/// The plugin and skill folders one configuration table or patch declares.
fn declare_table_paths(table: &toml::Table, base: Option<&Path>, out: &mut Declared) {
    for (section, key) in DECLARED_LISTS {
        let entries = table
            .get(*section)
            .and_then(|section| section.get(*key))
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str);
        for entry in entries {
            declare_list_entry(section, entry, base, out);
        }
    }
}

/// One entry of a `[plugins]` or `[skills]` list.
fn declare_list_entry(section: &str, entry: &str, base: Option<&Path>, out: &mut Declared) {
    let mut declared = Declared::default();
    declare(entry, base, &mut declared);
    if section == "skills" {
        // The skills loader takes a file entry's folder as a skills folder.
        for path in &mut declared.paths {
            let folder = path.parent().map(Path::to_path_buf);
            if let Some(folder) = folder.filter(|_| path.is_file()) {
                *path = folder;
            }
        }
    }
    out.extend(declared);
}

/// Plugin and skill folders set by the campaign patches in
/// `GROK_CAMPAIGNS_OVERRIDE`, which Turbo applies to every session started with
/// that variable.
fn campaign_override_paths() -> Declared {
    match std::env::var("GROK_CAMPAIGNS_OVERRIDE") {
        Ok(json) => campaign_override_paths_from(&json),
        Err(_) => Declared::default(),
    }
}

/// [`campaign_override_paths`] from the variable's text. Turbo reads each entry
/// as a campaign id and a patch flattened beside it
/// (`{"id": "team", "skills": {"paths": [...]}}`), so every other key is part
/// of the patch.
pub(crate) fn campaign_override_paths_from(json: &str) -> Declared {
    let mut out = Declared::default();
    let Ok(Value::Array(campaigns)) = serde_json::from_str::<Value>(json) else {
        return out;
    };
    for campaign in &campaigns {
        for (section, key) in DECLARED_LISTS {
            let entries = campaign
                .get(*section)
                .and_then(|section| section.get(*key))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str);
            for entry in entries {
                declare_list_entry(section, entry, None, &mut out);
            }
        }
    }
    out
}

/// Local marketplace folders named in the Claude settings file at `path`, which
/// sits in `base`.
fn claude_marketplace_paths(path: &Path, base: &Path) -> Declared {
    let mut out = Declared::default();
    let Some(text) = read_declaration(path) else {
        return out;
    };
    let Ok(settings) = serde_json::from_str::<Value>(&text) else {
        return out;
    };
    let base = canonicalize_for_permission(base).compare;
    let entries = settings
        .get("extraKnownMarketplaces")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|marketplaces| marketplaces.values())
        .filter_map(|marketplace| marketplace.get("source")?.get("path")?.as_str());
    for entry in entries {
        declare(entry, Some(&base), &mut out);
    }
    out
}

/// Marketplace folders recorded in Claude's `known_marketplaces.json` in
/// `plugins_dir` (`~/.claude/plugins`), whose plugins Turbo's Claude-compatible
/// discovery loads. Turbo takes a relative `installLocation` from the folder it
/// runs in; Claude records one under `plugins_dir`, so that counts too.
pub(crate) fn known_marketplace_paths(plugins_dir: &Path) -> Declared {
    let mut out = Declared::default();
    let Some(text) = read_declaration(&plugins_dir.join("known_marketplaces.json")) else {
        return out;
    };
    let Ok(Value::Object(registry)) = serde_json::from_str::<Value>(&text) else {
        return out;
    };
    let entries = registry
        .values()
        .filter_map(|entry| entry.get("installLocation")?.as_str());
    for raw in entries {
        declare(raw, None, &mut out);
        if !Path::new(raw.trim()).is_absolute() {
            for path in resolve_declared(plugins_dir, raw) {
                out.push_path(path);
            }
        }
    }
    out
}

/// Plugin folders recorded in Claude's `installed_plugins.json` in
/// `plugins_dir`, which Turbo's Claude-compatible discovery loads as trusted
/// user plugins, wherever they are.
pub(crate) fn installed_plugin_paths(plugins_dir: &Path) -> Declared {
    let mut out = Declared::default();
    let Some(text) = read_declaration(&plugins_dir.join("installed_plugins.json")) else {
        return out;
    };
    let Ok(installed) = serde_json::from_str::<Value>(&text) else {
        return out;
    };
    let Some(plugins) = installed.get("plugins").and_then(Value::as_object) else {
        return out;
    };
    for entries in plugins.values() {
        // A list of entries, or a single one.
        let entries = entries
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(std::slice::from_ref(entries));
        for entry in entries {
            if let Some(raw) = entry.get("installPath").and_then(Value::as_str) {
                declare(raw, None, &mut out);
            }
        }
    }
    out
}

/// Where Turbo keeps the plugins it has installed: `[plugins].install_dir` from
/// a global configuration file, or `installed-plugins` in a Grok home.
fn install_dirs(
    grok_homes: &[PathBuf],
    global: &[PathBuf],
    declared: &mut Declared,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in global {
        for name in GLOBAL_CONFIG_NAMES {
            let Some(text) = read_declaration(&dir.join(name)) else {
                continue;
            };
            let Ok(config) = text.parse::<toml::Table>() else {
                continue;
            };
            // A version or campaign patch moves the install directory just as it
            // moves a plugin path.
            let patches = ["version_overrides", "campaigns"]
                .iter()
                .filter_map(|key| config.get(*key))
                .filter_map(toml::Value::as_array)
                .flatten()
                .filter_map(toml::Value::as_table);
            for table in std::iter::once(&config).chain(patches) {
                let Some(raw) = table
                    .get("plugins")
                    .and_then(|plugins| plugins.get("install_dir"))
                    .and_then(toml::Value::as_str)
                else {
                    continue;
                };
                // A relative or `~/` value counts the way every other declared
                // location does; only an absolute one names a folder to read
                // the registry from.
                declare(raw, None, declared);
                let expanded = xai_grok_config::expand_env_vars_in_string(raw);
                dirs.extend(resolve_declared_from(&[], expanded.trim()));
            }
        }
    }
    dirs.extend(grok_homes.iter().map(|home| home.join("installed-plugins")));
    dirs
}

/// The plugin sources and snapshots Turbo's install registry records. Turbo
/// re-copies a local install's whole source folder into its snapshot at every
/// session spawn, when the source is under the operator's home or trusted, and
/// loads the snapshot as a trusted plugin, so both count, and so does the
/// install directory itself when a root holds it.
fn install_registry_paths(install_dirs: &[PathBuf]) -> Declared {
    let mut out = Declared::default();
    for dir in install_dirs {
        let Some(text) = read_declaration(&dir.join("registry.json")) else {
            continue;
        };
        let Ok(registry) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        out.push_path(lexical_normalize(dir));
        let repos = registry
            .get("repos")
            .and_then(Value::as_object)
            .into_iter()
            .flatten();
        for (_, repo) in repos {
            if let Some(path) = repo.get("path").and_then(Value::as_str) {
                declare(path, None, &mut out);
            }
            let kind = repo.get("kind");
            let source = kind
                .filter(|kind| kind.get("type").and_then(Value::as_str) == Some("Local"))
                .and_then(|kind| kind.get("source_path"))
                .and_then(Value::as_str);
            if let Some(source) = source {
                declare(source, None, &mut out);
            }
        }
    }
    out
}

/// Plugin, skill and marketplace folders Turbo loads that configuration
/// declares: each Grok home's and the system's `config.toml`,
/// `managed_config.toml` and `requirements.toml` (their patches included), the
/// campaign patches and launcher skill folders set in the environment, the
/// plugins Turbo's install registry and Claude's own records name, the
/// marketplaces in Claude's `known_marketplaces.json`, and `.grok/config.toml`
/// and `.claude/settings.json` at the root, in every folder above it, and in
/// every folder under it that `config_dirs` names.
fn declared_extension_paths(
    root: &Path,
    grok_homes: &[PathBuf],
    config_dirs: &[PathBuf],
) -> Declared {
    let mut global: Vec<PathBuf> = grok_homes.to_vec();
    global.extend(xai_grok_config::system_config_dir());
    let mut out = Declared::default();
    for dir in &global {
        for name in GLOBAL_CONFIG_NAMES {
            out.extend(grok_config_paths(&dir.join(name), None));
        }
    }
    out.extend(campaign_override_paths());
    let install_dirs = install_dirs(grok_homes, &global, &mut out);
    out.extend(install_registry_paths(&install_dirs));
    for variable in [
        "GROK_WORKSPACE_SERVER_SKILLS_DIR",
        "GROK_WORKSPACE_BUNDLED_SKILLS_DIR",
    ] {
        if let Some(value) = std::env::var_os(variable) {
            declare(&value.to_string_lossy(), None, &mut out);
        }
    }
    // Claude-compatible discovery loads what Claude has recorded.
    if let Some(home) = dirs::home_dir() {
        let plugins = home.join(".claude").join("plugins");
        out.extend(known_marketplace_paths(&plugins));
        out.extend(installed_plugin_paths(&plugins));
    }
    for dir in root
        .ancestors()
        .chain(config_dirs.iter().map(PathBuf::as_path))
    {
        out.extend(grok_config_paths(
            &dir.join(".grok").join("config.toml"),
            Some(dir),
        ));
        out.extend(claude_marketplace_paths(
            &dir.join(".claude").join("settings.json"),
            dir,
        ));
    }
    out
}
/// Something an `.envrc` makes direnv or Turbo load, besides the file itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvrcLoad {
    /// A file sourced by name: `source`, `.`, `dotenv` or `dotenv_if_exists`.
    File(String),
    /// `source_env` or `source_env_if_exists`: a file, or a folder's `.envrc`.
    FileOrEnvrc(String),
    /// `source_up` or `source_up_if_exists` with a file name: that file, in the
    /// nearest folder above that has one.
    Up(String),
    /// `use flake`, with its flake reference when one is given.
    Flake(Option<String>),
    /// `use nix`, with its first argument when one is given.
    Nix(Option<String>),
    /// `use devenv`.
    Devenv,
}

/// What an `.envrc` loads. Each command in the file is read on its own, the way
/// a shell splits one: quotes and backslashes resolved, comments dropped, a
/// backslash before a line break joining two lines, and `;`, `&`, `|`, `(`, `)`
/// and line breaks ending a command. A shell keyword or grouping token the
/// command starts with is stepped past.
pub(crate) fn envrc_loads(text: &str) -> Vec<EnvrcLoad> {
    const LEADING: &[&str] = &[
        "!", "builtin", "command", "do", "elif", "else", "exec", "if", "then", "time", "until",
        "while", "{", "}",
    ];
    let mut out = Vec::new();
    for command in shell_commands(text) {
        let mut words = command
            .iter()
            .map(String::as_str)
            .skip_while(|word| LEADING.contains(word));
        let Some(name) = words.next() else {
            continue;
        };
        let argument = words.next().map(str::to_string);
        let load = match (name, argument) {
            ("." | "source" | "dotenv" | "dotenv_if_exists", Some(file)) => EnvrcLoad::File(file),
            ("source_env" | "source_env_if_exists", Some(file)) => EnvrcLoad::FileOrEnvrc(file),
            ("source_up" | "source_up_if_exists", Some(file)) => EnvrcLoad::Up(file),
            ("use", Some(kind)) => match kind.as_str() {
                "flake" => EnvrcLoad::Flake(words.next().map(str::to_string)),
                "nix" => EnvrcLoad::Nix(words.next().map(str::to_string)),
                "devenv" => EnvrcLoad::Devenv,
                _ => continue,
            },
            ("use_flake", reference) => EnvrcLoad::Flake(reference),
            ("use_nix", argument) => EnvrcLoad::Nix(argument),
            ("use_devenv", _) => EnvrcLoad::Devenv,
            _ => continue,
        };
        out.push(load);
    }
    out
}

/// The commands in shell `text`, each as its words. Words are split and
/// unquoted the way a shell does it, so a loader's argument holding a space or
/// spelled across two lines is read whole.
fn shell_commands(text: &str) -> Vec<Vec<String>> {
    let mut split = ShellSplit::default();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                // A backslash before a line break continues the line.
                Some('\n') => {}
                Some('\r') => {
                    chars.next_if_eq(&'\n');
                }
                Some(escaped) => split.push(escaped),
                None => {}
            },
            '\'' => {
                split.start_word();
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    split.push(c);
                }
            }
            '"' => {
                split.start_word();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('\n') => {}
                            Some('\r') => {
                                chars.next_if_eq(&'\n');
                            }
                            // Inside double quotes a backslash escapes only
                            // these, and stands for itself before anything else.
                            Some(escaped @ ('"' | '\\' | '$' | '`')) => split.push(escaped),
                            Some(other) => {
                                split.push('\\');
                                split.push(other);
                            }
                            None => {}
                        },
                        other => split.push(other),
                    }
                }
            }
            // A `#` starts a comment only where a word does.
            '#' if !split.in_word => {
                skip_to_line_end(&mut chars);
                split.end_command();
            }
            ' ' | '\t' | '\r' => split.end_word(),
            '\n' | ';' | '&' | '|' | '(' | ')' => split.end_command(),
            other => split.push(other),
        }
    }
    split.finish()
}

/// The state of [`shell_commands`] as it reads a file.
#[derive(Default)]
struct ShellSplit {
    commands: Vec<Vec<String>>,
    words: Vec<String>,
    word: String,
    /// Whether a word has started, which an empty quoted word also does.
    in_word: bool,
}

impl ShellSplit {
    fn push(&mut self, c: char) {
        self.in_word = true;
        self.word.push(c);
    }

    fn start_word(&mut self) {
        self.in_word = true;
    }

    fn end_word(&mut self) {
        if self.in_word {
            self.words.push(std::mem::take(&mut self.word));
            self.in_word = false;
        }
    }

    fn end_command(&mut self) {
        self.end_word();
        if !self.words.is_empty() {
            self.commands.push(std::mem::take(&mut self.words));
        }
    }

    fn finish(mut self) -> Vec<Vec<String>> {
        self.end_command();
        self.commands
    }
}
/// The files `load`, in an `.envrc` in `dir`, has direnv read. A file it looks
/// for in the folders above `dir` counts only inside `root`.
fn envrc_load_paths(dir: &Path, root: &Path, load: &EnvrcLoad) -> Vec<PathBuf> {
    match load {
        EnvrcLoad::File(file) => resolve_declared(dir, file),
        // A folder names its `.envrc`, which the `.envrc` name rule refuses.
        EnvrcLoad::FileOrEnvrc(file) => resolve_declared(dir, file)
            .into_iter()
            .filter(|path| !path.is_dir())
            .collect(),
        EnvrcLoad::Up(file) => dir
            .ancestors()
            .skip(1)
            .take_while(|folder| folder.starts_with(root))
            .map(|folder| folder.join(file))
            .collect(),
        EnvrcLoad::Flake(reference) => {
            let reference = reference.as_deref().unwrap_or(".");
            let reference = reference.strip_prefix("path:").unwrap_or(reference);
            let folder = reference.split('#').next().unwrap_or_default();
            // A reference with a scheme names a flake somewhere else.
            if folder.contains(':') && !Path::new(folder).is_absolute() {
                return Vec::new();
            }
            let folder = if folder.is_empty() { "." } else { folder };
            resolve_declared(dir, folder)
                .into_iter()
                .flat_map(|folder| [folder.join("flake.nix"), folder.join("flake.lock")])
                .collect()
        }
        EnvrcLoad::Nix(Some(argument)) if argument.starts_with('-') => Vec::new(),
        EnvrcLoad::Nix(Some(file)) => resolve_declared(dir, file),
        EnvrcLoad::Nix(None) => vec![dir.join("shell.nix"), dir.join("default.nix")],
        EnvrcLoad::Devenv => vec![
            dir.join("devenv.nix"),
            dir.join("devenv.yaml"),
            dir.join("devenv.lock"),
        ],
    }
}

/// What a walk of a root finds.
#[derive(Default)]
struct WalkedDeclarations {
    /// Files that `.envrc` loaders pull in.
    envrc_sources: Vec<PathBuf>,
    /// Folders holding a `.grok` or `.claude` folder.
    config_dirs: Vec<PathBuf>,
    /// Repositories git would run hooks for: a folder holding `.git`, and a
    /// bare repository, which its content gives away whatever it is called.
    repositories: Vec<GitRepository>,
    /// Every link found, as the root spells it.
    links: Vec<PathBuf>,
    /// Paths through a link whose names are refused for every access. What they
    /// lead to is refused for every access too.
    refused_links: Vec<RefusedLink>,
    /// The same for a walk target: a walk of what the link leads to is refused.
    walk_refused_links: Vec<RefusedLink>,
    /// The same for writes.
    write_refused_links: Vec<RefusedLink>,
}

/// A path through a link whose names a rule refuses.
#[derive(Debug, Clone)]
struct RefusedLink {
    /// The link, as the root spells it.
    link: PathBuf,
    /// What the refused path holds after it: the second name of a rule that
    /// needs two, and nothing for a rule about the link's own name.
    rest: &'static str,
}

/// Names a rule refuses only when one of a few names follows, with those names.
/// A link named like the first leads to a folder with another name, so the
/// second name is probed through the link as well. A test keeps this in step
/// with the name rules.
pub(crate) const PAIR_RULE_NAMES: &[(&str, &[&str])] = &[
    (".aws", &["credentials"]),
    (".cargo", &["credentials", "credentials.toml"]),
    (".docker", &["config.json"]),
    (".git", &["config", "config.worktree", "hooks"]),
    (".github", &["actions", "workflows"]),
    (".kube", &["config"]),
    ("hooks", &["hooks.json"]),
];

/// The names that complete a rule about `name`, lowercase.
fn pair_rule_seconds(name: &str) -> &'static [&'static str] {
    if name.len() > 4 && name.ends_with(".git") {
        // A bare or mirror repository that does not exist yet.
        return &["config"];
    }
    PAIR_RULE_NAMES
        .iter()
        .find(|(first, _)| *first == name)
        .map_or(&[], |(_, seconds)| *seconds)
}

/// Whether a folder is one git would open as a repository, answered once per
/// folder for a whole walk.
#[derive(Default)]
struct RepositoryCache(std::collections::HashMap<PathBuf, bool>);

impl RepositoryCache {
    /// Whether `dir` is a repository, for a rule about a path through `link`.
    /// A folder at or below the link is never looked at: the link's own name
    /// decides, and looking through it can mount a filesystem or wait out an
    /// unreachable share.
    fn is_repository(&mut self, dir: &Path, link: &Path) -> bool {
        if dir.starts_with(link) {
            return false;
        }
        if let Some(known) = self.0.get(dir) {
            return *known;
        }
        let answer = is_git_dir(dir);
        self.0.insert(dir.to_path_buf(), answer);
        answer
    }
}

/// Record the paths through `link` that a name rule refuses: the link's own,
/// and the one a rule needing a second name would refuse.
fn record_refused_link(
    link: &Path,
    repositories: &mut RepositoryCache,
    found: &mut WalkedDeclarations,
) {
    let name = link
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    for rest in std::iter::once("").chain(pair_rule_seconds(&name).iter().copied()) {
        let probe = if rest.is_empty() {
            link.to_path_buf()
        } else {
            link.join(rest)
        };
        let refused = |access, repositories: &mut RepositoryCache| {
            name_only_reason(&probe, access)
                .or_else(|| {
                    git_metadata_reason_with(&probe, access, &mut |dir| {
                        repositories.is_repository(dir, link)
                    })
                })
                .is_some()
        };
        let record = RefusedLink {
            link: link.to_path_buf(),
            rest,
        };
        if refused(Access::Read, repositories) {
            found.refused_links.push(record);
            continue;
        }
        if refused(Access::Walk, repositories) {
            found.walk_refused_links.push(record.clone());
        }
        if refused(Access::Write, repositories) {
            found.write_refused_links.push(record);
        }
    }
}

/// Walk `root` for declarations, for repositories and for links whose names are
/// refused. The walk never follows links, skips VCS metadata and dependency or
/// build trees, and stops after 50,000 folders or 12 levels.
fn walk_declarations(root: &Path) -> WalkedDeclarations {
    const MAX_DIRS: usize = 50_000;
    const MAX_DEPTH: usize = 12;
    const MAX_LINKS: usize = 100_000;
    const SKIPPED: &[&str] = &[".git", "node_modules", "target", ".venv", "venv"];
    let mut found = WalkedDeclarations::default();
    let mut repositories = RepositoryCache::default();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    let mut link_cap_reported = false;
    while let Some((dir, depth)) = pending.pop() {
        visited += 1;
        if visited > MAX_DIRS {
            tracing::warn!(
                root = ?root,
                "stopped looking for declared locations after {MAX_DIRS} directories"
            );
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        // What a bare repository holds, whatever the folder is called.
        let (mut head, mut objects, mut refs) = (false, false, false);
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let path = entry.path();
            // Only a hint: the listing's types say nothing about a linked
            // `objects`, and Windows spells names in any case.
            head |= name.eq_ignore_ascii_case("HEAD");
            objects |= name.eq_ignore_ascii_case("objects");
            refs |= name.eq_ignore_ascii_case("refs");
            if file_type.is_symlink() {
                // Past the cap a link is still judged by its name, which is
                // cheap; only following it to what it leads to stops.
                if found.links.len() < MAX_LINKS {
                    found.links.push(path.clone());
                } else if !link_cap_reported {
                    link_cap_reported = true;
                    tracing::warn!(
                        root = ?root,
                        "stopped following links after {MAX_LINKS} of them"
                    );
                }
                // What a link leads to is judged by its own name, not by the
                // name it leads to.
                record_refused_link(&path, &mut repositories, &mut found);
            }
            // A link named `.envrc` counts: direnv loads it like a regular file.
            if (file_type.is_file() || file_type.is_symlink())
                && name == ".envrc"
                && let Some(text) = read_declaration(&path)
            {
                for load in envrc_loads(&text) {
                    found
                        .envrc_sources
                        .extend(envrc_load_paths(&dir, root, &load));
                }
            }
            if (name == ".grok" || name == ".claude")
                && (file_type.is_dir() || (file_type.is_symlink() && path.is_dir()))
            {
                found.config_dirs.push(dir.clone());
            }
            if name.eq_ignore_ascii_case(".git") {
                // Not walked. Git runs this repository's hooks, but only where
                // it would open one: a stray `.git` is not a repository.
                if is_worktree_top(&dir) {
                    found
                        .repositories
                        .push(GitRepository::Worktree(dir.clone()));
                }
            } else if file_type.is_dir()
                && depth < MAX_DEPTH
                && !SKIPPED.iter().any(|skipped| name == *skipped)
            {
                pending.push((path, depth + 1));
            }
        }
        // Decided by opening it, as git and every rule that refuses writes
        // inside a repository do.
        if (head || objects || refs) && is_git_dir(&dir) {
            found.repositories.push(GitRepository::Bare(dir.clone()));
        }
    }
    found.config_dirs.sort();
    found.config_dirs.dedup();
    found.repositories.dedup();
    found
}
/// `folder` when it is a link, and every link directly inside it.
fn folder_links(folder: &Path) -> Vec<PathBuf> {
    let mut links = Vec::new();
    if folder
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        links.push(folder.to_path_buf());
    }
    if let Ok(entries) = std::fs::read_dir(folder) {
        links.extend(
            entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_symlink()))
                .map(|entry| entry.path()),
        );
    }
    links
}

/// Files that `.envrc` files in the folders above `root` load inside it: direnv
/// loads the nearest `.envrc` above a folder, and Turbo the one where it runs.
fn ancestor_envrc_sources(root: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for dir in root.ancestors().skip(1) {
        if let Some(text) = read_declaration(&dir.join(".envrc")) {
            for load in envrc_loads(&text) {
                sources.extend(envrc_load_paths(dir, root, &load));
            }
        }
    }
    sources
}

/// Locations that Turbo, git or direnv load or run with no prompt, which name
/// rules cannot know: those a root's own configuration, a Grok home's or git's
/// declares, and what links named like a file refused for writes lead to. Read
/// once when the guard is built; the files in a root that declare them are
/// themselves write-refused, so a client cannot add to the list. The locations
/// come back canonical and in compare form, with the relative declarations.
/// More declarations than [`MAX_DECLARED_LOCATIONS`] refuse the tier instead.
fn declared_auto_run(
    roots: &[PathBuf],
    walks: &[WalkedDeclarations],
    grok_homes: &[PathBuf],
) -> Result<(Vec<PathBuf>, Vec<RelativeDeclaration>), Denial> {
    let mut declared = Declared::default();
    let mut paths = Vec::new();
    let mut resolved = Vec::new();
    for (root, walked) in roots.iter().zip(walks) {
        declared.extend(declared_extension_paths(
            root,
            grok_homes,
            &walked.config_dirs,
        ));
        // Git runs the hooks of every repository under a root, and of the one
        // holding the root when the root is a folder inside a worktree.
        let mut repositories = walked.repositories.clone();
        repositories.extend(enclosing_worktree(root));
        repositories.sort();
        repositories.dedup();
        paths.extend(git_locations(&repositories));
        paths.extend(walked.envrc_sources.iter().cloned());
        paths.extend(ancestor_envrc_sources(root));
        resolved.extend(
            walked
                .write_refused_links
                .iter()
                .flat_map(link_destinations),
        );
    }
    if declared.overflowed {
        tracing::error!(
            "the configuration under these roots declares more than {MAX_DECLARED_LOCATIONS} \
             plugin, skill or marketplace locations"
        );
        return Err(Denial::new(Reason::TooManyDeclarations));
    }
    let mut compare: Vec<PathBuf> = declared
        .paths
        .iter()
        .chain(paths.iter())
        .map(|path| canonicalize_for_permission(path).compare)
        .collect();
    compare.extend(resolved);
    compare.sort();
    compare.dedup();
    Ok((compare, declared.relative))
}

/// A link a loader could follow, in both forms the rules compare.
struct WalkedLink {
    /// The link's path, as the walk found it: a canonical folder and its name,
    /// which is how a loader reading that folder spells it.
    display: PathBuf,
    /// The same path in compare form.
    compare: PathBuf,
    /// Its names, lowercase.
    names: Vec<String>,
}

impl WalkedLink {
    fn new(link: &Path) -> Self {
        let compare = fold_for_compare(link);
        Self {
            names: lowercase_names(&compare),
            compare,
            display: link.to_path_buf(),
        }
    }

    /// Where the link leads with `rest` joined on, in compare form. A link in
    /// `rest` is followed too, which is what a loader walking there does.
    fn leads_to(&self, rest: &[String]) -> PathBuf {
        // A link that resolves to nothing canonicalizes to its own path, so
        // where it leads is read from the link itself: a client could create
        // that target and have the loader follow it next session.
        let mut path = dangling_link_target(&self.display)
            .unwrap_or_else(|| canonicalize_for_permission(&self.display).display);
        path.extend(rest);
        canonicalize_for_permission(&path).compare
    }
}

/// A path in the form the deny prefixes compare, which ignores letter case on
/// Windows exactly as [`canonicalize_for_permission`] does. A canonical path
/// needs no filesystem call to reach that form; a test keeps the two in step.
pub(crate) fn fold_for_compare(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Where `link` leads when nothing exists at the end of it: its target joined
/// to its folder, followed through further links, and resolved as far as
/// anything on the way exists. `None` when the link resolves on its own, so the
/// canonical path already names what it leads to.
fn dangling_link_target(link: &Path) -> Option<PathBuf> {
    let mut current = canonicalize_for_permission(link).display;
    let mut followed = 0;
    while std::fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink()) {
        if followed == MAX_LINK_HOPS {
            break;
        }
        followed += 1;
        let Ok(target) = std::fs::read_link(&current) else {
            break;
        };
        let folder = current.parent().map(Path::to_path_buf).unwrap_or_default();
        current = canonicalize_for_permission(&folder.join(target)).display;
    }
    (followed > 0).then_some(current)
}

/// Where a refused path through a link leads, in compare form: through the link
/// as it resolves, and, when it resolves to nothing, through the target it
/// names, which a client could otherwise create for it to lead to.
fn link_destinations(refused: &RefusedLink) -> Vec<PathBuf> {
    let joined = |path: &Path| {
        if refused.rest.is_empty() {
            path.to_path_buf()
        } else {
            path.join(refused.rest)
        }
    };
    let mut destinations = vec![canonicalize_for_permission(&joined(&refused.link)).compare];
    if let Some(target) = dangling_link_target(&refused.link) {
        destinations.push(canonicalize_for_permission(&joined(&target)).compare);
    }
    destinations
}

/// Add to `prefixes` where every link a loader reads leads: one inside a
/// location already there, or along a location declared relative to where Turbo
/// runs. Repeated until nothing new appears, because a location reached through
/// one link can hold another.
fn follow_links_below(
    prefixes: &mut Vec<PathBuf>,
    relative: &[RelativeDeclaration],
    links: &[WalkedLink],
) {
    let mut order: Vec<usize> = (0..links.len()).collect();
    order.sort_by(|a, b| links[*a].compare.cmp(&links[*b].compare));
    let mut followed = vec![false; links.len()];
    let mut pending: Vec<PathBuf> = prefixes.clone();
    let follow = |index: usize,
                  followed: &mut Vec<bool>,
                  prefixes: &mut Vec<PathBuf>,
                  pending: &mut Vec<PathBuf>| {
        if followed[index] {
            return;
        }
        followed[index] = true;
        let target = links[index].leads_to(&[]);
        prefixes.push(target.clone());
        pending.push(target);
    };
    for (index, link) in links.iter().enumerate() {
        if relative
            .iter()
            .any(|declared| declared.covers(&link.compare, &link.names))
        {
            follow(index, &mut followed, prefixes, &mut pending);
        }
    }
    while let Some(prefix) = pending.pop() {
        // A path sorts next to the ones that start with it, so only one range
        // of the links can lie inside this location.
        let start = order.partition_point(|index| links[*index].compare < prefix);
        for index in order[start..].iter().copied() {
            if !links[index].compare.starts_with(&prefix) {
                break;
            }
            follow(index, &mut followed, prefixes, &mut pending);
        }
    }
}

/// Where the links lead that a loader follows on its way to a location declared
/// relative to where Turbo runs: the declared folder itself when it is a link,
/// and a link on any name above it. The loader follows them, so what they lead
/// to is that declared location.
fn links_along_relative_declarations(
    links: &[WalkedLink],
    relative: &[RelativeDeclaration],
) -> Vec<PathBuf> {
    // Keyed by the name a link's own would have to be, so a link is compared
    // only with the declarations that could end there.
    let mut by_last_name: std::collections::HashMap<&str, Vec<(usize, usize)>> =
        std::collections::HashMap::new();
    for (index, declared) in relative.iter().enumerate() {
        for (position, name) in declared.names.iter().enumerate() {
            by_last_name
                .entry(name.as_str())
                .or_default()
                .push((index, position + 1));
        }
    }
    let mut out = Vec::new();
    for link in links {
        let Some(last) = link.names.last() else {
            continue;
        };
        for (index, taken) in by_last_name
            .get(last.as_str())
            .into_iter()
            .flatten()
            .copied()
        {
            if let Some(rest) = relative[index].rest_after(&link.compare, &link.names, taken) {
                out.push(link.leads_to(rest));
            }
        }
    }
    out
}

/// Most folders listed while looking outside the roots for links a loader
/// follows into one.
const MAX_LOADER_SCAN_DIRS: usize = 20_000;

/// How deep that search goes below a loader's folder.
const MAX_LOADER_SCAN_DEPTH: usize = 8;

/// The agent configuration folders a loader reads (`.claude`), each with the
/// folders inside it that hold what is loaded (`.claude/skills`). Taken from
/// the same tables as the auto-run names, so a folder added there is searched
/// here without an edit.
static LOADER_FOLDERS: LazyLock<std::collections::BTreeMap<String, BTreeSet<String>>> =
    LazyLock::new(|| {
        let compat = CompatConfig::default();
        let mut folders: std::collections::BTreeMap<String, BTreeSet<String>> =
            std::collections::BTreeMap::new();
        let mut record = |entry: &str, is_folder: bool| {
            let mut names = entry.split('/');
            let Some(dir) = names.next().filter(|name| name.starts_with('.')) else {
                return;
            };
            let inside = folders.entry(dir.to_ascii_lowercase()).or_default();
            // The second name is a folder when the entry names one, or when
            // more names follow it.
            if let Some(second) = names.next()
                && (is_folder || names.next().is_some())
            {
                inside.insert(second.to_ascii_lowercase());
            }
        };
        for entry in compat
            .rules_dirs()
            .into_iter()
            .chain(compat.skill_config_dirs())
        {
            record(entry, true);
        }
        for entry in FOLDER_TRUST_MARKER_SAMPLES
            .iter()
            .flat_map(|(_, samples)| *samples)
            .copied()
            .chain(compat.agent_filenames())
        {
            record(entry, false);
        }
        folders
    });

/// The folders outside the roots that Turbo, or a tool it reads the
/// configuration of, loads from, with whether what a link in one leads to is
/// refused for every access (a Grok home's content is) or only for writes, and
/// whether the search goes below the folder.
fn loader_folders(grok_homes: &[PathBuf]) -> Vec<(PathBuf, bool, bool)> {
    let mut folders = Vec::new();
    let mut add = |folder: PathBuf, every_access: bool, inside: Option<&BTreeSet<String>>| {
        // The folder itself is listed only one level deep: a Grok home also
        // holds sessions and caches, which are not loaded from.
        folders.push((folder.clone(), every_access, false));
        for name in inside.into_iter().flatten() {
            folders.push((folder.join(name), every_access, true));
        }
    };
    for home in grok_homes {
        add(home.clone(), true, LOADER_FOLDERS.get(".grok"));
    }
    for home in tilde_homes() {
        for (dir, inside) in LOADER_FOLDERS.iter() {
            add(home.join(dir), false, Some(inside));
        }
    }
    folders
}

/// Where the links in those folders lead, when they lead into a root: what a
/// loader reaches from outside every root, as (for every access, for writes).
fn loader_folder_link_targets(
    grok_homes: &[PathBuf],
    roots: &[PathBuf],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut budget = MAX_LOADER_SCAN_DIRS;
    let mut every_access_targets = Vec::new();
    let mut write_targets = Vec::new();
    let mut pending = loader_folders(grok_homes);
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    while let Some((folder, every_access, deep)) = pending.pop() {
        if budget == 0 {
            break;
        }
        let canonical = canonicalize_for_permission(&folder);
        // A folder inside a root is walked with the root, which records every
        // link in it. The visited set ends a cycle of links.
        if canonical.lexical_only || !seen.insert(canonical.compare.clone()) {
            continue;
        }
        if roots.iter().any(|root| canonical.compare.starts_with(root)) {
            continue;
        }
        for link in links_under(&canonical.display, deep, &mut budget) {
            let target = WalkedLink::new(&link).leads_to(&[]);
            if roots.iter().any(|root| target.starts_with(root)) {
                if every_access {
                    every_access_targets.push(target);
                } else {
                    write_targets.push(target);
                }
            } else {
                // A link out of a loader folder can lead to a folder that holds
                // another link into a root; the loader follows both.
                pending.push((target, every_access, deep));
            }
        }
    }
    (every_access_targets, write_targets)
}

/// The links directly in `folder`, and below it when `deep`, listing at most
/// `budget` folders and never following a link.
fn links_under(folder: &Path, deep: bool, budget: &mut usize) -> Vec<PathBuf> {
    const SKIPPED: &[&str] = &[".git", "node_modules", "target", ".venv", "venv"];
    let mut links = Vec::new();
    let mut pending = vec![(folder.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = pending.pop() {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                links.push(entry.path());
            } else if deep
                && file_type.is_dir()
                && depth < MAX_LOADER_SCAN_DEPTH
                && !SKIPPED.iter().any(|skipped| entry.file_name() == *skipped)
            {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    links
}
/// Name patterns ripgrep must exclude during a recursive search, lowercase.
const DENY_READ_PATTERNS: &[&str] = &[
    "**/.git/**",
    "**/*.git/**/config",
    "**/*.git/**/config.worktree",
    "**/*.git/**/hooks/**",
    "**/.bare/**/config",
    "**/.bare/**/config.worktree",
    "**/.bare/**/hooks/**",
    "**/.git-credentials",
    "**/.grok/**",
    "**/grok.toml",
    "**/policy.toml",
    "**/.ssh/**",
    "**/.gnupg/**",
    "**/.aws/credentials",
    "**/.docker/config.json",
    "**/.kube/config",
    "**/.cargo/credentials",
    "**/.cargo/credentials.toml",
    "**/.netrc",
    "**/_netrc",
    "**/.npmrc",
    "**/.pypirc",
    "**/id_rsa",
    "**/id_dsa",
    "**/id_ecdsa",
    "**/id_ed25519",
    "**/*.tfstate",
    "**/*.tfstate.backup",
    "**/.env",
    "**/.env.*",
];

/// Rewrite every ASCII letter in a glob as a two-case class (`a` -> `[aA]`).
/// ripgrep matches `--glob` case-sensitively; the name rules here do not.
pub(crate) fn fold_glob_case(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() * 4);
    for c in pattern.chars() {
        if c.is_ascii_alphabetic() {
            out.push('[');
            out.push(c.to_ascii_lowercase());
            out.push(c.to_ascii_uppercase());
            out.push(']');
        } else {
            out.push(c);
        }
    }
    out
}

/// How a root is shown to the operator: its canonical spelling, which is also
/// how the tools print paths under it, so a client can name every file their
/// output shows. The error says why no client path could name a file under it.
pub(crate) fn printable_root(canonical: &Path) -> Result<PathBuf, &'static str> {
    // dunce keeps the `\\?\` spelling only where the plain one would not name
    // the same folder: a network share, or a path too long for it.
    if has_verbatim_prefix(canonical) {
        return Err("a network share, or a path too long for its plain spelling");
    }
    match root_inadmissible(canonical) {
        None => Ok(canonical.to_path_buf()),
        Some(cause) => Err(cause),
    }
}

/// Why no client path could name a file under `root`, or `None` if one can.
fn root_inadmissible(root: &Path) -> Option<&'static str> {
    // A client names files in JSON strings, which cannot hold a name that is not
    // valid Unicode.
    let Some(root) = root.to_str() else {
        return Some("a name that is not valid Unicode");
    };
    // On Windows ripgrep prints a name it cannot decode with U+FFFD, and grep
    // refuses any path holding one, so every search under such a root would
    // come back empty.
    if cfg!(windows) && root.contains('\u{FFFD}') {
        return Some("replacement character");
    }
    // Judged the way a client names a file inside the root.
    inadmissible(&Path::new(root).join("file").to_string_lossy())
}

/// Why `access` to a path is refused by its names or its git content, apart
/// from the locations a guard is configured with; `None` when it is not.
/// `compare` is the path in compare form, `display` as spelled on disk.
pub(crate) fn name_rule_reason(
    compare: &Path,
    display: &Path,
    access: Access,
) -> Option<&'static str> {
    name_only_reason(compare, access).or_else(|| git_metadata_reason(display, access))
}

/// Why `access` to a path is refused by its names alone, which needs no
/// filesystem call: the rules that do not depend on what is on disk.
fn name_only_reason(compare: &Path, access: Access) -> Option<&'static str> {
    if xai_grok_sandbox::is_sensitive_credential_store(compare) {
        return Some("credential store");
    }

    let comps: Vec<String> = compare
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();

    for (i, c) in comps.iter().enumerate() {
        let next = comps.get(i + 1).map(String::as_str);
        let next_is_last = i + 2 == comps.len();
        if c == ".grok" {
            return Some("dot-grok directory");
        }
        // A bare or mirror repository that does not exist yet has no content
        // to recognise it by, only its conventional name.
        if c.len() > 4 && c.ends_with(".git") && next == Some("config") && next_is_last {
            return Some("bare repository config");
        }
        if c == ".kube" && next == Some("config") && next_is_last {
            return Some("kubernetes credentials");
        }
        if c == ".cargo" && matches!(next, Some("credentials" | "credentials.toml")) && next_is_last
        {
            return Some("cargo credentials");
        }
        if access == Access::Walk && WALK_DENIED_DIRS.contains(&c.as_str()) {
            return Some("credential directory");
        }
        if access == Access::Write {
            if AUTO_RUN.dirs.contains(c) {
                return Some("configuration another tool loads or runs automatically");
            }
            if c == ".github" && matches!(next, Some("workflows" | "actions")) {
                return Some("CI workflow or action");
            }
            if c == "hooks" && next == Some("hooks.json") && next_is_last {
                return Some("plugin hooks");
            }
        }
    }

    let last = comps.last()?.as_str();
    if last == ".git-credentials" {
        return Some("git credentials");
    }
    if CREDENTIAL_NAMES.contains(&last)
        || last.ends_with(".tfstate")
        || last.ends_with(".tfstate.backup")
    {
        return Some("credential file");
    }
    if last == "grok.toml" || last == "policy.toml" {
        return Some("policy file");
    }
    if is_dotenv(last) {
        return Some("dotenv secrets");
    }
    if access == Access::Write
        && (AUTO_RUN.names.contains(last) || is_lefthook_config(last) || last.starts_with(".envrc"))
    {
        return Some("file other tools load or execute automatically");
    }
    None
}
/// The containment boundary.
#[derive(Clone, Debug)]
pub struct PathGuard {
    roots: RootsInner,
    /// Canonical roots plus the operator's own spelling of each, for the
    /// lexical check that runs before any filesystem call.
    root_spellings: Arc<Vec<PathBuf>>,
    /// Each root as the operator is shown it (see [`printable_root`]).
    printable_roots: Arc<Vec<PathBuf>>,
    /// Canonical absolute prefixes refused for every access, even inside a root.
    hard_deny_prefixes: Arc<Vec<PathBuf>>,
    /// Grok homes with no `.grok` component, which the name-based ripgrep
    /// exclude cannot cover. A walk that would enter one is refused.
    walk_blocking_prefixes: Arc<Vec<PathBuf>>,
    /// Where a link named like a folder no walk may target leads.
    walk_deny_prefixes: Arc<Vec<PathBuf>>,
    /// Edit tier: locations a root's configuration runs automatically.
    write_deny_prefixes: Arc<Vec<PathBuf>>,
    /// Edit tier: locations configuration declares relative to where Turbo runs.
    write_deny_relative: Arc<Vec<RelativeDeclaration>>,
    read_only: bool,
    /// The argument check waits on this gate, so a test can hold a call there.
    #[cfg(test)]
    check_gate: Option<Arc<crate::read_confined_fs::SyncGate>>,
}

impl PathGuard {
    /// Build a guard that denies this process's Grok homes and refuses
    /// over-broad roots.
    pub fn new(
        roots: Vec<PathBuf>,
        extra_hard_deny: Vec<PathBuf>,
        read_only: bool,
    ) -> Result<Self, Denial> {
        // A root that is an automount point is mounted first, so the mounts and
        // identities read next describe the filesystem it will serve.
        for root in &roots {
            let _ = std::fs::metadata(root);
        }
        let found = find_candidate_homes();
        // A folder holding too many homes to list that looking at could mount is
        // compared by path, like the homes below such a point.
        let (lookable, unlookable): (Vec<PathBuf>, Vec<PathBuf>) = found
            .crowded
            .iter()
            .cloned()
            .partition(|folder| !found.automounts.could_mount(folder));
        let inside_an_unlookable_folder = |root: &Path| {
            // Normalised without the disk: looking below the point is what
            // mounts it, and the neighbouring check canonicalizes.
            root.is_absolute()
                && lexical_normalize(root)
                    .parent()
                    .is_some_and(|parent| unlookable.iter().any(|folder| folder == parent))
        };
        if let Some(root) = roots.iter().find(|r| {
            is_over_broad_root_with(r, &found.homes, &found.automounts)
                || is_in_a_crowded_folder(r, &lookable)
                || inside_an_unlookable_folder(r)
        }) {
            tracing::error!(
                root = ?root,
                "refusing a root that is a filesystem root or is or contains a home directory"
            );
            return Err(Denial::new(Reason::OverBroadRoot));
        }
        Self::with_grok_homes(roots, grok_homes(), extra_hard_deny, read_only)
    }
    /// Build a guard with explicit Grok home locations.
    ///
    /// Roots must be absolute, existing directories; each is canonicalized once.
    pub fn with_grok_homes(
        roots: Vec<PathBuf>,
        grok_homes: Vec<PathBuf>,
        extra_hard_deny: Vec<PathBuf>,
        read_only: bool,
    ) -> Result<Self, Denial> {
        if roots.is_empty() {
            return Err(Denial::new(Reason::NoRoots));
        }
        let mut canon_roots = Vec::with_capacity(roots.len());
        let mut root_spellings = Vec::with_capacity(roots.len() * 2);
        let mut printable_roots = Vec::with_capacity(roots.len());
        for r in roots {
            if !r.is_absolute() || !r.is_dir() {
                tracing::error!(root = ?r, "root must be an absolute existing directory");
                return Err(Denial::new(Reason::NoRoots));
            }
            let c = canonicalize_for_permission(&r);
            if c.lexical_only {
                return Err(Denial::new(Reason::NoRoots));
            }
            let spelled = lexical_normalize(&r);
            // A root no client path can name would refuse every call. Refuse it
            // here instead, where the operator is told why.
            let printable = printable_root(&c.display).map_err(|cause| {
                tracing::error!(
                    root = ?r,
                    canonical = ?c.display,
                    cause,
                    "no client path can name a file under this root"
                );
                Denial::new(Reason::Inadmissible)
            })?;
            printable_roots.push(printable);
            root_spellings.push(spelled);
            root_spellings.push(c.display.clone());
            canon_roots.push(c.display);
        }

        let canon = |p: &Path| canonicalize_for_permission(p).compare;
        let homes: Vec<PathBuf> = grok_homes.iter().map(|h| canon(h)).collect();
        let walk_blocking: Vec<PathBuf> = homes
            .iter()
            .filter(|h| {
                !h.components().any(|c| {
                    matches!(c, Component::Normal(s) if s.to_string_lossy().eq_ignore_ascii_case(".grok"))
                })
            })
            .cloned()
            .collect();
        let mut deny = homes;
        deny.extend(extra_hard_deny.iter().map(|p| canon(p)));
        // Walked in both tiers: a link in a root named like a refused file leads
        // to its target under another name, and reads of that are refused too.
        let walks: Vec<WalkedDeclarations> = canon_roots
            .iter()
            .map(|root| walk_declarations(root))
            .collect();
        let roots_compare: Vec<PathBuf> = canon_roots.iter().map(|root| canon(root)).collect();
        let links: Vec<WalkedLink> = walks
            .iter()
            .flat_map(|walked| &walked.links)
            .map(|link| WalkedLink::new(link))
            .collect();
        // A loader reading the operator's own configuration folders follows a
        // link there into a root, and what it leads to is loaded from there.
        let (outside_deny, outside_write_deny) =
            loader_folder_link_targets(&grok_homes, &roots_compare);
        deny.extend(outside_deny);
        deny.extend(
            walks
                .iter()
                .flat_map(|walked| &walked.refused_links)
                .flat_map(link_destinations),
        );
        follow_links_below(&mut deny, &[], &links);
        deny.sort();
        deny.dedup();
        let mut walk_deny: Vec<PathBuf> = walks
            .iter()
            .flat_map(|walked| &walked.walk_refused_links)
            .flat_map(link_destinations)
            .collect();
        walk_deny.sort();
        walk_deny.dedup();
        let (write_deny, write_deny_relative) = if read_only {
            (Vec::new(), Vec::new())
        } else {
            let (mut write_deny, relative) = declared_auto_run(&canon_roots, &walks, &grok_homes)?;
            write_deny.extend(outside_write_deny);
            write_deny.extend(links_along_relative_declarations(&links, &relative));
            follow_links_below(&mut write_deny, &relative, &links);
            write_deny.sort();
            write_deny.dedup();
            (write_deny, relative)
        };

        Ok(Self {
            roots: Arc::new(RwLock::new(canon_roots)),
            root_spellings: Arc::new(root_spellings),
            printable_roots: Arc::new(printable_roots),
            hard_deny_prefixes: Arc::new(deny),
            walk_blocking_prefixes: Arc::new(walk_blocking),
            walk_deny_prefixes: Arc::new(walk_deny),
            write_deny_prefixes: Arc::new(write_deny),
            write_deny_relative: Arc::new(write_deny_relative),
            read_only,
            #[cfg(test)]
            check_gate: None,
        })
    }

    /// Hold every argument check on `gate` until it opens.
    #[cfg(test)]
    pub(crate) fn with_check_gate(
        mut self,
        gate: Option<Arc<crate::read_confined_fs::SyncGate>>,
    ) -> Self {
        self.check_gate = gate;
        self
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Snapshot of the approved roots. A copy: a writable handle would let any
    /// holder widen the boundary.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The roots as the operator should see them: spellings a client path can
    /// start with.
    pub fn printable_roots(&self) -> Vec<PathBuf> {
        self.printable_roots.as_ref().clone()
    }

    /// Ripgrep excludes mirroring the name-based hard-deny rules, for recursive
    /// tools that do not read through the filesystem layer. Case-folded, because
    /// the rules they mirror ignore case.
    pub fn deny_read_globs(&self) -> Vec<String> {
        DENY_READ_PATTERNS
            .iter()
            .map(|p| fold_glob_case(p))
            .collect()
    }

    /// Check a client-supplied path argument.
    pub fn check_path(&self, raw: &str, access: Access) -> Result<(), Denial> {
        admit(raw)?;
        let path = Path::new(raw);
        // Before any filesystem call: the time a refusal takes must not depend
        // on what exists outside the roots.
        if !self
            .root_spellings
            .iter()
            .any(|root| starts_with_components(path, root))
        {
            tracing::warn!(path = %log_preview(raw), "spelled outside every approved root");
            return Err(Denial::new(Reason::OutsideRoots));
        }
        self.check_resolved(path, access)?;
        self.check_name_used(path, access)?;
        // A walker searches a named FIFO, socket or device directly, and opening
        // a FIFO blocks until a writer appears.
        if access == Access::Walk
            && let Ok(metadata) = std::fs::metadata(path)
            && !metadata.is_file()
            && !metadata.is_dir()
        {
            tracing::warn!(path = %log_preview(raw), "walk target is not a file or directory");
            return Err(Denial::new(Reason::SpecialFile));
        }
        // Mirror the tools' Unicode filename fallback: when the requested name
        // does not exist, a sibling that matches after NBSP normalization would
        // be opened instead, so it must pass the same rules.
        if !path.exists() {
            let Some(siblings) = unicode_siblings(path) else {
                tracing::warn!(
                    path = %log_preview(raw),
                    "directory too large to check the Unicode filename fallback"
                );
                return Err(Denial::new(Reason::Unresolvable));
            };
            for sibling in siblings {
                self.check_resolved(&sibling, access)?;
                self.check_name_used(&sibling, access)?;
            }
        }
        Ok(())
    }

    /// Check a path that has already been resolved by a tool. Used by the
    /// filesystem layer and grep's result filter, where spelling rules no
    /// longer apply but containment and hard-deny rules do.
    pub fn check_resolved(&self, path: &Path, access: Access) -> Result<(), Denial> {
        let canon = canonicalize_for_permission(path);
        if canon.lexical_only {
            tracing::warn!(path = ?path, "no canonicalizable ancestor; denying");
            return Err(Denial::new(Reason::Unresolvable));
        }
        if let Some(why) = self.hard_deny_reason(&canon.compare, &canon.display, access) {
            tracing::warn!(path = ?path, reason = why, "hard-denied");
            return Err(Denial::new(Reason::HardDenied));
        }
        {
            let roots = self.roots.read().unwrap_or_else(|e| e.into_inner());
            if roots.is_empty() {
                return Err(Denial::new(Reason::NoRoots));
            }
            // `path_is_under_confine_root` still ignores letter case where
            // neither side exists and the directory they share is
            // case-insensitive, which covers a root removed after startup. Also
            // require each root's spelling as it was on disk when the guard was
            // built, so the served boundary does not rest on reading that flag.
            // This comparison is exact, prefix included, so a path whose
            // canonical form keeps the `\\?\` spelling (longer than MAX_PATH)
            // is refused here: the hard-deny prefixes above compare plain
            // spellings and would not catch it.
            if !roots
                .iter()
                .any(|r| path_is_under_confine_root(path, r) && canon.display.starts_with(r))
            {
                tracing::warn!(path = ?path, "outside every approved root");
                return Err(Denial::new(Reason::OutsideRoots));
            }
        }
        // Canonicalization resolves every existing link, so a symlink left in the
        // resolved form is one it could not follow: a dangling link in the
        // not-yet-created tail. A write through it would land wherever it points.
        if access == Access::Write && has_symlink_component(&canon.display) {
            tracing::warn!(path = ?path, "write through a symlinked component");
            return Err(Denial::new(Reason::SymlinkComponent));
        }
        Ok(())
    }

    /// Refuse `path`, as the client spelled it, when its own names are refused,
    /// or when configuration declares that spelling relative to where Turbo
    /// runs. A link named like a refused file, or lying along a declared
    /// location, leads to a target with another name, and a check of the
    /// resolved path sees only that one.
    fn check_name_used(&self, path: &Path, access: Access) -> Result<(), Denial> {
        let spelled = lexical_normalize(path);
        let why = name_rule_reason(&spelled, &spelled, access).or_else(|| {
            (access == Access::Write)
                .then(|| self.declared_relative_reason(&self.rooted_compare(&spelled)))
                .flatten()
        });
        if let Some(why) = why {
            tracing::warn!(
                path = %log_preview(&path.to_string_lossy()),
                reason = why,
                "refused by the name it was given"
            );
            return Err(Denial::new(Reason::HardDenied));
        }
        Ok(())
    }

    /// `spelled`, a client path, with the root it names spelled as that root is
    /// on disk, and in compare form: a location declared by name is then
    /// recognised however the operator spelled the root.
    fn rooted_compare(&self, spelled: &Path) -> PathBuf {
        for pair in self.root_spellings.chunks(2) {
            let [root, canonical] = pair else {
                continue;
            };
            if starts_with_components(spelled, root) {
                let rest: PathBuf = spelled
                    .components()
                    .skip(root.components().count())
                    .collect();
                return fold_for_compare(&canonical.join(rest));
            }
        }
        fold_for_compare(spelled)
    }

    /// Why a write to `compare` is refused by a location configuration declares
    /// relative to the folder Turbo runs in; `None` when none names it.
    fn declared_relative_reason(&self, compare: &Path) -> Option<&'static str> {
        if self.write_deny_relative.is_empty() {
            return None;
        }
        let names = lowercase_names(compare);
        self.write_deny_relative
            .iter()
            .any(|declared| declared.covers(compare, &names))
            .then_some("a location configuration declares relative to where Turbo runs")
    }

    fn hard_deny_reason(
        &self,
        compare: &Path,
        display: &Path,
        access: Access,
    ) -> Option<&'static str> {
        for prefix in self.hard_deny_prefixes.iter() {
            if compare == prefix.as_path() || compare.starts_with(prefix) {
                return Some("grok home, configured deny prefix, or where a refused name leads");
            }
        }
        if access == Access::Walk {
            if self
                .walk_blocking_prefixes
                .iter()
                .any(|home| home.starts_with(compare))
            {
                return Some("a recursive walk would enter the grok home");
            }
            if self
                .walk_deny_prefixes
                .iter()
                .any(|prefix| compare == prefix.as_path() || compare.starts_with(prefix))
            {
                return Some("where a name no walk may target leads");
            }
        }
        if access == Access::Write {
            if self
                .write_deny_prefixes
                .iter()
                .any(|prefix| compare == prefix.as_path() || compare.starts_with(prefix))
            {
                return Some("a location the root's own configuration runs automatically");
            }
            if let Some(why) = self.declared_relative_reason(compare) {
                return Some(why);
            }
        }
        name_rule_reason(compare, display, access)
    }

    /// Full pre-dispatch check for one tool call.
    pub fn check_call(&self, tool: &str, kind: ToolKind, args: &Value) -> Result<(), Denial> {
        #[cfg(test)]
        {
            if let Some(gate) = &self.check_gate {
                gate.wait();
            }
        }
        if self.read_only && !kind.is_read_only() {
            tracing::warn!(tool, "refused: server is read-only");
            return Err(Denial::new(Reason::ReadOnlyServer));
        }
        let Some(fields) = declared_path_fields(tool) else {
            tracing::warn!(tool, "refused: tool is not servable");
            return Err(Denial::new(Reason::UndeclaredTool));
        };

        let mut walk_target_named = false;
        for field in fields {
            match args.get(field.name) {
                None | Some(Value::Null) => continue,
                Some(Value::String(s)) if !field.multi => {
                    self.check_path(s, field.access)?;
                    walk_target_named |= field.access == Access::Walk;
                }
                Some(Value::Array(items)) if field.multi => {
                    if items.len() > MAX_ARGUMENT_STRINGS {
                        return Err(Denial::new(Reason::MalformedArgument));
                    }
                    for item in items {
                        let Some(s) = item.as_str() else {
                            return Err(Denial::new(Reason::MalformedArgument));
                        };
                        self.check_path(s, field.access)?;
                    }
                    walk_target_named |= field.access == Access::Walk && !items.is_empty();
                }
                Some(_) => return Err(Denial::new(Reason::MalformedArgument)),
            }
        }

        // `grep` walks the session directory, the first root, when no path is
        // named. That walk gets the check a named one would. (`list_dir`
        // requires its target; checking here too costs nothing.)
        if fields.iter().any(|f| f.access == Access::Walk) && !walk_target_named {
            let Some(default_root) = self.roots().into_iter().next() else {
                return Err(Denial::new(Reason::NoRoots));
            };
            self.check_resolved(&default_root, Access::Walk)?;
        }

        let bare = tool.rsplit(':').next().unwrap_or(tool);
        if matches!(bare, "grep" | "grep_search") {
            match args.get("glob") {
                None | Some(Value::Null) => {}
                Some(Value::String(g)) => {
                    validate_search_argument("glob", g)?;
                    validate_glob(g)?;
                }
                Some(_) => return Err(Denial::new(Reason::MalformedArgument)),
            }
            for name in ["pattern", "type"] {
                match args.get(name) {
                    None | Some(Value::Null) => {}
                    Some(Value::String(text)) => validate_search_argument(name, text)?,
                    Some(_) => return Err(Denial::new(Reason::MalformedArgument)),
                }
            }
        }

        self.sweep(args, effective_access(kind))
    }

    /// Rewrite each named walk target to its canonical spelling, so the walker
    /// builds child paths from the spelling the guard judged: symlinks, short
    /// names and letter case resolved. Call only after [`Self::check_call`].
    pub fn canonical_walk_args(&self, tool: &str, mut args: Value) -> Value {
        let (Some(fields), Some(map)) = (declared_path_fields(tool), args.as_object_mut()) else {
            return args;
        };
        for field in fields.iter().filter(|f| f.access == Access::Walk) {
            if let Some(Value::String(target)) = map.get_mut(field.name) {
                let canon = canonicalize_for_permission(Path::new(target.as_str()));
                *target = canon.display.to_string_lossy().into_owned();
            }
        }
        args
    }

    /// Key-aware, existence-independent sweep of the whole argument tree. At
    /// most [`MAX_ARGUMENT_STRINGS`] strings are examined; a call with more is
    /// refused rather than checked at length.
    fn sweep(&self, args: &Value, access: Access) -> Result<(), Denial> {
        let mut seen = BTreeSet::new();
        let mut budget = MAX_ARGUMENT_STRINGS;
        self.sweep_inner(args, access, &mut seen, &mut budget, 0)
    }

    fn sweep_inner(
        &self,
        v: &Value,
        access: Access,
        seen: &mut BTreeSet<String>,
        budget: &mut usize,
        depth: usize,
    ) -> Result<(), Denial> {
        if depth > 32 {
            return Err(Denial::new(Reason::MalformedArgument));
        }
        match v {
            Value::String(s) => {
                let Some(left) = budget.checked_sub(1) else {
                    tracing::warn!("refused: too many strings in the arguments");
                    return Err(Denial::new(Reason::MalformedArgument));
                };
                *budget = left;
                if s.is_empty() || !seen.insert(s.clone()) {
                    return Ok(());
                }
                if Path::new(s).is_absolute() || s.replace('\\', "/").starts_with("//") {
                    self.check_path(s, access)?;
                }
                Ok(())
            }
            Value::Array(items) => {
                for i in items {
                    self.sweep_inner(i, access, seen, budget, depth + 1)?;
                }
                Ok(())
            }
            Value::Object(map) => {
                for (k, val) in map {
                    if is_known_path_free(k) {
                        continue;
                    }
                    self.sweep_inner(val, access, seen, budget, depth + 1)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// The prefixes of `path` that the symlink rule inspects. A bare drive prefix
/// (`C:`) is drive-relative, so probing it would inspect the process's current
/// directory on that drive; prefixes and the root are never probed.
pub(crate) fn symlink_probe_paths(path: &Path) -> Vec<PathBuf> {
    let mut cursor = PathBuf::new();
    let mut probes = Vec::new();
    for comp in path.components() {
        cursor.push(comp);
        if matches!(comp, Component::Normal(_)) {
            probes.push(cursor.clone());
        }
    }
    probes
}

fn has_symlink_component(path: &Path) -> bool {
    for probe in symlink_probe_paths(path) {
        match std::fs::symlink_metadata(&probe) {
            Ok(md) if md.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    false
}

fn effective_access(kind: ToolKind) -> Access {
    if kind.is_read_only() {
        Access::Read
    } else {
        Access::Write
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! Pure helpers the tests exercise directly.
    pub(crate) use super::{
        is_lefthook_config, lexical_normalize, name_rule_reason, parse_git_config, read_git_config,
    };

    /// Build a guard in `body` against `homes` instead of the operator's real
    /// home directories, which a guard otherwise searches for links that lead
    /// into a root. Without this every test would read `~/.claude` and the rest.
    pub(crate) fn with_test_homes<T>(
        homes: Vec<std::path::PathBuf>,
        body: impl FnOnce() -> T,
    ) -> T {
        super::TEST_HOMES.with(|slot| *slot.borrow_mut() = Some(homes));
        let out = body();
        super::TEST_HOMES.with(|slot| *slot.borrow_mut() = None);
        out
    }
}
