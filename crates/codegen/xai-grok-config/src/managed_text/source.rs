use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::{ManagedConfigError, ManagedConfigPlan};

pub(super) const MAX_SYMLINKS: usize = 40;
pub(super) const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceState {
    pub bytes: Option<Vec<u8>>,
    pub hash: String,
    pub mode: Option<u32>,
    pub identity: Option<FileIdentity>,
}

impl SourceState {
    pub fn text<'a>(&'a self, path: &Path) -> Result<&'a str, ManagedConfigError> {
        match self.bytes.as_deref() {
            Some(bytes) => std::str::from_utf8(bytes).map_err(|_| ManagedConfigError::UnsafePath {
                path: path.to_path_buf(),
                reason: "file is not valid UTF-8".to_owned(),
            }),
            None => Ok(""),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ParentPlan {
    parent: PathBuf,
    existing_chain: Vec<PathIdentity>,
    first_missing: Option<PathBuf>,
}

impl ParentPlan {
    pub fn capture(parent: &Path) -> Result<Self, ManagedConfigError> {
        let mut chain = Vec::new();
        let mut current = PathBuf::new();
        let mut first_missing = None;
        for component in parent.components() {
            current.push(component.as_os_str());
            if matches!(component, Component::Prefix(_) | Component::RootDir) {
                continue;
            }
            match fs::symlink_metadata(&current) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(ManagedConfigError::UnsafePath {
                            path: current,
                            reason: "symlinked parent directory is not allowed".to_owned(),
                        });
                    }
                    if !metadata.is_dir() {
                        return Err(ManagedConfigError::UnsafePath {
                            path: current,
                            reason: "parent component is not a directory".to_owned(),
                        });
                    }
                    #[cfg(windows)]
                    let identity = FileIdentity::from_path(&current).map_err(|source| {
                        ManagedConfigError::Read {
                            path: current.clone(),
                            source,
                        }
                    })?;
                    #[cfg(not(windows))]
                    let identity = FileIdentity::from_metadata(&metadata);
                    chain.push(PathIdentity {
                        path: current.clone(),
                        identity,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    first_missing = Some(current.clone());
                    break;
                }
                Err(source) => {
                    return Err(ManagedConfigError::Read {
                        path: current,
                        source,
                    });
                }
            }
        }
        Ok(Self {
            parent: parent.to_path_buf(),
            existing_chain: chain,
            first_missing,
        })
    }

    pub fn ensure_and_anchor(&self) -> Result<ParentAnchor, ManagedConfigError> {
        self.revalidate_existing()?;
        fs::create_dir_all(&self.parent).map_err(|source| ManagedConfigError::Write {
            path: self.parent.clone(),
            source,
        })?;
        self.revalidate_existing()?;
        let current = Self::capture(&self.parent)?;
        if current.first_missing.is_some()
            || !current.existing_chain.starts_with(&self.existing_chain)
        {
            return Err(ManagedConfigError::ParentChanged(self.parent.clone()));
        }
        ParentAnchor::capture(&self.parent)
    }

    pub fn revalidate_planned(&self) -> Result<(), ManagedConfigError> {
        self.revalidate_existing()?;
        if self.first_missing.is_none() {
            let current = Self::capture(&self.parent)?;
            if current.existing_chain != self.existing_chain {
                return Err(ManagedConfigError::ParentChanged(self.parent.clone()));
            }
        }
        Ok(())
    }

    fn revalidate_existing(&self) -> Result<(), ManagedConfigError> {
        for expected in &self.existing_chain {
            let metadata = fs::symlink_metadata(&expected.path)
                .map_err(|_| ManagedConfigError::ParentChanged(expected.path.clone()))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ManagedConfigError::ParentChanged(expected.path.clone()));
            }
            #[cfg(windows)]
            let identity = FileIdentity::from_path(&expected.path)
                .map_err(|_| ManagedConfigError::ParentChanged(expected.path.clone()))?;
            #[cfg(not(windows))]
            let identity = FileIdentity::from_metadata(&metadata);
            if identity != expected.identity {
                return Err(ManagedConfigError::ParentChanged(expected.path.clone()));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct ParentAnchor {
    path: PathBuf,
    identity: FileIdentity,
    /// Kept open so the parent directory cannot be replaced under us (TOCTOU).
    #[allow(dead_code)]
    directory: fs::File,
}

/// Open a directory for read/handle hold. Unix: plain `File::open`. Windows:
/// requires `FILE_FLAG_BACKUP_SEMANTICS` or open fails with Access Denied (5).
fn open_directory_handle(path: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS = 0x02000000 — allow opening directories.
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }
    #[cfg(not(windows))]
    {
        fs::File::open(path)
    }
}

impl ParentAnchor {
    fn capture(path: &Path) -> Result<Self, ManagedConfigError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| ManagedConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ManagedConfigError::ParentChanged(path.to_path_buf()));
        }
        // Hold an open handle so the parent directory cannot be replaced under
        // us (TOCTOU). On Windows, plain File::open(dir) returns ERROR_ACCESS_DENIED
        // (5) unless FILE_FLAG_BACKUP_SEMANTICS is set — that is also the root
        // cause of managed_text test hangs (barrier never reached AfterLock).
        let directory = open_directory_handle(path).map_err(|source| ManagedConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        #[cfg(windows)]
        let identity =
            FileIdentity::from_file(&directory).map_err(|source| ManagedConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        #[cfg(not(windows))]
        let identity = FileIdentity::from_metadata(&metadata);
        Ok(Self {
            path: path.to_path_buf(),
            identity,
            directory,
        })
    }

    pub fn revalidate(&self) -> Result<(), ManagedConfigError> {
        let current = Self::capture(&self.path)?;
        if current.identity != self.identity {
            return Err(ManagedConfigError::ParentChanged(self.path.clone()));
        }
        Ok(())
    }

    pub fn sync(&self) -> Result<(), ManagedConfigError> {
        #[cfg(unix)]
        {
            self.directory
                .sync_all()
                .map_err(|source| ManagedConfigError::Sync {
                    path: self.path.clone(),
                    source,
                })
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PathIdentity {
    path: PathBuf,
    identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    /// Windows: volume serial + file index (stable when directory children change).
    /// Using mtime+len was wrong: parent dir mtime updates when we write config.rc
    /// inside a temp parent → false ParentChanged / flaky managed_text tests.
    #[cfg(windows)]
    volume_serial: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(not(any(unix, windows)))]
    len: u64,
    #[cfg(not(any(unix, windows)))]
    modified: Option<std::time::SystemTime>,
}

impl FileIdentity {
    // Non-Windows only. Every Windows call site builds the identity from a
    // handle or a path instead, so on Windows this would be dead code.
    #[cfg(not(windows))]
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
            }
        }
        #[cfg(windows)]
        {
            // Prefer handle-based identity when possible; metadata path is used
            // only for the parent-chain PathIdentity snapshot (open each path).
            let _ = metadata;
            Self {
                volume_serial: 0,
                file_index: 0,
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Self {
                len: metadata.len(),
                modified: metadata.modified().ok(),
            }
        }
    }

    #[cfg(windows)]
    fn from_file(file: &fs::File) -> io::Result<Self> {
        use std::mem::MaybeUninit;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        // SAFETY: valid process-owned file handle; info filled by Win32.
        unsafe {
            let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
            let ok = GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr());
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let info = info.assume_init();
            let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
            Ok(Self {
                volume_serial: info.dwVolumeSerialNumber,
                file_index,
            })
        }
    }

    #[cfg(windows)]
    fn from_path(path: &Path) -> io::Result<Self> {
        let file = open_directory_or_file(path)?;
        Self::from_file(&file)
    }
}

/// Open a file or directory for identity probing (Windows directory open needs
/// BACKUP_SEMANTICS; regular files use a normal read open).
#[cfg(windows)]
fn open_directory_or_file(path: &Path) -> io::Result<fs::File> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        open_directory_handle(path)
    } else {
        fs::File::open(path)
    }
}

pub(super) fn absolute_lexical(path: &Path) -> Result<PathBuf, ManagedConfigError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| ManagedConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?
            .join(path)
    };
    Ok(normalize_lexically(&absolute))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

pub(super) fn resolve_final_symlink(path: &Path) -> Result<PathBuf, ManagedConfigError> {
    let mut current = physicalize_parent(path)?;
    let mut followed = false;
    let mut seen = HashSet::new();
    for _ in 0..MAX_SYMLINKS {
        if !seen.insert(current.clone()) {
            return Err(ManagedConfigError::UnsafePath {
                path: path.to_path_buf(),
                reason: "symlink cycle detected".to_owned(),
            });
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                followed = true;
                let link = fs::read_link(&current).map_err(|source| ManagedConfigError::Read {
                    path: current.clone(),
                    source,
                })?;
                current = if link.is_absolute() {
                    normalize_lexically(&link)
                } else {
                    normalize_lexically(
                        &current
                            .parent()
                            .unwrap_or_else(|| Path::new("/"))
                            .join(link),
                    )
                };
            }
            Ok(_) => return Ok(current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !followed => {
                return Ok(current);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ManagedConfigError::UnsafePath {
                    path: path.to_path_buf(),
                    reason: "symlink target does not exist".to_owned(),
                });
            }
            Err(source) => {
                return Err(ManagedConfigError::Read {
                    path: current,
                    source,
                });
            }
        }
    }
    Err(ManagedConfigError::UnsafePath {
        path: path.to_path_buf(),
        reason: format!("symlink chain exceeds {MAX_SYMLINKS} links"),
    })
}

fn physicalize_parent(path: &Path) -> Result<PathBuf, ManagedConfigError> {
    let Some(parent) = path.parent() else {
        return Ok(path.to_path_buf());
    };
    let mut probe = parent;
    let mut missing = Vec::new();
    loop {
        match dunce::canonicalize(probe) {
            Ok(canonical) => {
                let mut physical = canonical;
                for component in missing.iter().rev() {
                    physical.push(component);
                }
                if let Some(name) = path.file_name() {
                    physical.push(name);
                }
                return Ok(physical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = probe
                    .file_name()
                    .ok_or_else(|| ManagedConfigError::UnsafePath {
                        path: path.to_path_buf(),
                        reason: "could not resolve config parent".to_owned(),
                    })?;
                missing.push(name.to_os_string());
                probe = probe
                    .parent()
                    .ok_or_else(|| ManagedConfigError::UnsafePath {
                        path: path.to_path_buf(),
                        reason: "could not resolve config parent".to_owned(),
                    })?;
            }
            Err(source) => {
                return Err(ManagedConfigError::Read {
                    path: probe.to_path_buf(),
                    source,
                });
            }
        }
    }
}

pub(super) fn read_source(path: &Path) -> Result<SourceState, ManagedConfigError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SourceState {
                bytes: None,
                hash: blake3::hash(&[]).to_hex().to_string(),
                mode: default_mode(),
                identity: None,
            });
        }
        Err(source) => {
            return Err(ManagedConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.file_type().is_file() {
        return Err(ManagedConfigError::UnsafePath {
            path: path.to_path_buf(),
            reason: "target is not a regular file".to_owned(),
        });
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(ManagedConfigError::UnsafePath {
            path: path.to_path_buf(),
            reason: format!("file exceeds {MAX_CONFIG_BYTES} bytes"),
        });
    }
    let bytes = fs::read(path).map_err(|source| ManagedConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if bytes.contains(&0) {
        return Err(ManagedConfigError::UnsafePath {
            path: path.to_path_buf(),
            reason: "file contains NUL bytes".to_owned(),
        });
    }
    #[cfg(windows)]
    let identity =
        Some(
            FileIdentity::from_path(path).map_err(|source| ManagedConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?,
        );
    #[cfg(not(windows))]
    let identity = Some(FileIdentity::from_metadata(&metadata));
    Ok(SourceState {
        hash: blake3::hash(&bytes).to_hex().to_string(),
        bytes: Some(bytes),
        mode: file_mode(&metadata),
        identity,
    })
}

pub(super) fn revalidate(plan: &ManagedConfigPlan) -> Result<(), ManagedConfigError> {
    plan.parent_plan.revalidate_planned()?;
    let target = resolve_final_symlink(&plan.requested_path)?;
    if target != plan.target_path {
        return Err(ManagedConfigError::StalePlan(plan.requested_path.clone()));
    }
    let current = read_source(&target)?;
    if current != plan.original {
        return Err(ManagedConfigError::StalePlan(plan.requested_path.clone()));
    }
    Ok(())
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn file_mode(_: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn default_mode() -> Option<u32> {
    Some(0o644)
}

#[cfg(not(unix))]
fn default_mode() -> Option<u32> {
    None
}
