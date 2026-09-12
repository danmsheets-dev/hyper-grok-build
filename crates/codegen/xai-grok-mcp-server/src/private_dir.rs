//! The per-process private session folder.
//!
//! Tools may keep session state in the session folder, so it must be readable
//! by its owner only. (Served edits keep no receipts there: the toolset's
//! `ServedEditPolicy` turns them off.) A fixed shared name under the system temp
//! directory let other local users pre-create it and plant symlinks, and kept
//! whatever was written there around indefinitely.
//!
//! - **Unix:** `mkdir` creates it with mode `0700`, so it never exists with a
//!   wider mode.
//! - **Windows:** it is created, then given a protected DACL that grants only
//!   the current user and is inherited by everything created inside it. If
//!   anything appeared in it before the DACL was applied, creation fails.
//!
//! The folder is removed when its [`SessionDir`] drops. Release builds abort on
//! panic, which skips destructors, so live folders are also registered for
//! [`remove_session_dirs_for_abort`], which a panic hook calls.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, TryLockError};

static SESSION_DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A private session folder, removed on drop.
pub(crate) struct SessionDir {
    dir: tempfile::TempDir,
}

impl SessionDir {
    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for SessionDir {
    fn drop(&mut self) {
        let mut dirs = SESSION_DIRS.lock().unwrap_or_else(|e| e.into_inner());
        dirs.retain(|dir| dir != self.dir.path());
        // `dir` is dropped after this, which removes the folder.
    }
}

/// Remove every live session folder. For a panic hook under `panic = "abort"`,
/// where destructors do not run and the folders would stay behind.
pub fn remove_session_dirs_for_abort() {
    // Never block in a panic hook: the panic may have happened while the
    // registry was locked.
    let dirs = match SESSION_DIRS.try_lock() {
        Ok(dirs) => dirs.clone(),
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().clone(),
        Err(TryLockError::WouldBlock) => return,
    };
    for dir in dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
}

pub(crate) fn create() -> io::Result<SessionDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("turbo-mcp-serve-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let dir = builder.tempdir()?;
    #[cfg(windows)]
    {
        win::restrict_to_current_user(dir.path())?;
        if std::fs::read_dir(dir.path())?.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the session folder changed before it could be made private",
            ));
        }
    }
    SESSION_DIRS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(dir.path().to_path_buf());
    Ok(SessionDir { dir })
}

#[cfg(test)]
pub(crate) fn registered_session_dirs() -> Vec<PathBuf> {
    SESSION_DIRS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[cfg(windows)]
pub(crate) mod win {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
        SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// Memory a Win32 call allocated with `LocalAlloc`, freed once on drop.
    struct LocalMemory(*mut core::ffi::c_void);

    impl Drop for LocalMemory {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: returned by the Win32 call that allocated it, freed once.
                unsafe { LocalFree(self.0) };
            }
        }
    }

    /// The current process token's user SID, owned by its buffer.
    struct CurrentUser {
        token_user: Vec<u8>,
    }

    impl CurrentUser {
        fn query() -> io::Result<Self> {
            // SAFETY: plain Win32 calls with valid out-pointers; the token handle
            // is closed on every path.
            unsafe {
                let mut token: HANDLE = std::ptr::null_mut();
                if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut len = 0u32;
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
                if len == 0 {
                    let err = io::Error::last_os_error();
                    CloseHandle(token);
                    return Err(err);
                }
                let mut token_user = vec![0u8; len as usize];
                let ok = GetTokenInformation(
                    token,
                    TokenUser,
                    token_user.as_mut_ptr().cast(),
                    len,
                    &mut len,
                );
                let err = io::Error::last_os_error();
                CloseHandle(token);
                if ok == 0 {
                    return Err(err);
                }
                Ok(Self { token_user })
            }
        }

        fn sid(&self) -> *mut core::ffi::c_void {
            // SAFETY: the buffer holds the TOKEN_USER GetTokenInformation wrote;
            // its SID pointer points into the same buffer, which outlives `self`.
            unsafe {
                std::ptr::read_unaligned(self.token_user.as_ptr().cast::<TOKEN_USER>())
                    .User
                    .Sid
            }
        }
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Replace `path`'s DACL with a protected one granting only the current user,
    /// inherited by files and folders created inside it.
    ///
    /// Specific rights, not `GENERIC_ALL`: Windows splits an inheritable generic
    /// grant into an effective entry and an inherit-only entry.
    pub(crate) fn restrict_to_current_user(path: &Path) -> io::Result<()> {
        let user = CurrentUser::query()?;
        let name = wide(path);
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS,
            grfAccessMode: SET_ACCESS,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: user.sid().cast(),
            },
        };
        // SAFETY: every pointer is valid for the duration of the calls; the new
        // ACL is freed by `LocalMemory`.
        unsafe {
            let mut acl: *mut ACL = std::ptr::null_mut();
            let rc = SetEntriesInAclW(1, &access, std::ptr::null(), &mut acl);
            if rc != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
            let _acl = LocalMemory(acl.cast());
            let rc = SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            );
            if rc != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
        }
        Ok(())
    }

    /// One access entry of a DACL.
    #[cfg(test)]
    #[derive(Debug)]
    pub(crate) struct AceInfo {
        /// An access-allowed entry for the current user.
        pub allows_current_user: bool,
        pub flags: u8,
        pub mask: u32,
    }

    /// A DACL as the tests need to see it.
    #[cfg(test)]
    #[derive(Debug)]
    pub(crate) struct DaclInfo {
        pub protected: bool,
        pub entries: Vec<AceInfo>,
    }

    /// Read `path`'s DACL. Used by the tests to prove the folder is private.
    #[cfg(test)]
    pub(crate) fn describe_dacl(path: &Path) -> io::Result<DaclInfo> {
        use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
        use windows_sys::Win32::Security::{
            ACCESS_ALLOWED_ACE, EqualSid, GetAce, GetSecurityDescriptorControl,
            PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
        };
        /// `ACCESS_ALLOWED_ACE_TYPE`.
        const ACCESS_ALLOWED: u8 = 0;

        let user = CurrentUser::query()?;
        let name = wide(path);
        // SAFETY: out-pointers are valid; the security descriptor, which owns the
        // DACL, is freed by `LocalMemory` after the last read through it.
        unsafe {
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            let rc = GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            );
            if rc != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
            let _descriptor = LocalMemory(descriptor);

            let mut control = 0u16;
            let mut revision = 0u32;
            if GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) == 0 {
                return Err(io::Error::last_os_error());
            }
            let protected = control & SE_DACL_PROTECTED != 0;
            let mut entries = Vec::new();
            if !dacl.is_null() {
                for index in 0..u32::from((*dacl).AceCount) {
                    let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                    if GetAce(dacl, index, &mut ace) == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
                    // Only an access-allowed entry has its SID at `SidStart`; the
                    // type check short-circuits before the SID is read.
                    let allows_current_user = (*ace).Header.AceType == ACCESS_ALLOWED
                        && EqualSid(
                            std::ptr::addr_of_mut!((*ace).SidStart).cast::<core::ffi::c_void>(),
                            user.sid(),
                        ) != 0;
                    entries.push(AceInfo {
                        allows_current_user,
                        flags: (*ace).Header.AceFlags,
                        mask: (*ace).Mask,
                    });
                }
            }
            Ok(DaclInfo { protected, entries })
        }
    }
}
