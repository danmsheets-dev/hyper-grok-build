//! Type-safe heterogeneous resource container for the new tool architecture.
//!
//! `Resources` is the typed dependency injection container. It provides a
//! single `HashMap<TypeId, Box<dyn Any>>` that tools read from and write to.
//!
//! ## Design
//!
//! - **Typed access**: `get::<T>()`, `get_mut::<T>()`, `insert::<T>(val)`.
//! - **Params vs State**: `Params<T>` and `State<T>` are wrappers with distinct
//!   `TypeId`s so a tool's config and runtime state can coexist.
//! - **Serialization**: Registered types (via `register_params` / `register_state`)
//!   are serialized by category (`"params"` / `"state"`). Ephemeral types
//!   (e.g., `Cwd`) are silently skipped.
//! - **String-keyed access**: `get_json` / `set_json` for dynamic access by
//!   category + key string — used by the gRPC `SetToolOptions` / `GetToolOptions`
//!   RPCs.
use crate::computer::types::{AsyncFileSystem, TerminalBackend};
use crate::notification::types::ToolNotificationHandle;
use serde::Serialize;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
/// Marker trait for types that can be stored in `Resources`.
///
/// Each implementor must provide a unique `ID` string of the form
/// `"namespace.Name"` (e.g., `"grok_build.ReadFile"`). The ID is used as
/// the serialization key when persisting resources.
///
/// Use the `register_resource!` macro to implement this.
pub trait ResourceType: Any + 'static {
    /// Unique identifier, e.g. `"grok_build.ReadFile"`.
    const ID: &'static str;
    /// Additional semantic validation for finalize-time params.
    fn validate_params_value(
        _: &Self,
    ) -> Result<(), crate::types::params_validation::ParamValidationError> {
        Ok(())
    }
}
/// `()` implements `ResourceType` with an empty ID.
/// Used as `type Params = ()` for tools that have no configuration.
impl ResourceType for () {
    const ID: &'static str = "";
}
/// Implement `ResourceType` for a type with an explicit namespace and name.
///
/// ```ignore
/// register_resource!("grok_build", "ReadFile", ReadHistory);
/// ```
///
/// This generates:
/// ```ignore
/// impl ResourceType for ReadHistory {
///     const ID: &'static str = "grok_build.ReadFile";
/// }
/// ```
#[macro_export]
macro_rules! register_resource {
    ($namespace:literal, $name:literal, $ty:ty) => {
        impl $crate::types::resources::ResourceType for $ty {
            const ID: &'static str = concat!($namespace, ".", $name);
        }
    };
}
/// Wrapper for tool *configuration* / *parameters* stored in Resources.
///
/// `Params<T>` and `State<T>` have distinct `TypeId`s even for the same `T`,
/// so a tool's config and runtime state can coexist without collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params<T>(pub T);
impl<T: Default> Default for Params<T> {
    fn default() -> Self {
        Self(T::default())
    }
}
impl<T> std::ops::Deref for Params<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T> std::ops::DerefMut for Params<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
impl<T: Serialize> Serialize for Params<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Params<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Params)
    }
}
/// Wrapper for tool *runtime state* stored in Resources.
///
/// `State<T>` has a distinct `TypeId` from `Params<T>`, enabling both to
/// coexist in the same `Resources` container for the same inner type `T`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State<T>(pub T);
impl<T: Default> Default for State<T> {
    fn default() -> Self {
        Self(T::default())
    }
}
impl<T> std::ops::Deref for State<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T> std::ops::DerefMut for State<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
impl<T: Serialize> Serialize for State<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for State<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(State)
    }
}
/// Category for a registered resource — determines the top-level key in
/// serialized output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceCategory {
    Params,
    State,
}
impl ResourceCategory {
    fn as_str(&self) -> &'static str {
        match self {
            ResourceCategory::Params => "params",
            ResourceCategory::State => "state",
        }
    }
}
/// Type-erased serialize closure for a registered resource.
type SerializeFn = Box<dyn Fn(&(dyn Any + Send + Sync)) -> Option<serde_json::Value> + Send + Sync>;
/// Type-erased deserialize closure for a registered resource.
type DeserializeFn =
    Box<dyn Fn(serde_json::Value, &mut HashMap<TypeId, Box<dyn Any + Send + Sync>>) + Send + Sync>;
/// Metadata for a registered (serializable) resource.
///
/// Stores the `TypeId`, string key, category, and type-erased
/// serialize/deserialize closures so `Resources` can round-trip through JSON.
struct ResourceEntry {
    type_id: TypeId,
    /// The `ResourceType::ID` string (e.g., `"grok_build.ReadFile"`).
    id: String,
    category: ResourceCategory,
    /// Serialize the value stored at `type_id` to JSON.
    serialize_fn: SerializeFn,
    /// Deserialize a JSON value and insert it into the `data` map.
    deserialize_fn: DeserializeFn,
}
/// Type-safe heterogeneous container for tool resources.
///
/// Stores typed values indexed by `TypeId`. Registered types are serializable;
/// ephemeral types (inserted directly without registration) are skipped during
/// serialization.
///
/// All stored values must be `Send + Sync` so `Resources` itself is
/// `Send + Sync`. This is required because `ToolRegistry` (which owns
/// `Resources`) may be wrapped in a `RwLock` or `Mutex` by multi-threaded
/// hosts.
pub struct Resources {
    /// The actual storage: `TypeId` → `Box<dyn Any + Send + Sync>`.
    data: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    /// Registered (serializable) entries.
    entries: Vec<ResourceEntry>,
}
pub type SharedResources = Arc<Mutex<Resources>>;
impl Default for Resources {
    fn default() -> Self {
        Self::new()
    }
}
impl Resources {
    /// Create an empty resource container.
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
            entries: Vec::new(),
        }
    }
    /// Wrap this `Resources` into a `SharedResources` (`Arc<Mutex<Resources>>`).
    pub fn into_shared(self) -> SharedResources {
        Arc::new(Mutex::new(self))
    }
    /// Get a shared reference to a stored value.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.data
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref::<T>())
    }
    /// Get a shared reference to a stored value, or return
    /// a `custom("missing_resource", ...)` error with the type name if absent.
    pub fn require<T: Send + Sync + 'static>(&self) -> Result<&T, xai_tool_runtime::ToolError> {
        self.get::<T>().ok_or_else(|| {
            xai_tool_runtime::ToolError::custom(
                "missing_resource",
                format!("missing required resource: {}", std::any::type_name::<T>()),
            )
        })
    }
    /// Get a mutable reference to a stored value.
    pub fn get_mut<T: Send + Sync + 'static>(&mut self) -> Option<&mut T> {
        self.data
            .get_mut(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_mut::<T>())
    }
    /// Get a mutable reference, inserting `T::default()` if not present.
    pub fn get_or_default<T: Default + Send + Sync + 'static>(&mut self) -> &mut T {
        self.data
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut::<T>()
            .expect("TypeId collision: stored type doesn't match requested type")
    }
    /// Insert a typed value, replacing any existing value of the same type.
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) {
        self.data.insert(TypeId::of::<T>(), Box::new(value));
    }
    /// Remove a typed value, returning it if it existed.
    pub fn remove<T: Send + Sync + 'static>(&mut self) -> Option<T> {
        self.data
            .remove(&TypeId::of::<T>())
            .and_then(|boxed| (boxed as Box<dyn Any>).downcast::<T>().ok())
            .map(|boxed| *boxed)
    }
    /// Check if a value of type `T` is present.
    pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
        self.data.contains_key(&TypeId::of::<T>())
    }
    /// Register a `Params<T>` type for serialization under the `"params"` category.
    ///
    /// After registration, `Params<T>` values will be included in `serialize()`
    /// output and can be restored via `load_from()`.
    pub fn register_params<T>(&mut self)
    where
        T: ResourceType
            + serde::Serialize
            + for<'de> serde::Deserialize<'de>
            + Default
            + Send
            + Sync
            + 'static,
    {
        let type_id = TypeId::of::<Params<T>>();
        let id = T::ID.to_string();
        if self.entries.iter().any(|e| e.type_id == type_id) {
            return;
        }
        self.entries.push(ResourceEntry {
            type_id,
            id,
            category: ResourceCategory::Params,
            serialize_fn: Box::new(|any: &(dyn Any + Send + Sync)| {
                any.downcast_ref::<Params<T>>()
                    .and_then(|p| serde_json::to_value(p).ok())
            }),
            deserialize_fn: Box::new(
                |val: serde_json::Value, data: &mut HashMap<TypeId, Box<dyn Any + Send + Sync>>| {
                    if let Ok(p) = serde_json::from_value::<Params<T>>(val) {
                        data.insert(TypeId::of::<Params<T>>(), Box::new(p));
                    }
                },
            ),
        });
    }
    /// Register a `State<T>` type for serialization under the `"state"` category.
    ///
    /// After registration, `State<T>` values will be included in `serialize()`
    /// output and can be restored via `load_from()`.
    pub fn register_state<T>(&mut self)
    where
        T: ResourceType
            + serde::Serialize
            + for<'de> serde::Deserialize<'de>
            + Default
            + Send
            + Sync
            + 'static,
    {
        let type_id = TypeId::of::<State<T>>();
        let id = T::ID.to_string();
        if self.entries.iter().any(|e| e.type_id == type_id) {
            return;
        }
        self.entries.push(ResourceEntry {
            type_id,
            id,
            category: ResourceCategory::State,
            serialize_fn: Box::new(|any: &(dyn Any + Send + Sync)| {
                any.downcast_ref::<State<T>>()
                    .and_then(|s| serde_json::to_value(s).ok())
            }),
            deserialize_fn: Box::new(
                |val: serde_json::Value, data: &mut HashMap<TypeId, Box<dyn Any + Send + Sync>>| {
                    if let Ok(s) = serde_json::from_value::<State<T>>(val) {
                        data.insert(TypeId::of::<State<T>>(), Box::new(s));
                    }
                },
            ),
        });
    }
    /// Serialize all registered resources to a nested JSON structure.
    ///
    /// Output shape:
    /// ```json
    /// {
    ///   "params": {
    ///     "grok_build.Edit": { ... },
    ///   },
    ///   "state": {
    ///     "grok_build.ReadFile": { ... },
    ///     "grok_build.Todo": { ... },
    ///   }
    /// }
    /// ```
    ///
    /// Ephemeral types (not registered) are silently skipped.
    pub fn serialize(&self) -> serde_json::Value {
        let mut categories: HashMap<&str, serde_json::Map<String, serde_json::Value>> =
            HashMap::new();
        for entry in &self.entries {
            if let Some(boxed) = self.data.get(&entry.type_id)
                && let Some(val) = (entry.serialize_fn)(boxed.as_ref())
            {
                categories
                    .entry(entry.category.as_str())
                    .or_default()
                    .insert(entry.id.clone(), val);
            }
        }
        let mut top = serde_json::Map::new();
        for (cat, map) in categories {
            top.insert(cat.to_string(), serde_json::Value::Object(map));
        }
        serde_json::Value::Object(top)
    }
    /// Load registered resources from a previously serialized JSON structure.
    ///
    /// Expects the same shape as `serialize()` output:
    /// `{ "params": { ... }, "state": { ... } }`.
    ///
    /// Unknown keys are silently ignored. Missing keys leave the resource
    /// at its current value (or absent).
    pub fn load_from(&mut self, data: HashMap<String, HashMap<String, serde_json::Value>>) {
        for entry in &self.entries {
            let category_key = entry.category.as_str();
            if let Some(cat_map) = data.get(category_key)
                && let Some(val) = cat_map.get(&entry.id)
            {
                (entry.deserialize_fn)(val.clone(), &mut self.data);
            }
        }
    }
    /// Get a registered resource's value as JSON, by category and key.
    ///
    /// Used by the gRPC `GetToolOptions` RPC for dynamic access.
    pub fn get_json(&self, category: &str, key: &str) -> Option<serde_json::Value> {
        for entry in &self.entries {
            if entry.category.as_str() == category && entry.id == key {
                if let Some(boxed) = self.data.get(&entry.type_id) {
                    return (entry.serialize_fn)(boxed.as_ref());
                }
                return None;
            }
        }
        None
    }
    /// Set a registered resource's value from JSON, by category and key.
    ///
    /// Used by the gRPC `SetToolOptions` RPC for dynamic access.
    /// Returns `true` if a matching registration was found and the value was set.
    pub fn set_json(&mut self, category: &str, key: &str, val: serde_json::Value) -> bool {
        for entry in &self.entries {
            if entry.category.as_str() == category && entry.id == key {
                (entry.deserialize_fn)(val, &mut self.data);
                return true;
            }
        }
        false
    }
}
impl std::fmt::Debug for Resources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resources")
            .field("data_count", &self.data.len())
            .field("registered_entries", &self.entries.len())
            .finish()
    }
}
/// Current working directory for the session.
#[derive(Debug, Clone)]
pub struct Cwd(pub PathBuf);
/// Absolute path to the plan file for this session.
///
/// Set by the session layer (from `PlanModeTracker::plan_file_path()`);
/// read by `ExitPlanMode` to locate the plan on disk. When absent the
/// tool falls back to `Cwd/.grok/plan.md`.
#[derive(Debug, Clone)]
pub struct PlanFilePath(pub PathBuf);
/// Default plan-file path (relative to the workspace root) used when no
/// explicit [`PlanFilePath`] is set. Shared by the plan-mode tools.
pub const PLAN_FILE_RELATIVE_PATH: &str = ".grok/plan.md";
/// Resolve the session plan-file path from resources as `(absolute_target, display)`.
///
/// `absolute_target` is `Some` ONLY when the resolved path is absolute, so
/// callers that write/seed never create a file under the process CWD; it is
/// `None` for the display-only relative fallback. `display` is the
/// model-facing path string. Resolution: [`PlanFilePath`] (as-is), else
/// [`Cwd`]`/.grok/plan.md`, else the bare relative `.grok/plan.md`.
pub(crate) fn resolve_plan_file_path(res: &Resources) -> (Option<PathBuf>, String) {
    let path = if let Some(configured) = res.get::<PlanFilePath>() {
        configured.0.clone()
    } else if let Some(cwd) = res.get::<Cwd>() {
        cwd.0.join(PLAN_FILE_RELATIVE_PATH)
    } else {
        PathBuf::from(PLAN_FILE_RELATIVE_PATH)
    };
    let display = path.display().to_string();
    let absolute_target = path.is_absolute().then_some(path);
    (absolute_target, display)
}
/// Like [`resolve_plan_file_path`] but errors when no absolute target resolves.
pub(crate) fn require_plan_file_path(
    res: &Resources,
) -> Result<(PathBuf, String), xai_tool_runtime::ToolError> {
    let (target, display) = resolve_plan_file_path(res);
    let target = target.ok_or_else(|| {
        xai_tool_runtime::ToolError::custom(
            "missing_resource",
            "missing required resource: PlanFilePath or an absolute Cwd",
        )
    })?;
    Ok((target, display))
}
/// Stable display path for forked sessions.
///
/// When set, [`resolve_model_path`] rewrites absolute paths that start with
/// this prefix to the real [`Cwd`] (the on-disk worktree backing the fork).
/// This lets models keep using the original project path from conversation
/// history while all I/O hits the correct path on disk.
///
/// Inserted for forked sessions whose tool execution path differs from the
/// path the model should see.
#[derive(Debug, Clone)]
pub struct DisplayCwd(pub PathBuf);
/// Managed `Read`-deny glob patterns (e.g. `**/.env`, `**/*.pem`) from the
/// permission policy. The Grep tool passes these to ripgrep as `--glob '!<p>'`
/// excludes so a search never reads a path the policy forbids reading — whether
/// reached by a recursive walk or a `glob` arg that targets a denied file.
/// (An explicitly-passed denied `path` is blocked earlier by the permission
/// manager, since ripgrep searches explicit paths even against excludes.)
/// Empty when no managed Read denies apply.
#[derive(Debug, Clone, Default)]
pub struct DenyReadGlobs(pub Vec<String>);

/// Grep hardening for a toolset served to an untrusted client
/// (`turbo mcp serve`). When present, ripgrep ignores the operator's ripgrep
/// config and never follows symlinks, is killed if the search is abandoned,
/// and every result from a file `allow_file` rejects is dropped before it is
/// counted, truncated or rendered.
#[derive(Clone)]
pub struct ServedGrepPolicy {
    pub allow_file: std::sync::Arc<dyn Fn(&std::path::Path) -> bool + Send + Sync>,
}

impl std::fmt::Debug for ServedGrepPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServedGrepPolicy").finish_non_exhaustive()
    }
}

/// Edit bounds for a toolset served to an untrusted client (`turbo mcp serve`).
/// When present, `search_replace` refuses an edit that would produce a file of
/// more than `max_result_bytes`, builds details for at most
/// `max_detailed_edits` of its replacements, skips its line-count telemetry,
/// keeps a receipt with the file's previous content only if `record_receipts`
/// is set, and reports a workspace policy refusal as a `policy_denied` error.
#[derive(Debug, Clone, Copy)]
pub struct ServedEditPolicy {
    pub max_result_bytes: usize,
    pub max_detailed_edits: usize,
    pub record_receipts: bool,
}
/// Optional confinement root stamped into tool resources when `--confine` /
/// `--workspace-root` is set. Absolute model paths that do not resolve under
/// this root must be rejected (see [`path_is_under_confine_root`]) — never
/// passed through as-is the way unconfined absolute paths historically were.
#[derive(Debug, Clone)]
pub struct ConfineRoot(pub PathBuf);

/// Extra workspace roots attached to a session (ACP `additionalDirectories`).
///
/// These expand the write/read confine set without changing [`Cwd`], which
/// remains the base for relative paths. Empty means no extra roots.
#[derive(Debug, Clone, Default)]
pub struct AdditionalDirectories(pub Vec<PathBuf>);

/// Write-time path allowlist prefixes (relative, normalized). Empty = unrestricted.
///
/// Inserted from spawn `allowed_paths` so tools fail closed at write time (not
/// only at land). See subagent isolation docs / RC12 land allowlist.
#[derive(Debug, Clone, Default)]
pub struct AllowedWritePaths(pub Vec<String>);

/// Error when a write target is outside spawn `allowed_paths`.
#[derive(Debug, Clone)]
pub struct AllowedPathsViolation {
    pub path: String,
    pub allowed: Vec<String>,
}

/// Repo-root files crate-scoped tasks still need to pin (eol=lf, ignore).
const ROOT_METADATA_RELS: &[&str] = &[".gitattributes", ".gitignore"];

pub(crate) fn is_root_metadata_rel(rel: &str) -> bool {
    let n = rel.replace('\\', "/");
    let n = n.trim_start_matches("./");
    ROOT_METADATA_RELS.iter().any(|f| {
        if cfg!(windows) {
            n.eq_ignore_ascii_case(f)
        } else {
            n == *f
        }
    })
}

fn allowlist_respawn_hint(path: &str) -> String {
    let n = path.replace('\\', "/");
    let n = n.trim_start_matches("./");
    if is_root_metadata_rel(n) {
        return n.to_owned();
    }
    match n.split_once('/') {
        Some((first, _)) if !first.is_empty() => format!("{first}/"),
        _ => n.to_owned(),
    }
}

impl AllowedPathsViolation {
    pub fn code(&self) -> &'static str {
        "path_allowlist_violation"
    }

    pub fn message(&self) -> String {
        let hint = allowlist_respawn_hint(&self.path);
        format!(
            "write refused: path `{}` is outside allowed_paths {:?}. \
             Re-spawn with allowed_paths that includes this prefix, e.g. \
             allowed_paths=[..., \"{hint}\"], or omit allowed_paths for \
             unrestricted writes (isolation=none parent files like \
             .gitattributes / .gitignore need an explicit allowlist prefix).",
            self.path, self.allowed
        )
    }

    pub fn into_tool_error(self) -> xai_tool_runtime::ToolError {
        xai_tool_runtime::ToolError::custom(self.code(), self.message())
    }
}

/// Fail closed when `allowed` is non-empty and `resolved` is not under `cwd`
/// relative to any allowlist prefix.
///
/// Empty / missing allowlist = unrestricted (Ok). Paths that cannot be made
/// relative to `cwd` are refused when an allowlist is active.
pub fn enforce_allowed_write_paths(
    cwd: &std::path::Path,
    resolved: &std::path::Path,
    allowed: &[String],
) -> Result<(), AllowedPathsViolation> {
    if allowed.is_empty() {
        return Ok(());
    }
    // Canonicalize before the relative-prefix check so a symlink under an
    // allowed prefix (Schedules -> ~/.ssh) cannot lexical-match then write
    // through the link (F66). Fail closed when no ancestor can be resolved.
    // Containment follows `path_is_under_confine_root`: the case-folded
    // `compare` forms would also take in a case-distinct sibling of `cwd` in a
    // case-sensitive directory.
    let cwd_r = canonicalize_with_ancestor_walk(cwd);
    let res_r = canonicalize_with_ancestor_walk(resolved);
    if res_r.lexical_only || !resolved_is_under(&res_r, &cwd_r) {
        return Err(AllowedPathsViolation {
            path: resolved.display().to_string(),
            allowed: allowed.to_vec(),
        });
    }
    // By component count rather than `strip_prefix`: a resolved path longer than
    // MAX_PATH keeps its `\\?\` spelling while `cwd` does not. Both sides are
    // folded alike, so they split into components alike.
    let rel: PathBuf = fold_for_compare(&res_r.display)
        .components()
        .skip(fold_for_compare(&cwd_r.display).components().count())
        .collect();
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    // Collapse `..` / reject absolute-style relative escapes so write-time
    // gates match land_subagent normalize_allowlist_path semantics.
    let Some(norm) = normalize_rel_allowlist_path(&rel_str) else {
        return Err(AllowedPathsViolation {
            path: rel_str,
            allowed: allowed.to_vec(),
        });
    };
    let allowed_ok = allowed.iter().any(|prefix| {
        let Some(p) = normalize_rel_allowlist_path(prefix) else {
            return false;
        };
        // Windows: case-fold prefix match (NTFS); same rule as land path_is_allowed.
        if cfg!(windows) {
            let norm_l = norm.to_ascii_lowercase();
            let p_l = p.to_ascii_lowercase();
            norm_l == p_l || norm_l.starts_with(&(p_l + "/"))
        } else {
            norm == p || norm.starts_with(&(p + "/"))
        }
    });
    if allowed_ok {
        return Ok(());
    }
    Err(AllowedPathsViolation {
        path: norm,
        allowed: allowed.to_vec(),
    })
}

/// Normalize a cwd-relative path for allowlist matching (same rules as
/// land_subagent `normalize_allowlist_path`, kept here to avoid types↔impl cycles).
fn normalize_rel_allowlist_path(path: &str) -> Option<String> {
    let mut s = path.trim().replace('\\', "/");
    if s.is_empty() {
        return None;
    }
    if s.starts_with('/') || s.starts_with("//") {
        return None;
    }
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        return None;
    }
    while s.starts_with("./") {
        s = s[2..].to_owned();
    }
    if s == "." {
        return None;
    }
    if s.ends_with('/') && s.len() > 1 {
        s.pop();
    }
    let mut stack: Vec<&str> = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if stack.is_empty() {
                    return None;
                }
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        return None;
    }
    Some(stack.join("/"))
}

/// Whether `path` is rooted for model-path rewriting.
///
/// On Windows, `Path::is_absolute()` is false for POSIX spellings like
/// `/home/user/project` (no drive prefix). Models still emit those forms from
/// remote/container sessions and conversation history; treat a leading
/// [`std::path::Component::RootDir`] as rooted so display-cwd strip works
/// cross-platform (same rule as permission `edit_target_protection`).
fn path_is_rooted(path: &std::path::Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    matches!(
        path.components().next(),
        Some(std::path::Component::RootDir)
    )
}

/// True when `path` looks like an isolated product worktree checkout
/// (`…/.grok/worktrees/…/subagent-…` or temp `grok-subagent-worktrees/…`).
///
/// Used to refuse DisplayCwd remaps that would fold an authorized worktree
/// write onto a shared parent checkout (P0 isolation_fallback).
pub fn path_looks_like_isolated_worktree(path: &std::path::Path) -> bool {
    let s = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let has_subagent = s.contains("/subagent-");
    let under_product = s.contains("/.grok/worktrees/");
    let under_temp = s.contains("grok-subagent-worktrees");
    has_subagent && (under_product || under_temp)
}

/// Resolve a model-provided path, rewriting absolute paths from conversation
/// history when [`DisplayCwd`] is set.
///
/// - If `display_cwd` is `None`, falls back to `cwd.join(input)`.
/// - If `input` starts with the `display_cwd` prefix, strips it and joins
///   the suffix onto `cwd` (the real worktree path).
/// - If `input` is already under `cwd`, it is kept (never remapped).
/// - If `input` is an isolated worktree path (`…/.grok/worktrees/…/subagent-…`),
///   it is kept even when `display_cwd` matches that prefix. The inverted
///   DisplayCwd=worktree / Cwd=parent case used to fold those writes onto
///   the shared parent checkout.
/// - If `input` is absolute but doesn't match, returns it as-is
///   (**unconfined** default — see [`resolve_model_path_confined`] when a
///   confine root is active).
/// - Leading `~`/`~/` is expanded to the current user's home directory
///   before applying the above rules. `~username` is not expanded.
/// - Relative paths are always joined onto `cwd`.
pub fn resolve_model_path(
    cwd: &std::path::Path,
    display_cwd: Option<&std::path::Path>,
    input: &str,
) -> PathBuf {
    let input = sanitize_model_path_arg(input);
    let expanded = shellexpand::tilde(input);
    let input_path = std::path::Path::new(expanded.as_ref());
    // Already under the real tool CWD (the worktree): never fold via DisplayCwd.
    if path_is_rooted(input_path) && (input_path == cwd || input_path.starts_with(cwd)) {
        return input_path.to_path_buf();
    }
    // Isolated worktree spelling must stay on that tree. An inverted
    // DisplayCwd (worktree) + Cwd (parent) would otherwise strip the
    // worktree prefix and join onto the parent — the P0 remap.
    if path_is_rooted(input_path) && path_looks_like_isolated_worktree(input_path) {
        return input_path.to_path_buf();
    }
    if let Some(display) = display_cwd
        && path_is_rooted(input_path)
    {
        if let Ok(suffix) = input_path.strip_prefix(display) {
            return cwd.join(suffix);
        }
        // Rooted but not under display_cwd: keep the model spelling as a path
        // (POSIX absolute on Windows is still a distinct spelling for confine).
        return input_path.to_path_buf();
    }
    if !path_is_rooted(input_path) && !expanded.is_empty() {
        // Model may omit the leading `/` while still intending a path under
        // display_cwd / cwd (e.g. absolute-looking without root).
        let as_absolute = std::path::PathBuf::from(format!("/{}", expanded.as_ref()));
        let effective_base = display_cwd.unwrap_or(cwd);
        if as_absolute.starts_with(effective_base)
            && let Ok(suffix) = as_absolute.strip_prefix(effective_base)
        {
            return cwd.join(suffix);
        }
    }
    // Rooted model path that did not match display_cwd (or display was unset):
    // keep the model spelling. On Unix, `cwd.join("/abs")` would replace with
    // `/abs`; on Windows a leading `/` is only RootDir (not `is_absolute`), so
    // join invents a drive-relative path like `H:\wrong\...`. That breaks
    // not-found messages, confine checks, and skill-path suggestions.
    if path_is_rooted(input_path) {
        return input_path.to_path_buf();
    }
    cwd.join(input_path)
}

/// Like [`resolve_model_path`], but rejects absolute paths that do not
/// canonicalise under `confine_root`.
///
/// Path-prefix confinement (not globs): globs cannot safely express "not under
/// this root" on Windows (drive letters, case, `\\?\`, mixed separators).
/// Relative inputs are joined onto `cwd` first, then checked.
pub fn resolve_model_path_confined(
    cwd: &std::path::Path,
    display_cwd: Option<&std::path::Path>,
    confine_root: &std::path::Path,
    input: &str,
) -> Result<PathBuf, String> {
    resolve_model_path_confined_to_roots(cwd, display_cwd, &[confine_root.to_path_buf()], input)
}

/// Like [`resolve_model_path_confined`], but the path may lie under **any**
/// root in `roots` (primary cwd / confine plus ACP additional directories).
///
/// Empty `roots` is unconfined (legacy default).
pub fn resolve_model_path_confined_to_roots(
    cwd: &std::path::Path,
    display_cwd: Option<&std::path::Path>,
    roots: &[PathBuf],
    input: &str,
) -> Result<PathBuf, String> {
    let resolved = resolve_model_path(cwd, display_cwd, input);
    if roots.is_empty() || path_is_under_any_root(&resolved, roots) {
        Ok(resolved)
    } else {
        Err(format!(
            "path `{}` is outside the confine roots {}",
            resolved.display(),
            format_confine_roots(roots)
        ))
    }
}

fn format_confine_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|r| format!("`{}`", r.display()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Collect the effective write/read confine set for a session.
///
/// `confine_root` is the **session** isolation worktree (tighter than process).
/// Process `--confine` / `GROK_CONFINE` roots are consulted here; do **not**
/// pass the first process root as `confine_root` or sibling process roots
/// are dropped.
///
/// - No extras, no session confine, no process → empty (unconfined).
/// - No extras, no session confine, process roots → those process roots.
/// - Session confine present → `[session, ...additional]` (isolation wins).
/// - Extras, no session, no process → `[cwd, ...additional]`.
/// - Extras, no session, process roots → `[process..., ...additional]`.
pub fn collect_write_roots(
    cwd: &std::path::Path,
    confine_root: Option<&std::path::Path>,
    additional: &[PathBuf],
) -> Vec<PathBuf> {
    let process = process_confine_roots();
    let mut roots = Vec::with_capacity(additional.len() + process.len() + 1);
    if let Some(p) = confine_root {
        roots.push(p.to_path_buf());
    } else {
        roots.extend(process.iter().cloned());
    }
    if roots.is_empty() {
        if additional.is_empty() {
            return Vec::new();
        }
        roots.push(cwd.to_path_buf());
    }
    for extra in additional {
        if !roots.iter().any(|r| paths_equal_for_confine(r, extra)) {
            roots.push(extra.clone());
        }
    }
    roots
}

/// Whether `a` and `b` name the same directory, by the rules of
/// [`path_is_under_confine_root`]. Equal case-folded `compare` forms would
/// merge case-distinct directories in a case-sensitive parent. Two spellings
/// with no resolvable ancestor are the same only when they are identical.
fn paths_equal_for_confine(a: &std::path::Path, b: &std::path::Path) -> bool {
    let a = canonicalize_with_ancestor_walk(a);
    let b = canonicalize_with_ancestor_walk(b);
    if a.lexical_only || b.lexical_only {
        return a.display == b.display;
    }
    resolved_is_under(&a, &b) && resolved_is_under(&b, &a)
}

/// Resolve a write-tool path: when a session [`ConfineRoot`] **or** process
/// confine root is active, use [`resolve_model_path_confined`] so out-of-root
/// absolutes fail at resolve time (not only at ConfinedFs write). Without any
/// confine root, same as [`resolve_model_path`].
pub fn resolve_write_model_path(
    cwd: &std::path::Path,
    display_cwd: Option<&std::path::Path>,
    confine_root: Option<&std::path::Path>,
    input: &str,
) -> Result<PathBuf, String> {
    resolve_write_model_path_in_roots(
        cwd,
        display_cwd,
        &collect_write_roots(cwd, confine_root, &[]),
        input,
    )
}

/// Resolve a write path against a precomputed root set (cwd + extras).
pub fn resolve_write_model_path_in_roots(
    cwd: &std::path::Path,
    display_cwd: Option<&std::path::Path>,
    roots: &[PathBuf],
    input: &str,
) -> Result<PathBuf, String> {
    if roots.is_empty() {
        Ok(resolve_model_path(cwd, display_cwd, input))
    } else {
        resolve_model_path_confined_to_roots(cwd, display_cwd, roots, input)
    }
}

/// Outcome of reducing a path for permission / confine comparisons.
///
/// Both the confine check and managed path-rule matching MUST use this helper
/// (via [`path_is_under_confine_root`] / [`canonical_path_for_permission`]) so
/// the two layers cannot drift on `..`, 8.3 short names, or non-existent write
/// targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPermissionPath {
    /// Form used for prefix / equality compares (case-folded on Windows).
    pub compare: PathBuf,
    /// Human-readable form (dunce-simplified, original case) for denial text
    /// and `confine_violation` events.
    pub display: PathBuf,
    /// `true` when **no** existing ancestor could be `fs::canonicalize`'d.
    ///
    /// Under confine this MUST deny (fail closed). A pure lexical clean still
    /// collapses `..` but leaves 8.3 aliases (`MAINRE~1`) and symlink targets
    /// untouched — that is exactly how prior escapes landed real files in a
    /// protected checkout. Do not "simplify" this flag away.
    pub lexical_only: bool,
}

/// Reduce `path` to a canonical form for permission decisions.
///
/// - Resolves `.` / `..`, 8.3 short names, symlinks, and junctions via
///   `std::fs::canonicalize`, then strips the `\\?\` prefix with
///   `dunce::simplified`.
/// - **Non-existent write targets** (the common case): walk up to the nearest
///   existing ancestor, canonicalize that, then re-join the remaining
///   components (lexically cleaned). `…/MAINRE~1/new.txt` therefore resolves
///   its parent to `…/Main Repo` and is correctly denied by a
///   `Main Repo/**` rule.
/// - A `..` in that tail can step back into directories that exist, so the
///   tail is collapsed and the result resolved again: a symlink named after
///   the `..` is followed like any other.
/// - If no ancestor can be canonicalized, fall back to a lexical clean only
///   and set [`CanonicalPermissionPath::lexical_only`]. Callers under confine
///   treat that as outside the root — never as allow.
pub fn canonicalize_for_permission(path: &std::path::Path) -> CanonicalPermissionPath {
    let resolved = canonicalize_with_ancestor_walk(path);
    let compare = fold_for_compare(&resolved.display);
    CanonicalPermissionPath {
        compare,
        display: resolved.display,
        lexical_only: resolved.lexical_only,
    }
}

/// Shared reduction used by confine and path-rule matching.
/// Prefer [`canonicalize_for_permission`] when you need the full struct.
pub fn canonical_path_for_permission(path: &std::path::Path) -> PathBuf {
    canonicalize_for_permission(path).compare
}

/// True when `path` is the confine root or a descendant after
/// [`canonicalize_for_permission`].
///
/// **Fail closed:** if `path` cannot be resolved via any existing ancestor
/// (`lexical_only`), returns `false` and logs why. An unresolvable path must
/// never mean "allow" under confine — that was the previous escape hatch.
///
/// **Letter case:** components that exist are compared exactly, in the
/// spelling `fs::canonicalize` returns for them, so a root typed in another
/// case still matches its directory. Folding case there is an escape: an NTFS
/// directory can be case-sensitive (`fsutil file setCaseSensitiveInfo`, or
/// created from WSL), and inside one `C:\work\app` and `C:\work\APP` are
/// different directories. Where neither side exists yet there is no on-disk
/// spelling, and ASCII case is ignored only when the deepest directory both
/// sides share is known not to be case-sensitive (a flag that cannot be read
/// counts as case-sensitive), so a case variant of a root removed from a
/// case-sensitive directory is still refused. Drive letters and server or share
/// names never distinguish case, and the `\\?\` spelling of a drive or share
/// matches the plain one.
pub fn path_is_under_confine_root(path: &std::path::Path, root: &std::path::Path) -> bool {
    let path_r = canonicalize_with_ancestor_walk(path);
    if path_r.lexical_only {
        // Fail closed: no fs-backed resolution → treat as outside the root.
        // Lexical-only collapse of `..` is not enough (8.3 / symlink escapes).
        tracing::warn!(
            path = %path.display(),
            resolved = %path_r.display.display(),
            root = %root.display(),
            "confine: path has no canonicalizable ancestor; denying (fail closed)"
        );
        return false;
    }
    resolved_is_under(&path_r, &canonicalize_with_ancestor_walk(root))
}

/// The comparison behind [`path_is_under_confine_root`], on paths already
/// reduced by [`canonicalize_with_ancestor_walk`]. It ignores `lexical_only`;
/// callers decide what an unresolvable path means.
fn resolved_is_under(path: &ResolvedPath, root: &ResolvedPath) -> bool {
    // Before this index every component exists on at least one side and must
    // match exactly, so by the time a later component is compared, the first
    // `shared_on_disk` components name a directory both sides share on disk.
    let shared_on_disk = path.on_disk.max(root.on_disk);
    let mut shared_dir_ignores_case = None;
    // Component-wise so `C:\work` does not match `C:\work-evil\file`.
    let mut path_components = path.display.components();
    root.display.components().enumerate().all(|(index, want)| {
        let Some(got) = path_components.next() else {
            return false;
        };
        confine_components_match(got, want, false)
            || (index >= shared_on_disk
                && confine_components_match(got, want, true)
                && *shared_dir_ignores_case.get_or_insert_with(|| {
                    let shared: PathBuf = path.display.components().take(shared_on_disk).collect();
                    dir_case_sensitivity(&shared) == Some(false)
                }))
    })
}

/// One component of a resolved path against the same position in a resolved
/// root. `ignore_ascii_case` relaxes directory entry names only.
fn confine_components_match(
    got: std::path::Component<'_>,
    want: std::path::Component<'_>,
    ignore_ascii_case: bool,
) -> bool {
    use std::path::{Component, Prefix};
    match (got, want) {
        (Component::Prefix(got), Component::Prefix(want)) => {
            let fold = |s: &std::ffi::OsStr| s.to_string_lossy().to_lowercase();
            // Drive letters and server or share names never distinguish case,
            // and `\\?\C:` is the same volume as `C:`: an existing path longer
            // than MAX_PATH canonicalizes to the verbatim spelling.
            match (got.kind(), want.kind()) {
                (
                    Prefix::Disk(got) | Prefix::VerbatimDisk(got),
                    Prefix::Disk(want) | Prefix::VerbatimDisk(want),
                ) => got.eq_ignore_ascii_case(&want),
                (
                    Prefix::UNC(got_server, got_share) | Prefix::VerbatimUNC(got_server, got_share),
                    Prefix::UNC(want_server, want_share)
                    | Prefix::VerbatimUNC(want_server, want_share),
                ) => fold(got_server) == fold(want_server) && fold(got_share) == fold(want_share),
                _ => fold(got.as_os_str()) == fold(want.as_os_str()),
            }
        }
        // ASCII only: Unicode lowercasing does not agree with the volume's
        // upcase table for every character, and a false match admits a path.
        (Component::Normal(got), Component::Normal(want)) => {
            got == want
                || (ignore_ascii_case
                    && got
                        .as_encoded_bytes()
                        .eq_ignore_ascii_case(want.as_encoded_bytes()))
        }
        (got, want) => got == want,
    }
}

/// Whether names in the existing directory `dir` are case-sensitive: the NTFS
/// flag `fsutil file setCaseSensitiveInfo` sets, and WSL sets on directories it
/// creates. `None` when the flag cannot be read.
#[cfg(windows)]
fn dir_case_sensitivity(dir: &std::path::Path) -> Option<bool> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
        FileCaseSensitiveInfo, GetFileInformationByHandleEx,
    };
    // From winnt.h. The `windows` crate declares it under a feature this crate
    // does not enable.
    const FILE_CS_FLAG_CASE_SENSITIVE_DIR: u32 = 0x1;

    let handle = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(dir)
        .ok()?;
    let mut info = FILE_CASE_SENSITIVE_INFO { Flags: 0 };
    // SAFETY: `info` is a live, writable FILE_CASE_SENSITIVE_INFO and the size
    // passed is its size; `handle` stays open for the whole call.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(handle.as_raw_handle()),
            FileCaseSensitiveInfo,
            (&raw mut info).cast(),
            size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
        )
    }
    .ok()?;
    Some(info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR != 0)
}

#[cfg(not(windows))]
fn dir_case_sensitivity(_dir: &std::path::Path) -> Option<bool> {
    None
}

/// True when `path` is under **any** of `roots` after
/// [`canonicalize_for_permission`]. Empty `roots` is never a match (callers
/// treat empty as unconfined before calling this).
pub fn path_is_under_any_root(path: &std::path::Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| path_is_under_confine_root(path, root))
}

/// A path reduced by [`canonicalize_with_ancestor_walk`].
struct ResolvedPath {
    /// Canonical existing prefix, then the lexically cleaned tail that does
    /// not exist yet.
    display: PathBuf,
    /// How many leading components of `display` came from `fs::canonicalize`.
    /// Only those carry their on-disk spelling; the rest are as the caller
    /// wrote them.
    on_disk: usize,
    /// No ancestor could be canonicalized.
    lexical_only: bool,
}

/// Canonicalise via `fs::canonicalize` when the path exists; otherwise walk
/// up to the nearest existing ancestor, canonicalize that, and re-join the
/// non-existent tail.
fn canonicalize_with_ancestor_walk(path: &std::path::Path) -> ResolvedPath {
    // Fast path: path exists (or is a symlink the OS can resolve).
    if let Ok(display) = dunce::canonicalize(path) {
        return ResolvedPath {
            on_disk: display.components().count(),
            display,
            lexical_only: false,
        };
    }

    // Write targets usually do not exist yet. Walk up until canonicalize
    // succeeds, then push the remaining components back on. Without this
    // walk, a pure lexical clean of `…/MAINRE~1/new.txt` leaves the 8.3
    // segment intact and deny/confine globs keyed on the long name miss it.
    let mut cursor = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut tail_has_parent_dir = false;
    loop {
        let parent = match cursor.parent() {
            Some(p) if p != cursor.as_path() => p.to_path_buf(),
            _ => break,
        };
        // `Path::file_name` is `None` for a trailing `..`, which used to drop
        // the component and rewrite `docs/../secret.txt` (docs absent) as
        // `docs/secret.txt` — an allowlist and confine-root bypass on hosts
        // that cannot canonicalize through a missing directory (Linux, macOS).
        // Windows never reached this: its path API collapses `docs\..` before
        // the filesystem sees it.
        match cursor.components().next_back() {
            Some(std::path::Component::Normal(name)) => tail.push(name.to_os_string()),
            Some(std::path::Component::ParentDir) => {
                tail.push(std::ffi::OsString::from(".."));
                tail_has_parent_dir = true;
            }
            _ => {}
        }
        cursor = parent;
        if let Ok(mut out) = dunce::canonicalize(&cursor) {
            let on_disk = out.components().count();
            // Re-join farthest-parent → leaf (tail was pushed leaf-first).
            for component in tail.into_iter().rev() {
                out.push(component);
            }
            // Collapse `.` / `..` that lived only in the non-existent tail.
            let cleaned = lexical_clean(&out);
            if tail_has_parent_dir {
                // The `..` may have stepped back into directories that exist,
                // and nothing after it was resolved: `root/missing/../link/x`
                // would keep `link` as spelled while a write follows the
                // symlink. The cleaned path has no `..` left, so this recurses
                // once at most.
                return canonicalize_with_ancestor_walk(&cleaned);
            }
            return ResolvedPath {
                display: cleaned,
                on_disk,
                lexical_only: false,
            };
        }
    }

    // No ancestor could be canonicalized (missing drive, deleted volume, …).
    // Lexical fallback still collapses `..` for best-effort messages, but
    // callers MUST treat `lexical_only = true` as deny under confine.
    //
    // Cost of "simplifying" this to always-allow-on-lexical: WP-H4 escapes —
    // `…/wt/../Main Repo/d.txt` and `…/MAINRE~1/h.txt` both wrote into a
    // denied checkout because short names and unresolved ancestors never
    // matched the deny glob lexically. Do not reintroduce that.
    tracing::debug!(
        path = %path.display(),
        "canonicalize_for_permission: no ancestor could be canonicalized; lexical-only fallback"
    );
    ResolvedPath {
        display: lexical_clean(path),
        on_disk: 0,
        lexical_only: true,
    }
}

/// Collapse `.` and `..` without touching the filesystem. Does **not** expand
/// 8.3 short names or follow symlinks — that requires the ancestor walk above.
fn lexical_clean(path: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn fold_for_compare(path: &std::path::Path) -> PathBuf {
    #[cfg(windows)]
    {
        // Case-insensitive compare on Windows only.
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Process-wide confine roots set at CLI startup (`--confine` repeatable /
/// `GROK_CONFINE` path list). The permission manager and path resolvers consult
/// this so confinement is enforced even when a tool context was built without a
/// [`ConfineRoot`] resource. Empty = unconfined (legacy default).
static PROCESS_CONFINE_ROOTS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();

/// Env var exported by `--confine` so nested turbo / MCP / hooks inherit the
/// roots. Portable encoding is `;`-separated absolute paths (never `:` —
/// Windows drive letters). On Unix, a `GROK_CONFINE` value with no `;` may
/// also be split on `:` (PATH-style).
pub const ENV_GROK_CONFINE: &str = "GROK_CONFINE";
/// Marker set alongside [`ENV_GROK_CONFINE`] when this process applied confine.
pub const ENV_GROK_CONFINE_INHERIT: &str = "GROK_CONFINE_INHERIT";
/// Shell confine enforcement mode env. Default under confine is fail-closed
/// (`fail-closed`); set to `operand` to opt out to the legacy write-operand
/// scan only (unknown programs allowed when no write operand is extracted).
pub const ENV_GROK_CONFINE_SHELL_MODE: &str = "GROK_CONFINE_SHELL_MODE";

/// Split a `GROK_CONFINE` env value into paths.
///
/// `;` always splits. On Unix, `:` also splits when no `;` is present. Windows
/// never splits on `:` so `C:\work` stays one root.
pub fn parse_confine_path_list(raw: &str) -> Vec<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let delim = if raw.contains(';') {
        ';'
    } else if cfg!(not(windows)) && raw.contains(':') {
        ':'
    } else {
        return vec![PathBuf::from(raw)];
    };
    raw.split(delim)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Join confine roots for `GROK_CONFINE` export. Always `;`, including on Unix.
pub fn join_confine_path_list(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join(";")
}

/// Stamp the confine roots for this process. Idempotent first-write-wins (CLI
/// startup is the only writer). Canonicalise and verify each path is a
/// directory *before* calling this.
pub fn set_process_confine_roots(roots: Vec<PathBuf>) {
    let _ = PROCESS_CONFINE_ROOTS.set(roots);
}

/// Stamp a single confine root (tests / single `--confine`).
pub fn set_process_confine_root(root: PathBuf) {
    set_process_confine_roots(vec![root]);
}

/// All process confine roots. Empty slice = unconfined.
pub fn process_confine_roots() -> &'static [PathBuf] {
    PROCESS_CONFINE_ROOTS
        .get()
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

/// First process confine root, if any.
///
/// Display / legacy single-field (`confineRoot`) only. Bound checks must use
/// [`process_confine_roots`] / [`path_is_under_any_root`] — returning only
/// the first root as the write jail is a lie when several were stamped.
pub fn process_confine_root() -> Option<&'static PathBuf> {
    process_confine_roots().first()
}

// ── RC13 Wave A: fail-closed write roots (cwd / confine tombstones) ─────────

/// Why a write was refused because a root directory is gone.
///
/// Used when a session CWD or `--confine` root was deleted out from under the
/// agent (soft-preserve prune, discard, worktree tombstone). Tools must surface
/// these as `cwd_missing` / `worktree_tombstone` rather than cascading IO noise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteRootError {
    /// Session working directory is missing or not a directory.
    CwdMissing { path: PathBuf },
    /// Confine / worktree root is missing or not a directory.
    ConfineRootMissing { path: PathBuf },
}

impl WriteRootError {
    /// Stable tool error code (`cwd_missing` or `worktree_tombstone`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::CwdMissing { .. } => "cwd_missing",
            Self::ConfineRootMissing { .. } => "worktree_tombstone",
        }
    }

    /// Human-readable detail for tool / computer errors.
    pub fn message(&self) -> String {
        match self {
            Self::CwdMissing { path } => format!(
                "cwd_missing: session working directory is missing or not a directory: `{}` \
                 (error_class=worktree_tombstone). The worktree may have been pruned, discarded, \
                 or never created. Do not continue writing; recover via land/diff/open --restore \
                 or file developer_log.",
                path.display()
            ),
            Self::ConfineRootMissing { path } => format!(
                "worktree_tombstone: confine root is missing or not a directory: `{}` \
                 (error_class=worktree_tombstone / cwd_missing). Writes are fail-closed while the \
                 root is gone. Recover via land/diff/open --restore or file developer_log.",
                path.display()
            ),
        }
    }

    /// Convert to a runtime [`xai_tool_runtime::ToolError`].
    pub fn into_tool_error(self) -> xai_tool_runtime::ToolError {
        xai_tool_runtime::ToolError::custom(self.code(), self.message())
    }

    /// Convert to a computer-layer IO error (for LocalFs / ConfinedFs).
    pub fn into_computer_error(self) -> crate::computer::types::ComputerError {
        crate::computer::types::ComputerError::io_with_kind(
            self.message(),
            std::io::ErrorKind::NotFound,
        )
    }
}

/// True when `path` exists and is a directory (symlink-to-dir counts).
fn path_is_existing_dir(path: &std::path::Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => m.is_dir(),
        Err(_) => false,
    }
}

/// Fail closed before any write when the session CWD and/or confine root is gone.
///
/// - `cwd`: session working directory (required for relative path resolution).
/// - `confine_root`: optional `--confine` / [`ConfineRoot`] / process root.
///
/// Call this from write tools **and** FS choke points so a tombstoned worktree
/// cannot accept phantom writes that later land empty.
pub fn enforce_write_roots(
    cwd: Option<&std::path::Path>,
    confine_root: Option<&std::path::Path>,
) -> Result<(), WriteRootError> {
    if let Some(cwd) = cwd
        && !path_is_existing_dir(cwd)
    {
        return Err(WriteRootError::CwdMissing {
            path: cwd.to_path_buf(),
        });
    }
    if let Some(root) = confine_root
        && !path_is_existing_dir(root)
    {
        return Err(WriteRootError::ConfineRootMissing {
            path: root.to_path_buf(),
        });
    }
    Ok(())
}

/// Fail closed for a write under `cwd`, consulting the process confine root
/// when set (and an optional resource-level root that may differ).
///
/// Prefer this from tools that already resolved session CWD.
pub fn enforce_write_path(
    cwd: &std::path::Path,
    confine_root: Option<&std::path::Path>,
) -> Result<(), WriteRootError> {
    enforce_write_path_in_roots(cwd, &collect_write_roots(cwd, confine_root, &[]))
}

/// Fail closed when `cwd` is gone, or when every confine root is gone.
/// A missing extra root does not tombstone writes to remaining live roots.
pub fn enforce_write_path_in_roots(
    cwd: &std::path::Path,
    roots: &[PathBuf],
) -> Result<(), WriteRootError> {
    if !path_is_existing_dir(cwd) {
        return Err(WriteRootError::CwdMissing {
            path: cwd.to_path_buf(),
        });
    }
    if roots.is_empty() {
        return Ok(());
    }
    if roots.iter().any(|r| path_is_existing_dir(r)) {
        return Ok(());
    }
    Err(WriteRootError::ConfineRootMissing {
        path: roots[0].clone(),
    })
}

/// Process-confine-only preflight (LocalFs has no session Cwd resource).
/// A missing extra process root does not tombstone writes to remaining live
/// roots; all gone is fail-closed.
pub fn enforce_process_confine_root_exists() -> Result<(), WriteRootError> {
    let roots = process_confine_roots();
    if roots.is_empty() {
        return Ok(());
    }
    if roots.iter().any(|r| path_is_existing_dir(r)) {
        return Ok(());
    }
    Err(WriteRootError::ConfineRootMissing {
        path: roots[0].clone(),
    })
}

/// Shell confinement enforcement level active for this process.
///
/// Reported on the streaming-json `start` event so harnesses record what was
/// actually enforced rather than assuming a hard OS boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfineShellEnforcement {
    /// Unknown / unmodelled programs are denied (default under `--confine`).
    FailClosed,
    /// Legacy: only extracted write/`cd` operands are checked; unknown programs
    /// with empty operand lists are allowed.
    OperandScan,
}

impl ConfineShellEnforcement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FailClosed => "fail-closed",
            Self::OperandScan => "operand-scan",
        }
    }
}

/// Resolve shell confine enforcement for the current process.
///
/// Default is fail-closed whenever a confine root is active. Callers may opt
/// into the legacy operand-scan via `GROK_CONFINE_SHELL_MODE=operand`.
pub fn confine_shell_enforcement() -> ConfineShellEnforcement {
    if process_confine_roots().is_empty() {
        return ConfineShellEnforcement::OperandScan;
    }
    match std::env::var(ENV_GROK_CONFINE_SHELL_MODE)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "operand" | "operand-scan" | "allowlist" | "legacy" => ConfineShellEnforcement::OperandScan,
        _ => ConfineShellEnforcement::FailClosed,
    }
}

/// Pin `GROK_CONFINE` / `GROK_CONFINE_INHERIT` / `GROK_CONFINE_SHELL_MODE` on a
/// child command so the model cannot unset or downgrade them via `env -u` /
/// request env. No-op when unconfined. Multi-root values are `;`-joined.
pub fn pin_confine_env_on_tokio_command(cmd: &mut tokio::process::Command) {
    let roots = process_confine_roots();
    if roots.is_empty() {
        return;
    }
    cmd.env(ENV_GROK_CONFINE, join_confine_path_list(roots));
    cmd.env(ENV_GROK_CONFINE_INHERIT, "1");
    cmd.env(
        ENV_GROK_CONFINE_SHELL_MODE,
        confine_shell_enforcement().as_str(),
    );
}

/// [`std::process::Command`] counterpart of [`pin_confine_env_on_tokio_command`].
pub fn pin_confine_env_on_std_command(cmd: &mut std::process::Command) {
    let roots = process_confine_roots();
    if roots.is_empty() {
        return;
    }
    cmd.env(ENV_GROK_CONFINE, join_confine_path_list(roots));
    cmd.env(ENV_GROK_CONFINE_INHERIT, "1");
    cmd.env(
        ENV_GROK_CONFINE_SHELL_MODE,
        confine_shell_enforcement().as_str(),
    );
}

/// When true, [`emit_confine_violation`] prints an NDJSON event on stdout
/// (streaming-json channel). Set from the headless emitter at startup.
static STREAMING_JSON_CONFINE_EMIT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Enable/disable stdout emission of `confine_violation` NDJSON events.
/// Headless `streaming-json` turns this on; other formats leave it off so TUI
/// sessions never print harness events onto the UI stream.
pub fn set_streaming_json_confine_emit(enabled: bool) {
    STREAMING_JSON_CONFINE_EMIT.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Emit a `confine_violation` event on the streaming-json channel when enabled.
///
/// Harnesses count these to detect attempted escapes without diffing the
/// filesystem after the run. Safe no-op when streaming-json emit is off.
///
/// `rule` names the confine rule that fired (e.g. `path-outside-root`,
/// `shell-unmodelled-program`, `shell-unparseable`, `mcp-path-outside-root`,
/// `fs-write-chokepoint`) so operators can act without replaying the run.
pub fn emit_confine_violation(tool: &str, path: &str, resolved_path: &str, root: &str, rule: &str) {
    if !STREAMING_JSON_CONFINE_EMIT.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    // Same NDJSON channel as HeadlessEmitter (`println!` one object per line).
    let event = serde_json::json!({
        "type": "confine_violation",
        "tool": tool,
        "path": path,
        "resolvedPath": resolved_path,
        "root": root,
        "rule": rule,
        "schemaVersion": 1,
    });
    println!("{event}");
}
/// Strip surrounding whitespace (e.g. a trailing newline from block-form
/// tool args) and quotes that models occasionally emit around path args.
///
/// When the arg was quote-wrapped, the model emitted a *string literal* (e.g.
/// a JSON-style `"/path/file.ts\n"` pasted into a block-form arg where no
/// JSON unescaping ever runs). In that case also strip trailing **literal**
/// escape sequences (`\n`, `\r`, `\t` as two characters) left at the end of
/// the unquoted value — `str::trim` only removes real whitespace, so the
/// resolved path would otherwise end in a literal backslash-n and miss the
/// file. Escape stripping requires the trimmed arg to both *start and end*
/// with a quote character (true quote-wrapping): a stray unbalanced quote is
/// still stripped, but does not enable escape stripping, so backslashes in
/// otherwise-unquoted real paths (e.g. Windows `dir\n ame`) are never eaten.
/// Trim and unquote a model-supplied path argument.
///
/// PUBLIC because the permission gate must derive its operand from the exact
/// same string the tool will act on. If the gate sees ` .env` while the tool
/// opens `.env`, a configured deny keyed on `**/.env` misses and the read
/// falls through to the default auto-allow.
pub fn sanitize_model_path_arg(input: &str) -> &str {
    let trimmed = input.trim();
    let quote_wrapped =
        trimmed.len() >= 2 && trimmed.starts_with(['"', '\'']) && trimmed.ends_with(['"', '\'']);
    let unquoted = trimmed.trim_matches(['"', '\'']).trim();
    if !quote_wrapped {
        return unquoted;
    }
    let mut result = unquoted;
    while let Some(stripped) = result
        .strip_suffix("\\n")
        .or_else(|| result.strip_suffix("\\r"))
        .or_else(|| result.strip_suffix("\\t"))
    {
        result = stripped.trim_end();
    }
    result
}
/// Return the display path (for model-facing output) or fall back to cwd.
pub fn display_cwd_or_cwd(cwd: &std::path::Path, display_cwd: Option<&std::path::Path>) -> PathBuf {
    display_cwd.unwrap_or(cwd).to_path_buf()
}

/// Model-facing path for tool error messages and UI.
///
/// Relative model inputs are joined onto `display_base` (display cwd or real
/// cwd). **Rooted** model spellings (`/foo`, `C:\foo`, …) are returned as the
/// model wrote them — never via `display_base.join(input)`.
///
/// On Windows, `Path::join` treats a leading `/` as root-relative to the
/// *current drive*, rewriting `/wrong/root/x` into `H:\wrong\root\x` when the
/// base is under `H:`. That mutates the path the model asked about and breaks
/// not-found messaging / skill hints. Preserve the model spelling instead.
pub fn model_display_path(display_base: &std::path::Path, model_input: &str) -> PathBuf {
    let input = sanitize_model_path_arg(model_input);
    let expanded = shellexpand::tilde(input);
    let input_path = std::path::Path::new(expanded.as_ref());
    if path_is_rooted(input_path) {
        return std::path::PathBuf::from(expanded.as_ref());
    }
    display_base.join(input_path)
}
/// Newtype wrapper for `Arc<dyn xai_tool_runtime::ToolDispatch>` so it can
/// be stored in `ToolCallContext::extensions`. Used by `use_tool` and the
/// external MCP-call tool, which dispatch to target tools without going
/// through the outer `ToolBridge` (which would deadlock).
#[derive(Clone)]
pub struct InnerDispatch(pub std::sync::Arc<dyn xai_tool_runtime::ToolDispatch>);
#[derive(Debug, Clone)]
pub struct ManagedGatewayToolSource {
    pub connector_id: String,
    pub connector_name: String,
    pub tool_id: String,
    pub tool_name: String,
    pub call_id: String,
}
#[derive(Debug, Clone, Default)]
pub struct ManagedGatewayToolCatalog(pub HashMap<String, ManagedGatewayToolSource>);
impl ManagedGatewayToolCatalog {
    pub fn get(&self, name: &str) -> Option<&ManagedGatewayToolSource> {
        self.0.get(name)
    }
}
#[derive(Debug, Clone)]
pub struct ManagedGatewayToolCallResponse {
    pub result: serde_json::Value,
    pub connectors_needing_reauth: Vec<String>,
}
#[async_trait::async_trait]
pub trait ManagedGatewayToolCaller: Send + Sync {
    async fn call_tool(
        &self,
        call_id: &str,
        arguments: serde_json::Value,
        caller: &str,
    ) -> Result<ManagedGatewayToolCallResponse, xai_tool_runtime::ToolError>;
}
#[derive(Clone)]
pub struct ManagedGatewayToolClient(pub Arc<dyn ManagedGatewayToolCaller>);
impl std::fmt::Debug for ManagedGatewayToolClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedGatewayToolClient").finish()
    }
}
/// Whether streaming output is enabled for this invocation.
#[derive(Debug, Clone, Copy)]
pub struct StreamEnabled(pub bool);
/// Client-configurable truncation settings.
#[derive(Debug, Clone)]
pub struct TruncationCfg(pub crate::types::context::TruncationConfig);
/// Environment variables from .envrc etc.
#[derive(Debug, Clone)]
pub struct SessionEnv(pub Arc<HashMap<String, String>>);
/// Whether system reminders are enabled globally.
#[derive(Debug, Clone, Copy)]
pub struct SystemRemindersEnabled(pub bool);
/// Enforces `.gitignore` patterns on file-access tools (`read_file`, `search_replace`).
///
/// Seeded at session start from the same rules used by AGENTS.md discovery.
/// When absent (no git repo), tools allow all files.
#[derive(Clone)]
pub struct GitignoreFilter {
    gitignore: ignore::gitignore::Gitignore,
    git_root: PathBuf,
}
impl GitignoreFilter {
    pub fn new(gitignore: ignore::gitignore::Gitignore, git_root: PathBuf) -> Self {
        Self {
            gitignore,
            git_root,
        }
    }
    /// Check whether a path is gitignored.
    ///
    /// For non-existent files (new file creation), canonicalizes the parent
    /// directory to handle symlinks (e.g., macOS `/var` → `/private/var`).
    pub fn is_ignored(&self, path: &std::path::Path) -> bool {
        let normalized = dunce::canonicalize(path).unwrap_or_else(|_| {
            path.parent()
                .and_then(|parent| {
                    dunce::canonicalize(parent)
                        .ok()
                        .map(|p| p.join(path.file_name().unwrap_or_default()))
                })
                .unwrap_or_else(|| path.to_path_buf())
        });
        crate::gitignore::is_ignored(&self.gitignore, &normalized, Some(&self.git_root))
    }
}
impl std::fmt::Debug for GitignoreFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitignoreFilter")
            .field("git_root", &self.git_root)
            .finish()
    }
}
/// Controls whether tools respect `.gitignore` patterns.
///
/// Always seeded by `agent_rebuild`. When `true`, all tools block gitignored
/// files. When `false`, `read_file` allows via `is_some_and` while
/// `grep`/`list_dir`/`search_replace` also allow via `is_none_or`.
///
/// Configured via `[tools] respect_gitignore = true` in `config.toml`.
#[derive(Debug, Clone, Copy)]
pub struct RespectGitignore(pub bool);
impl Default for RespectGitignore {
    fn default() -> Self {
        Self(true)
    }
}
/// Whether to enrich path-not-found errors with CWD reminders, "dropped repo
/// folder" correction, and similar-name suggestions.
///
/// Default **true** for RC13 (atlas miss recovery). Hosts may disable via
/// remote config / local settings.
#[derive(Debug, Clone, Copy)]
pub struct PathNotFoundHints(pub bool);
impl Default for PathNotFoundHints {
    fn default() -> Self {
        Self(true)
    }
}

/// Whether `workspace_tree` / `resolve_path` may walk and persist an atlas for
/// the session CWD (folder-trust gate).
///
/// When **absent**, tools allow indexing (unit tests / headless fixtures).
/// When **present and false**, tools fail with `workspace_tree_untrusted`.
/// Shell inserts `true` only when `project_scope_allowed` (same as kickoff).
#[derive(Debug, Clone, Copy)]
pub struct WorkspaceTreeIndexingAllowed(pub bool);
/// Whether scheduled task fires execute in background loop subagents.
///
/// `false` forces every fire onto the legacy main-conversation path.
/// Configured via `[scheduler] background_loops` in `config.toml`, the
/// `GROK_SCHEDULER_BACKGROUND_LOOPS` env var, or the
/// `scheduler_background_loops` remote setting.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerBackgroundLoops(pub bool);
impl Default for SchedulerBackgroundLoops {
    fn default() -> Self {
        Self(true)
    }
}
/// Map of canonical tool names → model-facing tool names.
#[derive(Debug, Clone, Default)]
pub struct ToolNameMapping(pub HashMap<String, String>);
impl ToolNameMapping {
    /// Resolve a canonical tool name to the model-facing name.
    /// Falls back to the canonical name if not in the map.
    pub fn resolve<'a>(&'a self, canonical: &'a str) -> &'a str {
        self.0
            .get(canonical)
            .map(|s| s.as_str())
            .unwrap_or(canonical)
    }
}
/// Set of client-facing names of all enabled **native** (non-MCP) tools.
///
/// Populated once at `finalize()` from the finalized tool list (every tool
/// whose client-facing name does not contain the `__` MCP delimiter). Used by
/// `use_tool` to detect when the model wrongly routes a native tool call
/// (e.g. `scheduler_create`) through `use_tool`. Without this, such calls hit
/// the generic "not a valid MCP tool name" error and the model gets stuck,
/// because `search_tool` only indexes MCP tools.
///
/// Detected at runtime by `use_tool::run()` to return a corrective error
/// ("call it directly") instead of the generic "not a valid MCP tool name"
/// message that left the model stuck.
#[derive(Debug, Clone, Default)]
pub struct EnabledNativeToolNames(pub std::collections::HashSet<String>);
impl EnabledNativeToolNames {
    /// Whether `name` is an enabled native (non-MCP) tool.
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}
/// Map of canonical tool name → {canonical param name → model-facing param name}.
#[derive(Debug, Clone, Default)]
pub struct ParamNameMapping(pub HashMap<String, HashMap<String, String>>);
impl ParamNameMapping {
    /// Resolve a canonical parameter name for a given tool.
    /// Falls back to the canonical name if not in the map.
    pub fn resolve<'a>(&'a self, tool: &str, canonical: &'a str) -> &'a str {
        self.0
            .get(tool)
            .and_then(|m| m.get(canonical))
            .map(|s| s.as_str())
            .unwrap_or(canonical)
    }
}
/// Canonical → client-facing param names for the tool currently executing.
///
/// Stamped onto [`xai_tool_runtime::ToolCallContext::extensions`] by
/// `prepare_dispatch` / `call_raw` from that tool's own
/// `params_name_overrides`. Prefer this over kind-wide
/// [`crate::types::template_renderer::TemplateRenderer::param_for_kind`] when
/// naming params in that tool's own errors — multiple tools can share a
/// `ToolKind` with different renames, and the kind map is first/last-wins.
#[derive(Debug, Clone, Default)]
pub struct InvokingToolParamNames(pub HashMap<String, String>);
impl InvokingToolParamNames {
    /// Build from a client→canonical reverse map (the dispatch remap direction).
    pub fn from_reverse_params(reverse_params: &HashMap<String, String>) -> Self {
        Self(
            reverse_params
                .iter()
                .map(|(client, canonical)| (canonical.clone(), client.clone()))
                .collect(),
        )
    }
    /// Resolve a canonical parameter name for the invoking tool.
    /// Falls back to the canonical name if not in the map.
    pub fn resolve<'a>(&'a self, canonical: &'a str) -> &'a str {
        self.0
            .get(canonical)
            .map(String::as_str)
            .unwrap_or(canonical)
    }
}
/// Map of `ToolKind` → client-facing tool name.
///
/// Built at finalize time from the enabled tools and client name overrides.
/// Used at runtime by tools that reference other tools in error messages
/// (e.g., search_replace saying "use the Read tool first").
///
/// This is the **kind-based** counterpart to `ToolNameMapping`. Tools query
/// by semantic role (`ToolKind::Read`), not canonical name (`"read_file"`).
#[derive(Debug, Clone, Default)]
pub struct ToolKindNames(pub HashMap<crate::types::tool::ToolKind, String>);
/// Map of `ToolKind` → { canonical param name → client-facing param name }.
///
/// Built at finalize time from client param overrides. Used at runtime by
/// tools that reference their own (or other tools') param names in error
/// messages (e.g., "use `replaceAll` to replace all occurrences").
#[derive(Debug, Clone, Default)]
pub struct ParamKindNames(pub HashMap<crate::types::tool::ToolKind, HashMap<String, String>>);
impl ParamKindNames {
    /// Resolve a canonical parameter name for a given tool kind.
    /// Falls back to the canonical name if not in the map.
    pub fn resolve<'a>(
        &'a self,
        kind: crate::types::tool::ToolKind,
        canonical: &'a str,
    ) -> &'a str {
        self.0
            .get(&kind)
            .and_then(|m| m.get(canonical))
            .map(|s| s.as_str())
            .unwrap_or(canonical)
    }
}
/// Available skills for description template rendering.
///
/// Stored in Resources so `build_description_context()` can populate the
/// `skills` field of `DescriptionContext`. Inserted by `with_backend()`
/// before any tools are registered.
#[derive(Debug, Clone)]
pub struct AvailableSkills(pub Vec<crate::implementations::skills::types::SkillInfo>);
impl AvailableSkills {
    /// Check if a skill with the given name is available for model invocation.
    ///
    /// Returns `false` for skills with `disable_model_invocation = true` (model
    /// cannot auto-invoke) or `user_invocable = false` (not shown in skill tool),
    /// since the model would be unable to successfully invoke them.
    pub fn has_skill(&self, name: &str) -> bool {
        self.0
            .iter()
            .any(|s| s.name == name && s.enabled && !s.disable_model_invocation && s.user_invocable)
    }
}
/// Session folder for logs and output files.
#[derive(Debug, Clone)]
pub struct SessionFolder(pub PathBuf);
/// Per-turn registry mapping each attached image's `[Image #N]` display
/// number to a reference `image_edit` can resolve.
///
/// The model sees attachments inline (as pixels) and only the `[Image #N]`
/// token in text — never a path — so this lets `image_edit` resolve that
/// token instead of fabricating a filesystem path it can't know.
///
/// Keyed by display **number**, not list position: numbers are not
/// renumbered when a chip is removed mid-compose (`#1` and `#3` survive
/// after `#2`) and images may be dropped during normalization, so the two
/// diverge. Each reference is a bare filesystem path (the durable
/// `session_image_path`) or a `data:<mime>;base64,<data>` URL fallback.
///
/// Replaced wholesale each turn (empty when there are no attachments) so a
/// stale registry never resolves to a prior turn's image. Ephemeral — not
/// persisted, not serde-registered.
#[derive(Debug, Clone, Default)]
pub struct AttachedImages(pub Vec<(usize, String)>);
impl AttachedImages {
    /// Resolve an `[Image #N]` display number to its reference string.
    pub fn reference_for(&self, display_number: usize) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| *n == display_number)
            .map(|(_, reference)| reference.as_str())
    }
}
/// Notification handle for streaming tool output.
#[derive(Clone)]
pub struct NotificationHandle(pub ToolNotificationHandle);
impl std::fmt::Debug for NotificationHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationHandle").finish()
    }
}
/// File system abstraction.
pub struct FileSystem(pub Arc<dyn AsyncFileSystem>);
impl std::fmt::Debug for FileSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSystem").finish()
    }
}
/// Terminal backend abstraction.
pub struct Terminal(pub Arc<dyn TerminalBackend>);
/// Session ID that owns processes spawned by this session's tools.
/// Used to scope kill operations so subagent teardown only kills
/// the subagent's own tasks on a shared terminal backend.
#[derive(Debug, Clone)]
pub struct OwnerSessionId(pub String);
/// Shared citation counter for `[web:N]` numbering across web tools.
///
/// Stored as `State<WebCitationCounter>` in Resources so web tools that emit
/// citations share the same monotonically increasing counter within a session.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebCitationCounter {
    pub counter: u32,
}
impl WebCitationCounter {
    /// Return the current value and increment.
    pub fn next_citation(&mut self) -> u32 {
        let val = self.counter;
        self.counter += 1;
        val
    }
}
register_resource!("grok_build", "WebCitation", WebCitationCounter);
impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal").finish()
    }
}
/// Per-tool retry/backoff configurations.
/// Set by the agent builder, consumed by the bridge's retry loop.
///
/// NOT persisted — ephemeral runtime state that's re-set on each session.
#[derive(Debug, Clone, Default)]
pub struct ToolRetries(pub HashMap<String, crate::retry::BackoffConfig>);
impl ToolRetries {
    /// Set a retry config for a specific tool.
    pub fn set(&mut self, tool: &str, config: crate::retry::BackoffConfig) {
        self.0.insert(tool.to_string(), config);
    }
    /// Get the retry config for a specific tool, if set.
    pub fn get(&self, tool: &str) -> Option<&crate::retry::BackoffConfig> {
        self.0.get(tool)
    }
    /// Clear all retry configs.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}
/// Tracks whether a required "completion" tool has been called this turn.
///
/// Used by agent definitions that require a specific tool to be called
/// before the agent can be considered "done" (e.g. a workflow's
/// `complete_task` tool).
///
/// Ephemeral — NOT persisted. Stored in Resources, not serde-registered.
#[derive(Debug, Clone)]
pub struct CompletionTracker {
    /// Canonical name of the tool that must be called.
    pub tool: String,
    /// Reminder text to inject if the tool hasn't been called.
    pub reminder: String,
    /// Whether the tool was called during the current turn.
    pub called_this_turn: bool,
}
impl ResourceType for CompletionTracker {
    const ID: &'static str = "";
}
/// Metadata for a single MCP resource, returned by [`McpResourceProvider::list_resources`].
#[derive(Debug, Clone)]
pub struct McpResourceInfo {
    pub uri: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub server: String,
}
/// Content payload returned by [`McpResourceProvider::read_resource`].
#[derive(Debug)]
pub enum McpResourceContent {
    Text(String),
    Blob(Vec<u8>),
}
/// Result of reading a single MCP resource.
#[derive(Debug)]
pub struct McpResourceReadResult {
    pub uri: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub content: Option<McpResourceContent>,
}
/// Provider trait for MCP resource operations.
///
/// Injected into `SharedResources` by the shell layer so tools
/// (`ListMcpResources`, `FetchMcpResource`) can access MCP servers without
/// depending on `xai-grok-mcp` directly.  Follows the same pattern as
/// [`FileSystem`] (`Arc<dyn AsyncFileSystem>`).
#[async_trait::async_trait]
pub trait McpResourceProvider: Send + Sync {
    /// List resources from one or all MCP servers.
    async fn list_resources(&self, server: Option<String>) -> Result<Vec<McpResourceInfo>, String>;
    /// Read a specific resource by server name and URI.
    async fn read_resource(
        &self,
        server: String,
        uri: String,
    ) -> Result<McpResourceReadResult, String>;
}
/// Wrapper stored in [`Resources`] for MCP resource access.
#[derive(Clone)]
pub struct McpResourceAccess(pub Arc<dyn McpResourceProvider>);
impl std::fmt::Debug for McpResourceAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpResourceAccess").finish()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn enforce_write_roots_accepts_existing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        enforce_write_roots(Some(dir.path()), Some(dir.path())).expect("both present");
        enforce_write_roots(Some(dir.path()), None).expect("cwd only");
        enforce_write_roots(None, Some(dir.path())).expect("root only");
        enforce_write_roots(None, None).expect("nothing to check");
    }

    #[test]
    fn enforce_write_roots_rejects_missing_cwd() {
        let gone = Path::new("/definitely/not/a/real/cwd-for-rc13-test-xyz");
        let err = enforce_write_roots(Some(gone), None).expect_err("missing cwd");
        assert_eq!(err.code(), "cwd_missing");
        assert!(err.message().contains("cwd_missing"));
    }

    #[test]
    fn enforce_write_roots_rejects_missing_confine_root() {
        let gone = Path::new("/definitely/not/a/real/confine-for-rc13-test-xyz");
        let err = enforce_write_roots(None, Some(gone)).expect_err("missing root");
        assert_eq!(err.code(), "worktree_tombstone");
        assert!(err.message().contains("worktree_tombstone"));
    }

    #[test]
    fn enforce_write_path_rejects_file_as_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir.txt");
        std::fs::write(&file, b"x").unwrap();
        let err = enforce_write_path(&file, None).expect_err("file is not cwd");
        assert_eq!(err.code(), "cwd_missing");
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct EditConfig {
        skip_read_before_edit: bool,
        max_file_size: Option<usize>,
    }
    register_resource!("grok_build", "Edit", EditConfig);
    #[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct ReadHistory {
        files_read: Vec<String>,
    }
    register_resource!("grok_build", "ReadFile", ReadHistory);
    #[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct TodoData {
        items: Vec<String>,
    }
    register_resource!("grok_build", "Todo", TodoData);
    #[test]
    fn insert_and_get_typed_values() {
        let mut res = Resources::new();
        res.insert(42u32);
        res.insert("hello".to_string());
        assert_eq!(res.get::<u32>(), Some(&42));
        assert_eq!(res.get::<String>(), Some(&"hello".to_string()));
        assert_eq!(res.get::<bool>(), None);
    }
    #[test]
    fn get_mut_modifies_in_place() {
        let mut res = Resources::new();
        res.insert(10i32);
        *res.get_mut::<i32>().unwrap() += 5;
        assert_eq!(res.get::<i32>(), Some(&15));
    }
    #[test]
    fn get_or_default_inserts_when_missing() {
        let mut res = Resources::new();
        let val = res.get_or_default::<Vec<i32>>();
        val.push(1);
        val.push(2);
        assert_eq!(res.get::<Vec<i32>>(), Some(&vec![1, 2]));
    }
    #[test]
    fn get_or_default_returns_existing() {
        let mut res = Resources::new();
        res.insert(vec![42i32]);
        let val = res.get_or_default::<Vec<i32>>();
        assert_eq!(val, &vec![42]);
    }
    #[test]
    fn remove_returns_value() {
        let mut res = Resources::new();
        res.insert("test".to_string());
        let removed = res.remove::<String>();
        assert_eq!(removed, Some("test".to_string()));
        assert_eq!(res.get::<String>(), None);
    }
    #[test]
    fn remove_returns_none_when_missing() {
        let mut res = Resources::new();
        assert_eq!(res.remove::<String>(), None);
    }
    #[test]
    fn contains_checks_presence() {
        let mut res = Resources::new();
        assert!(!res.contains::<u32>());
        res.insert(42u32);
        assert!(res.contains::<u32>());
    }
    #[test]
    fn params_and_state_coexist_without_collision() {
        let mut res = Resources::new();
        res.insert(Params(EditConfig {
            skip_read_before_edit: true,
            max_file_size: Some(100),
        }));
        res.insert(State(EditConfig {
            skip_read_before_edit: false,
            max_file_size: None,
        }));
        let params = res.get::<Params<EditConfig>>().unwrap();
        let state = res.get::<State<EditConfig>>().unwrap();
        assert!(params.skip_read_before_edit);
        assert_eq!(params.max_file_size, Some(100));
        assert!(!state.skip_read_before_edit);
        assert_eq!(state.max_file_size, None);
    }
    #[test]
    fn params_and_state_have_different_typeids() {
        assert_ne!(
            TypeId::of::<Params<EditConfig>>(),
            TypeId::of::<State<EditConfig>>()
        );
    }
    #[test]
    fn serde_roundtrip_registered_types() {
        let mut res = Resources::new();
        res.register_params::<EditConfig>();
        res.register_state::<ReadHistory>();
        res.register_state::<TodoData>();
        res.insert(Params(EditConfig {
            skip_read_before_edit: true,
            max_file_size: Some(1024),
        }));
        res.insert(State(ReadHistory {
            files_read: vec!["main.rs".to_string(), "lib.rs".to_string()],
        }));
        res.insert(State(TodoData {
            items: vec!["task1".to_string()],
        }));
        let json = res.serialize();
        let json_str = serde_json::to_string_pretty(&json).unwrap();
        let mut res2 = Resources::new();
        res2.register_params::<EditConfig>();
        res2.register_state::<ReadHistory>();
        res2.register_state::<TodoData>();
        let parsed: HashMap<String, HashMap<String, serde_json::Value>> =
            serde_json::from_str(&json_str).unwrap();
        res2.load_from(parsed);
        let params = res2.get::<Params<EditConfig>>().unwrap();
        assert!(params.0.skip_read_before_edit);
        assert_eq!(params.0.max_file_size, Some(1024));
        let state = res2.get::<State<ReadHistory>>().unwrap();
        assert_eq!(
            state.0.files_read,
            vec!["main.rs".to_string(), "lib.rs".to_string()]
        );
        let todo = res2.get::<State<TodoData>>().unwrap();
        assert_eq!(todo.0.items, vec!["task1".to_string()]);
    }
    #[test]
    fn ephemeral_types_silently_skipped_during_serialization() {
        let mut res = Resources::new();
        res.register_state::<ReadHistory>();
        res.insert(State(ReadHistory {
            files_read: vec!["file.rs".to_string()],
        }));
        res.insert(Cwd(PathBuf::from("/home/user")));
        res.insert(StreamEnabled(true));
        let json = res.serialize();
        assert!(json.get("state").is_some());
        let state = json.get("state").unwrap();
        assert!(state.get("grok_build.ReadFile").is_some());
        let json_str = serde_json::to_string(&json).unwrap();
        assert!(!json_str.contains("/home/user"));
    }
    #[test]
    fn load_from_populates_registered_types() {
        let mut res = Resources::new();
        res.register_state::<ReadHistory>();
        res.register_params::<EditConfig>();
        let mut state_map = HashMap::new();
        state_map.insert(
            "grok_build.ReadFile".to_string(),
            serde_json::json!({"files_read": ["loaded.rs"]}),
        );
        let mut params_map = HashMap::new();
        params_map.insert(
            "grok_build.Edit".to_string(),
            serde_json::json!({"skip_read_before_edit": true, "max_file_size": 512}),
        );
        let mut data = HashMap::new();
        data.insert("state".to_string(), state_map);
        data.insert("params".to_string(), params_map);
        res.load_from(data);
        let history = res.get::<State<ReadHistory>>().unwrap();
        assert_eq!(history.0.files_read, vec!["loaded.rs".to_string()]);
        let config = res.get::<Params<EditConfig>>().unwrap();
        assert!(config.0.skip_read_before_edit);
        assert_eq!(config.0.max_file_size, Some(512));
    }
    #[test]
    fn load_from_ignores_unknown_keys() {
        let mut res = Resources::new();
        res.register_state::<ReadHistory>();
        let mut state_map = HashMap::new();
        state_map.insert(
            "unknown.Type".to_string(),
            serde_json::json!({"foo": "bar"}),
        );
        state_map.insert(
            "grok_build.ReadFile".to_string(),
            serde_json::json!({"files_read": ["ok.rs"]}),
        );
        let mut data = HashMap::new();
        data.insert("state".to_string(), state_map);
        res.load_from(data);
        let history = res.get::<State<ReadHistory>>().unwrap();
        assert_eq!(history.0.files_read, vec!["ok.rs".to_string()]);
    }
    #[test]
    fn get_json_returns_registered_value() {
        let mut res = Resources::new();
        res.register_params::<EditConfig>();
        res.insert(Params(EditConfig {
            skip_read_before_edit: true,
            max_file_size: None,
        }));
        let val = res.get_json("params", "grok_build.Edit").unwrap();
        assert_eq!(val["skip_read_before_edit"], true);
    }
    #[test]
    fn get_json_returns_none_for_unregistered() {
        let res = Resources::new();
        assert!(res.get_json("params", "nonexistent").is_none());
    }
    #[test]
    fn get_json_returns_none_for_missing_value() {
        let mut res = Resources::new();
        res.register_params::<EditConfig>();
        assert!(res.get_json("params", "grok_build.Edit").is_none());
    }
    #[test]
    fn set_json_updates_registered_value() {
        let mut res = Resources::new();
        res.register_params::<EditConfig>();
        let ok = res.set_json(
            "params",
            "grok_build.Edit",
            serde_json::json!({"skip_read_before_edit": true}),
        );
        assert!(ok);
        let config = res.get::<Params<EditConfig>>().unwrap();
        assert!(config.0.skip_read_before_edit);
    }
    #[test]
    fn set_json_returns_false_for_unregistered() {
        let mut res = Resources::new();
        let ok = res.set_json("params", "unknown", serde_json::json!({}));
        assert!(!ok);
    }
    #[test]
    fn double_register_is_idempotent() {
        let mut res = Resources::new();
        res.register_state::<ReadHistory>();
        res.register_state::<ReadHistory>();
        assert_eq!(res.entries.len(), 1);
    }
    #[test]
    fn serialize_empty_resources_produces_empty_object() {
        let res = Resources::new();
        let json = res.serialize();
        assert_eq!(json, serde_json::json!({}));
    }
    #[test]
    fn serialize_with_registrations_but_no_values() {
        let mut res = Resources::new();
        res.register_params::<EditConfig>();
        res.register_state::<ReadHistory>();
        let json = res.serialize();
        assert_eq!(json, serde_json::json!({}));
    }
    #[test]
    fn tool_name_mapping_resolve() {
        let mut mapping = ToolNameMapping::default();
        mapping
            .0
            .insert("read_file".to_string(), "Read".to_string());
        assert_eq!(mapping.resolve("read_file"), "Read");
        assert_eq!(mapping.resolve("grep"), "grep");
    }
    #[test]
    fn param_name_mapping_resolve() {
        let mut mapping = ParamNameMapping::default();
        let mut tool_map = HashMap::new();
        tool_map.insert("old_string".to_string(), "find".to_string());
        mapping.0.insert("search_replace".to_string(), tool_map);
        assert_eq!(mapping.resolve("search_replace", "old_string"), "find");
        assert_eq!(
            mapping.resolve("search_replace", "new_string"),
            "new_string"
        );
        assert_eq!(mapping.resolve("other_tool", "old_string"), "old_string");
    }
    #[test]
    fn invoking_tool_param_names_from_reverse_and_resolve() {
        let reverse = HashMap::from([
            ("start_line".to_string(), "offset".to_string()),
            ("max_lines".to_string(), "limit".to_string()),
        ]);
        let names = InvokingToolParamNames::from_reverse_params(&reverse);
        assert_eq!(names.resolve("offset"), "start_line");
        assert_eq!(names.resolve("limit"), "max_lines");
        assert_eq!(names.resolve("path"), "path");
    }
    #[test]
    fn params_deref() {
        let p = Params(EditConfig {
            skip_read_before_edit: true,
            max_file_size: Some(42),
        });
        assert!(p.skip_read_before_edit);
        assert_eq!(p.max_file_size, Some(42));
    }
    #[test]
    fn state_deref_mut() {
        let mut s = State(ReadHistory { files_read: vec![] });
        s.files_read.push("new.rs".to_string());
        assert_eq!(s.files_read, vec!["new.rs".to_string()]);
    }
    #[test]
    fn resolve_model_path_relative_no_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    #[test]
    fn resolve_model_path_posix_absolute_without_display_keeps_spelling() {
        // Windows must not rewrite `/etc/hosts` into `H:\etc\hosts` when no
        // DisplayCwd is set (join treats leading `/` as current-drive root).
        let cwd = std::path::Path::new(if cfg!(windows) {
            r"H:\worktree\abc"
        } else {
            "/worktree/abc"
        });
        let result = super::resolve_model_path(cwd, None, "/etc/hosts");
        assert_eq!(result, std::path::PathBuf::from("/etc/hosts"));
    }
    #[test]
    fn model_display_path_preserves_rooted_model_spelling() {
        let base = std::path::Path::new(if cfg!(windows) {
            r"H:\tmp\session"
        } else {
            "/tmp/session"
        });
        assert_eq!(
            super::model_display_path(base, "/wrong/root/SKILL.md"),
            std::path::PathBuf::from("/wrong/root/SKILL.md")
        );
        assert_eq!(
            super::model_display_path(base, "relative/file.rs"),
            base.join("relative/file.rs")
        );
    }
    #[test]
    fn resolve_model_path_absolute_matching_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result =
            super::resolve_model_path(cwd, Some(display), "/home/user/project/src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    #[test]
    fn resolve_model_path_absolute_non_matching() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result = super::resolve_model_path(cwd, Some(display), "/etc/hosts");
        assert_eq!(result, std::path::PathBuf::from("/etc/hosts"));
    }
    #[test]
    fn resolve_model_path_relative_with_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result = super::resolve_model_path(cwd, Some(display), "src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    #[test]
    fn resolve_model_path_root_itself() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result = super::resolve_model_path(cwd, Some(display), "/home/user/project");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc"));
    }
    /// Kimi sent a bare colon as grep path. Should be treated as relative
    /// (joined onto cwd), NOT produce a worktree-path leak.
    #[test]
    fn resolve_model_path_bare_colon() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), ":");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc/:"));
    }
    /// Kimi sent ":/testbed/cache/cache.go" — colon before display path.
    /// This is NOT absolute (doesn't start with '/'), so treated as relative.
    #[test]
    fn resolve_model_path_colon_prefixed_display_path() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), ":/testbed/cache/cache.go");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/:/testbed/cache/cache.go"),
        );
    }
    /// Empty string input — should resolve to cwd itself.
    #[test]
    fn resolve_model_path_empty_string() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), "");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc"));
    }
    /// Absolute path that is a partial prefix match should NOT be rewritten.
    /// e.g., display="/testbed/cache" but input="/testbed/cacheXYZ/foo" — no match.
    #[test]
    fn resolve_model_path_partial_prefix_no_match() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), "/testbed/cacheXYZ/foo");
        assert_eq!(result, std::path::PathBuf::from("/testbed/cacheXYZ/foo"));
    }
    /// Dotdot traversal in relative path — should join as-is (no normalization).
    #[test]
    fn resolve_model_path_dotdot_relative() {
        let cwd = std::path::Path::new("/worktree/abc/subdir");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), "../other/file.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/subdir/../other/file.rs"),
        );
    }
    /// Absolute path matching display with trailing slash — strip_prefix
    /// handles this because Path normalizes trailing slashes.
    #[test]
    fn resolve_model_path_display_trailing_slash() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), "/testbed/cache/src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Trailing newline (from block-form tool args) must be stripped so the
    /// path targets `foo`, not a file literally named `foo\n`.
    #[test]
    fn resolve_model_path_trailing_newline() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "/worktree/abc/tsconfig.json\n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/tsconfig.json")
        );
    }
    /// Trailing newline on a relative path is trimmed before the cwd join.
    #[test]
    fn resolve_model_path_relative_trailing_newline() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "src/main.rs\n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// A trailing newline must not defeat the display-cwd rewrite (the
    /// absolute prefix match would otherwise fail on `...main.rs\n`).
    #[test]
    fn resolve_model_path_trailing_newline_with_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result =
            super::resolve_model_path(cwd, Some(display), "/home/user/project/src/main.rs\n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Leading/trailing spaces and tabs are trimmed.
    #[test]
    fn resolve_model_path_surrounding_whitespace() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "  \tsrc/main.rs \r\n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Whitespace outside quotes is trimmed, then the quotes are stripped.
    #[test]
    fn resolve_model_path_quoted_with_surrounding_whitespace() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, " \"src/main.rs\"\n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Whitespace *inside* the quotes (trailing newline before the closing
    /// quote) is caught by the second trim after quote-stripping.
    #[test]
    fn resolve_model_path_newline_inside_quotes() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "\"src/main.rs\n\"");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Whitespace-only input still resolves to cwd (matches empty-string case).
    #[test]
    fn resolve_model_path_whitespace_only() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "  \n");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc"));
    }
    /// A quote-wrapped arg carrying a *literal* `\n` escape sequence (two
    /// characters, backslash + n) — a JSON string literal pasted into a
    /// block-form arg with no unescaping — must resolve to the real file,
    /// not one whose name ends in a literal backslash-n.
    #[test]
    fn resolve_model_path_quoted_literal_backslash_n() {
        let cwd = std::path::Path::new("/workspace");
        let result = super::resolve_model_path(cwd, None, "\"/workspace/src/game/data.ts\\n\"");
        assert_eq!(
            result,
            std::path::PathBuf::from("/workspace/src/game/data.ts")
        );
    }
    /// Same for single quotes and stacked escapes (`\r\n`), plus outer
    /// real whitespace.
    #[test]
    fn resolve_model_path_quoted_literal_crlf_escapes() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, " 'src/main.rs\\r\\n' \n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// The literal-escape stripping must not defeat the display-cwd rewrite.
    #[test]
    fn resolve_model_path_quoted_literal_backslash_n_with_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result =
            super::resolve_model_path(cwd, Some(display), "\"/home/user/project/src/main.rs\\n\"");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    #[test]
    fn resolve_model_path_sensitive_edit_spellings() {
        let cwd = std::path::Path::new("/worktree/abc");
        for input in ["  /etc/hosts  ", "\"/etc/hosts\\n\"", "'/etc/hosts\\r\\t'"] {
            assert_eq!(
                super::resolve_model_path(cwd, None, input),
                std::path::PathBuf::from("/etc/hosts"),
                "{input:?}"
            );
        }
    }
    /// An *unquoted* path keeps its backslashes: `\n` there may be a real
    /// path component (e.g. a Windows-style separator + dir named `n`).
    #[test]
    fn resolve_model_path_unquoted_backslash_preserved() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "src\\n");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc/src\\n"));
    }
    /// A stray *trailing* quote on an otherwise-unquoted path must not
    /// enable escape stripping: the quote is stripped, but the literal
    /// backslash-n is a real path component and must survive.
    #[test]
    fn resolve_model_path_stray_trailing_quote_keeps_literal_escape() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "src\\n\"");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc/src\\n"));
    }
    /// Same for a stray *leading* quote: only args that both start and end
    /// with a quote are string literals eligible for escape stripping.
    #[test]
    fn resolve_model_path_stray_leading_quote_keeps_literal_escape() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "\"src\\n");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc/src\\n"));
    }
    /// A lone quote character satisfies both starts_with and ends_with, so
    /// without the length guard it would count as quote-wrapped. It must be
    /// treated as a stray quote: stripped, resolving to cwd like empty input.
    #[test]
    fn resolve_model_path_lone_quote() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "\"");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc"));
    }
    #[test]
    fn display_cwd_or_cwd_with_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        assert_eq!(
            super::display_cwd_or_cwd(cwd, Some(display)),
            std::path::PathBuf::from("/home/user/project"),
        );
    }
    #[test]
    fn display_cwd_or_cwd_without_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        assert_eq!(
            super::display_cwd_or_cwd(cwd, None),
            std::path::PathBuf::from("/worktree/abc"),
        );
    }
    /// Absolute path already under the real cwd must not be remapped via DisplayCwd.
    #[test]
    fn resolve_model_path_keeps_path_already_under_cwd() {
        let cwd = std::path::Path::new("/home/user/.grok/worktrees/pirates/subagent-abc");
        let display = std::path::Path::new("/home/user/Pirates");
        let input = "/home/user/.grok/worktrees/pirates/subagent-abc/tools/blender/nav.py";
        let result = super::resolve_model_path(cwd, Some(display), input);
        assert_eq!(result, std::path::PathBuf::from(input));
    }
    /// Inverted DisplayCwd=worktree / Cwd=parent must not fold a worktree write
    /// onto the shared parent checkout (P0 isolation_fallback).
    #[test]
    fn resolve_model_path_does_not_remap_worktree_onto_parent() {
        let parent = std::path::Path::new("/home/user/Pirates");
        let worktree = std::path::Path::new("/home/user/.grok/worktrees/pirates/subagent-abc");
        let input = "/home/user/.grok/worktrees/pirates/subagent-abc/tools/blender/nav.py";
        let result = super::resolve_model_path(parent, Some(worktree), input);
        assert_eq!(
            result,
            std::path::PathBuf::from(input),
            "worktree absolute must not become parent/tools/blender/nav.py"
        );
        assert!(
            !result.starts_with(parent) || result.starts_with(worktree),
            "resolved {} must stay on the worktree",
            result.display()
        );
    }
    #[test]
    fn path_looks_like_isolated_worktree_detects_product_and_temp() {
        assert!(super::path_looks_like_isolated_worktree(
            std::path::Path::new("/home/user/.grok/worktrees/pirates/subagent-abc/src/main.rs")
        ));
        assert!(super::path_looks_like_isolated_worktree(
            std::path::Path::new(
                r"C:\Users\testuser\.grok\worktrees\pirates\subagent-019ffc2f-9daa\tools\x.py"
            )
        ));
        assert!(!super::path_looks_like_isolated_worktree(
            std::path::Path::new("/home/user/Pirates/tools/x.py")
        ));
    }
    #[test]
    fn resolve_model_path_tilde_expands_to_home() {
        let home = dirs::home_dir().expect("test requires home_dir");
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "~/foo/bar.rs");
        assert_eq!(result, home.join("foo/bar.rs"));
    }
    #[test]
    fn resolve_model_path_tilde_alone() {
        let Some(home) = dirs::home_dir() else { return };
        let cwd = std::path::Path::new("/worktree/abc");
        assert_eq!(super::resolve_model_path(cwd, None, "~"), home);
    }
    #[test]
    fn resolve_model_path_tilde_slash_only() {
        let Some(home) = dirs::home_dir() else { return };
        let cwd = std::path::Path::new("/worktree/abc");
        assert_eq!(super::resolve_model_path(cwd, None, "~/"), home);
    }
    #[test]
    fn resolve_model_path_tilde_no_home_falls_back_to_literal() {
        let no_home = shellexpand::tilde_with_context("~/foo", || Option::<String>::None);
        assert_eq!(no_home.as_ref(), "~/foo");
    }
    #[test]
    fn resolve_model_path_tilde_username_not_expanded() {
        let cwd = std::path::Path::new("/worktree/abc");
        assert_eq!(
            super::resolve_model_path(cwd, None, "~root/foo"),
            std::path::PathBuf::from("/worktree/abc/~root/foo")
        );
    }
    #[test]
    fn resolve_model_path_tilde_not_at_start() {
        let cwd = std::path::Path::new("/worktree/abc");
        assert_eq!(
            super::resolve_model_path(cwd, None, "foo~bar"),
            std::path::PathBuf::from("/worktree/abc/foo~bar")
        );
    }
    #[test]
    fn resolve_model_path_tilde_with_display_cwd() {
        let Some(home) = dirs::home_dir() else { return };
        let cwd = std::path::Path::new("/worktree/abc");
        let display = home.join("project");
        let result = super::resolve_model_path(cwd, Some(&display), "~/project/src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Model gives cwd path without leading "/" — should resolve to cwd itself
    /// instead of producing a doubled path like /cwd/cwd.
    #[test]
    fn resolve_model_path_forgot_leading_slash_exact_cwd() {
        let cwd = std::path::Path::new("/data/user/workspace/repo/project");
        let result = super::resolve_model_path(cwd, None, "data/user/workspace/repo/project");
        assert_eq!(result, cwd);
    }
    /// Model gives cwd + subpath without leading "/" — should resolve correctly.
    #[test]
    fn resolve_model_path_forgot_leading_slash_subpath() {
        let cwd = std::path::Path::new("/data/user/workspace/repo/project");
        let result =
            super::resolve_model_path(cwd, None, "data/user/workspace/repo/project/src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/data/user/workspace/repo/project/src/main.rs"),
        );
    }
    /// Same pattern but with display_cwd set — model forgets "/" on the
    /// display path.
    #[test]
    fn resolve_model_path_forgot_leading_slash_with_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result = super::resolve_model_path(cwd, Some(display), "home/user/project/src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs"),
        );
    }
    /// Normal relative paths that don't match the cwd prefix are unaffected.
    #[test]
    fn resolve_model_path_normal_relative_unaffected() {
        let cwd = std::path::Path::new("/data/user/workspace/repo/project");
        let result = super::resolve_model_path(cwd, None, "src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/data/user/workspace/repo/project/src/main.rs"),
        );
    }
    #[test]
    fn resolve_model_path_strips_leading_trailing_quotes() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "\"src/main.rs\"");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs"),
        );
    }
    #[test]
    fn resolve_model_path_strips_leading_quote_only() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "\"src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs"),
        );
    }
    #[test]
    fn resolve_model_path_interior_quotes_preserved() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "path/with\"quote/file.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/path/with\"quote/file.rs"),
        );
    }

    #[test]
    fn confine_accepts_paths_under_root_rejects_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let inside = root.join("src").join("main.rs");
        let _ = std::fs::create_dir_all(inside.parent().unwrap());
        std::fs::write(&inside, "fn main() {}").unwrap();
        assert!(
            super::path_is_under_confine_root(&inside, root),
            "inside path must be under root"
        );
        assert!(
            super::path_is_under_confine_root(root, root),
            "root is under itself"
        );
        let outside = tmp.path().parent().unwrap().join("outside-sibling.txt");
        assert!(
            !super::path_is_under_confine_root(&outside, root),
            "sibling outside root must be rejected"
        );
    }

    #[test]
    fn resolve_model_path_confined_errors_on_absolute_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outside = if cfg!(windows) {
            std::path::PathBuf::from(r"C:\Windows\System32\drivers\etc\hosts")
        } else {
            std::path::PathBuf::from("/etc/hosts")
        };
        let err = super::resolve_model_path_confined(root, None, root, outside.to_str().unwrap())
            .unwrap_err();
        assert!(err.contains("outside the confine root"), "{err}");
        // Relative inside-root path succeeds.
        let ok = super::resolve_model_path_confined(root, None, root, "src/a.rs").unwrap();
        assert!(super::path_is_under_confine_root(&ok, root));
    }

    #[test]
    fn resolve_write_model_path_confines_when_root_set() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ok = super::resolve_write_model_path(root, None, Some(root), "a/b.rs").unwrap();
        assert!(super::path_is_under_confine_root(&ok, root));
        let outside = if cfg!(windows) {
            r"C:\Windows\System32\drivers\etc\hosts"
        } else {
            "/etc/hosts"
        };
        assert!(
            super::resolve_write_model_path(root, None, Some(root), outside).is_err(),
            "outside absolute must fail when confined"
        );
        // Without confine root, absolute is accepted (unconfined default).
        let bare = super::resolve_write_model_path(root, None, None, outside).unwrap();
        assert_eq!(bare, std::path::PathBuf::from(outside));
    }

    #[test]
    fn path_is_under_any_root_accepts_extra_and_rejects_outside() {
        let primary = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let inside_extra = extra.path().join("lib.rs");
        std::fs::write(&inside_extra, "fn x() {}").unwrap();
        let roots = vec![
            primary.path().to_path_buf(),
            extra.path().to_path_buf(),
        ];
        assert!(super::path_is_under_any_root(&inside_extra, &roots));
        assert!(super::path_is_under_any_root(primary.path(), &roots));
        let outside = primary.path().parent().unwrap().join("not-attached.txt");
        assert!(!super::path_is_under_any_root(&outside, &roots));
    }

    #[test]
    fn collect_write_roots_with_extras_uses_cwd_when_unconfined() {
        let cwd = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let roots = super::collect_write_roots(cwd.path(), None, &[extra.path().to_path_buf()]);
        assert_eq!(roots.len(), 2);
        assert!(super::path_is_under_any_root(&cwd.path().join("a.rs"), &roots));
        assert!(super::path_is_under_any_root(&extra.path().join("b.rs"), &roots));
    }

    #[test]
    fn collect_write_roots_without_extras_is_empty_when_unconfined() {
        let cwd = tempfile::tempdir().unwrap();
        let roots = super::collect_write_roots(cwd.path(), None, &[]);
        assert!(roots.is_empty(), "unconfined + no extras stays unconfined");
    }

    #[test]
    fn parse_confine_path_list_semicolon_splits_and_keeps_windows_drive() {
        let got = super::parse_confine_path_list(r"C:\work;D:\other");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], std::path::PathBuf::from(r"C:\work"));
        assert_eq!(got[1], std::path::PathBuf::from(r"D:\other"));
        let single = super::parse_confine_path_list(r"C:\work");
        if cfg!(windows) {
            assert_eq!(single, vec![std::path::PathBuf::from(r"C:\work")]);
        } else {
            // Unix splits a `;`-less list on `:`; a lone drive path is not a
            // Unix root, so the documented rule applies.
            assert_eq!(single.len(), 2);
        }
        assert!(super::parse_confine_path_list("").is_empty());
        assert!(super::parse_confine_path_list("   ").is_empty());
    }

    #[test]
    fn parse_confine_path_list_unix_colon_when_no_semicolon() {
        let got = super::parse_confine_path_list("/a:/b");
        if cfg!(windows) {
            assert_eq!(got, vec![std::path::PathBuf::from("/a:/b")]);
        } else {
            assert_eq!(
                got,
                vec![
                    std::path::PathBuf::from("/a"),
                    std::path::PathBuf::from("/b")
                ]
            );
        }
        let semi_wins = super::parse_confine_path_list("/a:/b;/c");
        assert_eq!(semi_wins.len(), 2);
        assert_eq!(semi_wins[0], std::path::PathBuf::from("/a:/b"));
        assert_eq!(semi_wins[1], std::path::PathBuf::from("/c"));
    }

    #[test]
    fn join_confine_path_list_uses_semicolon() {
        let joined = super::join_confine_path_list(&[
            std::path::PathBuf::from("/a"),
            std::path::PathBuf::from("/b"),
        ]);
        assert_eq!(joined, "/a;/b");
    }

    #[test]
    fn enforce_write_path_in_roots_missing_extra_does_not_tombstone() {
        let cwd = tempfile::tempdir().unwrap();
        let gone = cwd.path().join("gone-extra");
        let roots = vec![cwd.path().to_path_buf(), gone];
        super::enforce_write_path_in_roots(cwd.path(), &roots)
            .expect("live primary root must keep writes open");
    }

    #[test]
    fn enforce_write_path_in_roots_all_roots_gone_is_tombstone() {
        let cwd = tempfile::tempdir().unwrap();
        let gone = cwd.path().join("gone-confine");
        std::fs::create_dir(&gone).unwrap();
        let roots = vec![gone.clone()];
        std::fs::remove_dir(&gone).unwrap();
        match super::enforce_write_path_in_roots(cwd.path(), &roots) {
            Err(super::WriteRootError::ConfineRootMissing { path }) => {
                assert_eq!(path, gone);
            }
            other => panic!("expected ConfineRootMissing, got {other:?}"),
        }
    }

    #[test]
    fn resolve_model_path_confined_to_roots_allows_extra() {
        let primary = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let extra_file = extra.path().join("shared.rs");
        std::fs::write(&extra_file, "// extra").unwrap();
        let roots = vec![
            primary.path().to_path_buf(),
            extra.path().to_path_buf(),
        ];
        let ok = super::resolve_model_path_confined_to_roots(
            primary.path(),
            None,
            &roots,
            extra_file.to_str().unwrap(),
        )
        .expect("extra root must be writable");
        assert!(super::path_is_under_any_root(&ok, &roots));
        let outside = if cfg!(windows) {
            r"C:\Windows\System32\drivers\etc\hosts"
        } else {
            "/etc/hosts"
        };
        let err = super::resolve_model_path_confined_to_roots(
            primary.path(),
            None,
            &roots,
            outside,
        )
        .unwrap_err();
        assert!(err.contains("outside the confine root"), "{err}");
    }

    #[test]
    fn enforce_allowed_write_paths_rejects_dotdot_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        // Path under cwd that normalizes outside allowlist prefix via ..
        let target = cwd.join("docs").join("..").join("secret.txt");
        let err = super::enforce_allowed_write_paths(cwd, &target, &["docs".into()]).unwrap_err();
        assert_eq!(err.path, "secret.txt");
        // Clean under-prefix path is ok.
        let ok_path = cwd.join("docs").join("a.md");
        super::enforce_allowed_write_paths(cwd, &ok_path, &["docs".into()]).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn enforce_allowed_write_paths_refuses_symlink_prefix_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(cwd.join("ok")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, cwd.join("Schedules")).unwrap();
        let target = cwd.join("Schedules").join("authorized_keys");
        let err = super::enforce_allowed_write_paths(cwd, &target, &["Schedules".into()])
            .expect_err("symlink out of tree must fail closed");
        assert!(err.message().contains("allowed_paths") || err.message().contains("Re-spawn"));
        super::enforce_allowed_write_paths(cwd, &cwd.join("ok").join("a.md"), &["ok".into()])
            .unwrap();
    }

    #[test]
    fn enforce_allowed_write_paths_requires_gitattributes_on_allowlist() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let ga = cwd.join(".gitattributes");
        let err =
            super::enforce_allowed_write_paths(cwd, &ga, &["crates/foo/".into()]).unwrap_err();
        assert!(err.message().contains("Re-spawn with allowed_paths"));
        let gi = cwd.join(".gitignore");
        assert!(super::enforce_allowed_write_paths(cwd, &gi, &["crates/foo/".into()]).is_err());
        super::enforce_allowed_write_paths(cwd, &ga, &[".gitattributes".into()]).unwrap();
        super::enforce_allowed_write_paths(cwd, &gi, &[".gitignore".into()]).unwrap();
        let nested = cwd.join("crates").join(".gitattributes");
        let err = super::enforce_allowed_write_paths(cwd, &nested, &["docs".into()]).unwrap_err();
        assert!(err.message().contains("Re-spawn with allowed_paths"));
    }

    /// WP-H4 regression: table-driven confine escapes that previously wrote
    /// into a denied checkout because matching was lexical only.
    #[test]
    fn confine_canonical_path_table() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("wt");
        let sibling = base.path().join("Main Repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(root.join("ok.txt"), "in").unwrap();
        std::fs::write(sibling.join("seed.txt"), "out").unwrap();

        // Write inside the root — allowed.
        assert!(
            super::path_is_under_confine_root(&root.join("ok.txt"), &root),
            "in-root write must be allowed"
        );

        // Absolute outside, forward slashes.
        let outside_fwd = sibling.join("a.txt");
        let outside_fwd_str = outside_fwd.to_string_lossy().replace('\\', "/");
        assert!(
            !super::path_is_under_confine_root(std::path::Path::new(&outside_fwd_str), &root),
            "absolute outside (forward slashes) must be denied"
        );

        // Absolute outside, backslashes (Windows) / native separators.
        assert!(
            !super::path_is_under_confine_root(&sibling.join("b.txt"), &root),
            "absolute outside (native separators) must be denied"
        );

        // Different case on Windows.
        #[cfg(windows)]
        {
            let lower = sibling.to_string_lossy().to_lowercase() + "\\c.txt";
            assert!(
                !super::path_is_under_confine_root(std::path::Path::new(&lower), &root),
                "absolute outside (different case) must be denied"
            );
        }

        // Parent traversal: `<root>/../<sibling>/x.txt`.
        let traversal = root.join("..").join("Main Repo").join("d.txt");
        assert!(
            !super::path_is_under_confine_root(&traversal, &root),
            "parent traversal into sibling must be denied"
        );

        // Extended-length `\\?\` form (Windows).
        #[cfg(windows)]
        {
            let extended = format!(r"\\?\{}", sibling.join("e.txt").display());
            assert!(
                !super::path_is_under_confine_root(std::path::Path::new(&extended), &root),
                "\\\\?\\ extended-length form outside root must be denied"
            );
        }

        // Non-existent parent inside root — allowed (write target).
        let nested_new = root.join("sub").join("deep").join("new.txt");
        assert!(
            super::path_is_under_confine_root(&nested_new, &root),
            "non-existent path under root must be allowed after ancestor walk"
        );

        // Non-existent parent outside root — denied.
        let outside_new = sibling.join("missing").join("new.txt");
        assert!(
            !super::path_is_under_confine_root(&outside_new, &root),
            "non-existent path outside root must be denied"
        );

        // 8.3 short name of an ancestor (Windows only; skip if disabled).
        #[cfg(windows)]
        {
            match short_path_name(&sibling) {
                Ok(short) => {
                    let via_short = std::path::Path::new(&short).join("h.txt");
                    assert!(
                        !super::path_is_under_confine_root(&via_short, &root),
                        "8.3 short-name form of sibling must be denied (got short={short})"
                    );
                    // Ancestor walk: non-existent file under short-name parent.
                    let via_short_new = std::path::Path::new(&short).join("brand_new.txt");
                    assert!(
                        !super::path_is_under_confine_root(&via_short_new, &root),
                        "write under 8.3 short-name ancestor must be denied"
                    );
                }
                Err(reason) => {
                    // Do not silently pass — surface why the case was skipped.
                    eprintln!("skipping 8.3 short-name confine case: {reason}");
                }
            }
        }
    }

    #[test]
    fn canonicalize_for_permission_resolves_dotdot_via_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("wt");
        std::fs::create_dir_all(&root).unwrap();
        let via_dotdot = root.join("sub").join("..").join("file.txt");
        let c = super::canonicalize_for_permission(&via_dotdot);
        assert!(!c.lexical_only, "existing ancestor must resolve");
        assert!(
            c.compare
                .starts_with(&super::canonicalize_for_permission(&root).compare),
            "dotdot path should land under root: {:?}",
            c.display
        );
    }

    /// NTFS directories can be case-sensitive (`fsutil file setCaseSensitiveInfo`;
    /// WSL creates them that way). Inside one, `app` and `APP` are different
    /// directories, so a confine root must not admit its case-distinct sibling.
    #[cfg(windows)]
    #[test]
    fn confine_rejects_case_distinct_sibling_in_case_sensitive_dir() {
        let Some(base) = case_sensitive_tempdir_or_skip() else {
            return;
        };
        let root = base.path().join("app");
        let sibling = base.path().join("APP");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&sibling).unwrap_or_else(|e| {
            panic!(
                "fsutil reported success but `APP` collides with `app`, so `{}` is not \
                 case-sensitive: {e}",
                base.path().display()
            )
        });
        std::fs::write(root.join("in.txt"), "in").unwrap();
        std::fs::write(sibling.join("out.txt"), "out").unwrap();

        assert!(super::path_is_under_confine_root(
            &root.join("in.txt"),
            &root
        ));
        assert!(super::path_is_under_confine_root(
            &root.join("new").join("file.txt"),
            &root
        ));
        assert!(
            !super::path_is_under_confine_root(&sibling.join("out.txt"), &root),
            "a file in the case-distinct sibling must be outside the root"
        );
        assert!(
            !super::path_is_under_confine_root(&sibling, &root),
            "the case-distinct sibling itself must be outside the root"
        );
        assert!(
            !super::path_is_under_confine_root(&sibling.join("new").join("file.txt"), &root),
            "a write target under the case-distinct sibling must be outside the root"
        );
        assert!(
            !super::path_is_under_confine_root(&root.join("in.txt"), &sibling),
            "the sibling root must not admit the original either"
        );
        // A case variant that does not exist yet would be created as a third
        // directory, not open the root.
        assert!(
            !super::path_is_under_confine_root(&base.path().join("App").join("new.txt"), &root),
            "a write that creates a new case variant of the root must be outside it"
        );
        assert!(!super::path_is_under_any_root(
            &sibling.join("out.txt"),
            std::slice::from_ref(&root)
        ));
    }

    /// A root removed from a case-sensitive directory has no on-disk spelling
    /// left, and a write to a case variant of it would create a different
    /// directory, so the case variant must not be admitted.
    #[cfg(windows)]
    #[test]
    fn confine_rejects_case_variant_of_removed_root_in_case_sensitive_dir() {
        let Some(base) = case_sensitive_tempdir_or_skip() else {
            return;
        };
        let root = base.path().join("gone");
        std::fs::create_dir(&root).unwrap();
        std::fs::remove_dir(&root).unwrap();

        assert!(
            super::path_is_under_confine_root(&root.join("new.txt"), &root),
            "the removed root's own spelling still matches"
        );
        assert!(
            !super::path_is_under_confine_root(&base.path().join("GONE").join("new.txt"), &root),
            "a case variant of a removed root must be outside it"
        );
    }

    /// An existing path longer than `MAX_PATH` canonicalizes to a `\\?\C:\`
    /// spelling while a short root keeps `C:\`. Both name the same volume, so
    /// containment must not depend on which spelling came back.
    #[cfg(windows)]
    #[test]
    fn confine_accepts_existing_long_path_under_short_root() {
        let root = tempfile::tempdir().unwrap();
        let deep = root
            .path()
            .join("a".repeat(90))
            .join("b".repeat(90))
            .join("c".repeat(90));
        std::fs::create_dir_all(&deep).unwrap();
        let file = deep.join("long.txt");
        std::fs::write(&file, "deep").unwrap();
        let resolved = super::canonicalize_for_permission(&file).display;
        assert!(
            matches!(
                resolved.components().next(),
                Some(std::path::Component::Prefix(prefix))
                    if matches!(prefix.kind(), std::path::Prefix::VerbatimDisk(_))
            ),
            "the fixture must canonicalize to a verbatim drive spelling: {}",
            resolved.display()
        );

        assert!(super::path_is_under_confine_root(&file, root.path()));
        assert!(!super::path_is_under_confine_root(
            &file,
            &root.path().join("b".repeat(90))
        ));
    }

    /// The allowlist is relative to the working directory, and a case-distinct
    /// sibling of that directory in a case-sensitive parent is not under it.
    #[cfg(windows)]
    #[test]
    fn allowed_write_paths_reject_case_distinct_sibling_of_cwd() {
        let Some(base) = case_sensitive_tempdir_or_skip() else {
            return;
        };
        let cwd = base.path().join("app");
        let sibling = base.path().join("APP");
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        std::fs::create_dir_all(sibling.join("src")).unwrap();
        let allowed = ["src".to_string()];

        super::enforce_allowed_write_paths(&cwd, &cwd.join("src").join("a.rs"), &allowed)
            .expect("a write under the working directory is allowed");
        super::enforce_allowed_write_paths(&cwd, &sibling.join("src").join("a.rs"), &allowed)
            .expect_err("a write under the case-distinct sibling must be refused");
    }

    /// Case-distinct directories in a case-sensitive parent are two roots, not
    /// one root spelled twice.
    #[cfg(windows)]
    #[test]
    fn collect_write_roots_keeps_case_distinct_extra_root() {
        let Some(base) = case_sensitive_tempdir_or_skip() else {
            return;
        };
        let cwd = base.path().join("app");
        let extra = base.path().join("APP");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(&extra).unwrap();

        let roots = super::collect_write_roots(&cwd, None, std::slice::from_ref(&extra));
        assert_eq!(roots, vec![cwd, extra]);
    }

    /// An extra root with no resolvable ancestor still merges with an identical
    /// spelling of itself.
    #[test]
    fn collect_write_roots_merges_identical_unresolvable_extras() {
        let cwd = tempfile::tempdir().unwrap();
        let unresolvable = std::path::PathBuf::from("no-such-relative-extra-root");
        let roots = super::collect_write_roots(
            cwd.path(),
            None,
            &[unresolvable.clone(), unresolvable.clone()],
        );
        assert_eq!(roots, vec![cwd.path().to_path_buf(), unresolvable]);
    }

    /// A root typed in a different letter case from its on-disk name still
    /// confines to that directory in an ordinary case-insensitive directory:
    /// both sides resolve to the on-disk spelling before they are compared.
    #[cfg(windows)]
    #[test]
    fn confine_accepts_root_and_path_spelled_in_other_letter_case() {
        let base = tempfile::tempdir().unwrap();
        assert_ne!(
            super::dir_case_sensitivity(base.path()),
            Some(true),
            "this test needs an ordinary case-insensitive temp directory"
        );
        let on_disk = base.path().join("Project");
        std::fs::create_dir_all(on_disk.join("src")).unwrap();
        std::fs::write(on_disk.join("src").join("lib.rs"), "").unwrap();
        let typed_root = base.path().join("PROJECT");

        assert!(super::path_is_under_confine_root(
            &base.path().join("project").join("SRC").join("LIB.rs"),
            &typed_root
        ));
        assert!(super::path_is_under_confine_root(
            &base.path().join(r"pRoJeCt\Src\new\mod.rs"),
            &typed_root
        ));
        assert!(super::path_is_under_confine_root(&on_disk, &typed_root));
        assert!(super::path_is_under_confine_root(&typed_root, &on_disk));
        assert!(!super::path_is_under_confine_root(
            &base.path().join("project-evil").join("x.rs"),
            &typed_root
        ));
    }

    /// Where neither the root nor the path exists yet there is no on-disk
    /// spelling to compare, so in an ordinary case-insensitive directory letter
    /// case is still ignored on Windows.
    #[cfg(windows)]
    #[test]
    fn confine_ignores_case_only_where_neither_side_exists() {
        let base = tempfile::tempdir().unwrap();
        assert_eq!(
            super::dir_case_sensitivity(base.path()),
            Some(false),
            "this test needs a temp directory whose case sensitivity can be read and is off"
        );
        let root = base.path().join("Gone").join("Sub");
        assert!(super::path_is_under_confine_root(
            &base.path().join("gone").join("SUB").join("x.txt"),
            &root
        ));
        assert!(!super::path_is_under_confine_root(
            &base.path().join("gone").join("SUB-evil").join("x.txt"),
            &root
        ));
    }

    /// A `..` in the not-yet-existing tail can step back into directories that
    /// do exist. What follows it must be resolved like any other component, or
    /// `root/missing/../link/x` is judged lexically while the write goes
    /// through the `link` symlink.
    #[cfg(unix)]
    #[test]
    fn confine_resolves_symlink_reached_through_dotdot_in_missing_tail() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("root");
        let outside = base.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        let via_dotdot = root.join("missing").join("..").join("link").join("x.txt");
        assert!(
            !super::path_is_under_confine_root(&via_dotdot, &root),
            "a symlink reached after `..` in a missing tail must be resolved"
        );
        assert_eq!(
            super::canonicalize_for_permission(&via_dotdot).display,
            dunce::canonicalize(&outside).unwrap().join("x.txt")
        );
        let back_through_root = root
            .join("missing")
            .join("..")
            .join("..")
            .join("root")
            .join("link")
            .join("y.txt");
        assert!(!super::path_is_under_confine_root(
            &back_through_root,
            &root
        ));
        assert!(super::path_is_under_confine_root(
            &root.join("missing").join("..").join("new.txt"),
            &root
        ));
    }

    /// A temporary directory made case-sensitive with `fsutil`, created in
    /// `TURBO_CASE_SENSITIVE_TEST_DIR` when that is set and in the system temp
    /// directory otherwise. The caller proves a security property, so a host
    /// that cannot set the flag fails the test; `None` means the operator opted
    /// out with `TURBO_ALLOW_CASE_SENSITIVE_SKIP=1`.
    #[cfg(windows)]
    fn case_sensitive_tempdir_or_skip() -> Option<tempfile::TempDir> {
        let dir = match std::env::var_os("TURBO_CASE_SENSITIVE_TEST_DIR") {
            Some(parent) => tempfile::tempdir_in(&parent).unwrap_or_else(|e| {
                panic!(
                    "cannot create a directory in TURBO_CASE_SENSITIVE_TEST_DIR `{}`: {e}",
                    std::path::Path::new(&parent).display()
                )
            }),
            None => tempfile::tempdir().unwrap(),
        };
        let failure = match std::process::Command::new("fsutil")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(dir.path())
            .arg("enable")
            .output()
        {
            Ok(out) if out.status.success() => {
                assert_eq!(
                    super::dir_case_sensitivity(dir.path()),
                    Some(true),
                    "fsutil reported success, but `{}` does not read back as case-sensitive",
                    dir.path().display()
                );
                return Some(dir);
            }
            Ok(out) => format!(
                "fsutil exited with {}: {} {}",
                out.status,
                String::from_utf8_lossy(&out.stdout).trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => format!("fsutil could not be started: {e}"),
        };
        if std::env::var("TURBO_ALLOW_CASE_SENSITIVE_SKIP").as_deref() == Ok("1") {
            eprintln!(
                "SKIPPED (case-sensitive directory): {failure}; TURBO_ALLOW_CASE_SENSITIVE_SKIP=1"
            );
            return None;
        }
        panic!(
            "cannot make `{}` case-sensitive ({failure}). This test proves a security property \
             and must not pass silently. Changing the flag needs the \"Delete subfolders and \
             files\" permission on the directory, which Modify does not grant, and on some \
             Windows builds an elevated process: set TURBO_CASE_SENSITIVE_TEST_DIR to a \
             directory you have Full Control over, such as %LOCALAPPDATA%\\Temp, or set \
             TURBO_ALLOW_CASE_SENSITIVE_SKIP=1 to skip it explicitly.",
            dir.path().display()
        );
    }

    /// Resolve the 8.3 short path for `path`, or a skip reason when generation
    /// is disabled on the volume (`fsutil 8dot3name` off).
    #[cfg(windows)]
    fn short_path_name(path: &std::path::Path) -> Result<String, String> {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows::Win32::Storage::FileSystem::GetShortPathNameW;
        use windows::core::PCWSTR;

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let long = PCWSTR(wide.as_ptr());
        // SAFETY: `wide` is NUL-terminated and lives for both calls; first call
        // with None returns the required buffer length.
        let needed = unsafe { GetShortPathNameW(long, None) };
        if needed == 0 {
            return Err(
                "GetShortPathNameW failed (8.3 generation may be disabled on this volume)".into(),
            );
        }
        let mut buf = vec![0u16; needed as usize];
        let written = unsafe { GetShortPathNameW(long, Some(&mut buf)) };
        if written == 0 {
            return Err("GetShortPathNameW second call failed".into());
        }
        buf.truncate(written as usize);
        let short = std::ffi::OsString::from_wide(&buf);
        let short = short.to_string_lossy().into_owned();
        // If the "short" form still contains spaces, 8.3 is not active for this path.
        if short.contains(' ') {
            return Err(format!(
                "8.3 short name not generated (got `{short}`); volume may have 8dot3 disabled"
            ));
        }
        // Require at least one `~` segment so we know we actually got a short alias.
        if !short.contains('~') {
            return Err(format!(
                "no 8.3 tilde alias in `{short}`; 8dot3 generation appears disabled"
            ));
        }
        Ok(short)
    }
}
