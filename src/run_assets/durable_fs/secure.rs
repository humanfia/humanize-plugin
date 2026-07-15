use std::ffi::{CStr, CString, OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(crate) struct SecureFsError {
    message: String,
    kind: io::ErrorKind,
}

impl SecureFsError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: io::ErrorKind::InvalidData,
        }
    }

    fn from_io(context: impl fmt::Display, error: io::Error) -> Self {
        Self {
            message: format!("{context}: {error}"),
            kind: error.kind(),
        }
    }

    pub(crate) fn kind(&self) -> io::ErrorKind {
        self.kind
    }
}

impl fmt::Display for SecureFsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SecureFsError {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum EntryKind {
    Directory,
    Regular,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SecureEntry {
    pub(crate) name: OsString,
    pub(crate) kind: EntryKind,
    pub(crate) uid: u32,
    pub(crate) mode: u32,
    pub(crate) nlink: u64,
}

#[derive(Debug)]
pub(crate) struct SecureDir {
    file: File,
}

pub(crate) fn open_parent(
    path: &Path,
    create: bool,
) -> Result<(SecureDir, OsString), SecureFsError> {
    let name = path
        .file_name()
        .ok_or_else(|| SecureFsError::new("path must have a final component"))?
        .to_os_string();
    validate_component(&name)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok((open_dir_path(parent, create, false)?, name))
}

pub(crate) fn open_dir_path(
    path: &Path,
    create: bool,
    private_final: bool,
) -> Result<SecureDir, SecureFsError> {
    let absolute = path.is_absolute();
    let start = if absolute {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut current = SecureDir::open_start(start)?;
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::RootDir | Component::CurDir => None,
            Component::Normal(name) => Some(Ok(name.to_os_string())),
            Component::ParentDir | Component::Prefix(_) => Some(Err(SecureFsError::new(
                "directory path must not contain '..'",
            ))),
        })
        .collect::<Result<Vec<_>, _>>()?;

    for (index, name) in components.iter().enumerate() {
        let is_final = index + 1 == components.len();
        current = match current.open_child_dir(name, private_final && is_final) {
            Ok(directory) => directory,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                match current.create_child_dir(name) {
                    Ok(directory) => directory,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        current.open_child_dir(name, true)?
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
    }

    if components.is_empty() && private_final {
        current.validate_private()?;
    }
    Ok(current)
}

impl SecureDir {
    fn open_start(path: &Path) -> Result<Self, SecureFsError> {
        let name = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| SecureFsError::new("directory path contains NUL"))?;
        let flags = libc::O_RDONLY
            | libc::O_DIRECTORY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK;
        // SAFETY: name is a valid NUL-terminated path and open returns a new descriptor.
        let fd = unsafe { libc::open(name.as_ptr(), flags) };
        if fd < 0 {
            return Err(SecureFsError::from_io(
                format_args!("open directory {}", path.display()),
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: fd is newly owned by this function.
        let file = unsafe { File::from_raw_fd(fd) };
        validate_directory_file(&file, false)?;
        Ok(Self { file })
    }

    pub(crate) fn try_clone(&self) -> Result<Self, SecureFsError> {
        Ok(Self {
            file: self
                .file
                .try_clone()
                .map_err(|error| SecureFsError::from_io("clone directory descriptor", error))?,
        })
    }

    pub(crate) fn validate_private(&self) -> Result<(), SecureFsError> {
        validate_directory_file(&self.file, true)
    }

    pub(crate) fn ensure_private(&self) -> Result<(), SecureFsError> {
        fchmod(&self.file, PRIVATE_DIR_MODE)?;
        self.validate_private()
    }

    pub(crate) fn create_child_dir(&self, name: &OsStr) -> Result<Self, SecureFsError> {
        let name = component_cstring(name)?;
        // SAFETY: self holds a directory descriptor and name is one NUL-terminated component.
        let result = unsafe {
            libc::mkdirat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                PRIVATE_DIR_MODE as libc::mode_t,
            )
        };
        if result != 0 {
            return Err(SecureFsError::from_io(
                "create private directory",
                io::Error::last_os_error(),
            ));
        }
        let directory = self.open_child_dir(OsStr::from_bytes(name.as_bytes()), false)?;
        fchmod(&directory.file, PRIVATE_DIR_MODE)?;
        directory.validate_private()?;
        directory.sync()?;
        self.sync()?;
        Ok(directory)
    }

    pub(crate) fn open_child_dir(
        &self,
        name: &OsStr,
        private: bool,
    ) -> Result<Self, SecureFsError> {
        let name = component_cstring(name)?;
        let flags = libc::O_RDONLY
            | libc::O_DIRECTORY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK;
        // SAFETY: self holds a directory descriptor and name is one NUL-terminated component.
        let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(SecureFsError::from_io(
                "open child directory",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: fd is newly owned by this function.
        let file = unsafe { File::from_raw_fd(fd) };
        validate_directory_file(&file, private)?;
        Ok(Self { file })
    }

    pub(crate) fn create_file(&self, name: &OsStr, bytes: &[u8]) -> Result<(), SecureFsError> {
        let name = component_cstring(name)?;
        let mut file = self.open_new_file(&name)?;
        file.write_all(bytes)
            .map_err(|error| SecureFsError::from_io("write private file", error))?;
        file.sync_all()
            .map_err(|error| SecureFsError::from_io("sync private file", error))?;
        drop(file);
        self.sync()
    }

    pub(crate) fn open_or_create_lock_file(&self, name: &OsStr) -> Result<File, SecureFsError> {
        let name = component_cstring(name)?;
        let create_flags = libc::O_RDWR
            | libc::O_CREAT
            | libc::O_EXCL
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK;
        // SAFETY: self holds a directory descriptor and name is one NUL-terminated component.
        let created_fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                create_flags,
                PRIVATE_FILE_MODE as libc::mode_t,
            )
        };
        let (file, created) = if created_fd >= 0 {
            // SAFETY: created_fd is newly owned by this function.
            (unsafe { File::from_raw_fd(created_fd) }, true)
        } else {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(SecureFsError::from_io("create private lock file", error));
            }
            let open_flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
            // SAFETY: self holds a directory descriptor and name is one NUL-terminated component.
            let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), open_flags) };
            if fd < 0 {
                return Err(SecureFsError::from_io(
                    "open private lock file",
                    io::Error::last_os_error(),
                ));
            }
            // SAFETY: fd is newly owned by this function.
            (unsafe { File::from_raw_fd(fd) }, false)
        };
        if created {
            fchmod(&file, PRIVATE_FILE_MODE)?;
        }
        validate_regular_file(&file)?;
        if created {
            self.sync()?;
        }
        Ok(file)
    }

    pub(crate) fn atomic_create_file(
        &self,
        name: &OsStr,
        bytes: &[u8],
    ) -> Result<(), SecureFsError> {
        self.atomic_write_file(name, bytes, false)
    }

    pub(crate) fn atomic_replace_file(
        &self,
        name: &OsStr,
        bytes: &[u8],
    ) -> Result<(), SecureFsError> {
        self.atomic_write_file(name, bytes, true)
    }

    fn atomic_write_file(
        &self,
        name: &OsStr,
        bytes: &[u8],
        replace: bool,
    ) -> Result<(), SecureFsError> {
        validate_component(name)?;
        let temporary = temporary_name(name, "tmp");
        self.create_file(&temporary, bytes)?;
        let result = if replace {
            self.rename_child(&temporary, name)
        } else {
            self.rename_child_noreplace(&temporary, name)
        };
        if result.is_err() {
            let _ = self.unlink_child(&temporary, false);
        }
        result
    }

    pub(crate) fn read_file(&self, name: &OsStr) -> Result<Vec<u8>, SecureFsError> {
        let name = component_cstring(name)?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: self holds a directory descriptor and name is one NUL-terminated component.
        let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(SecureFsError::from_io(
                "open private file",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: fd is newly owned by this function.
        let mut file = unsafe { File::from_raw_fd(fd) };
        validate_regular_file(&file)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| SecureFsError::from_io("read private file", error))?;
        Ok(bytes)
    }

    pub(crate) fn remove_regular_file(&self, name: &OsStr) -> Result<(), SecureFsError> {
        let component = component_cstring(name)?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: self holds a directory descriptor and name is one NUL-terminated component.
        let fd = unsafe { libc::openat(self.file.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 {
            return Err(SecureFsError::from_io(
                "open private file for removal",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: fd is newly owned by this function.
        let file = unsafe { File::from_raw_fd(fd) };
        validate_regular_file(&file)?;
        self.unlink_child(name, false)
    }

    pub(crate) fn entry(&self, name: &OsStr) -> Result<Option<SecureEntry>, SecureFsError> {
        let name = component_cstring(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: stat points to writable storage and name is valid for this directory descriptor.
        let result = unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(SecureFsError::from_io("inspect directory entry", error));
        }
        // SAFETY: fstatat initialized stat on success.
        Ok(Some(entry_from_stat(
            OsString::from_vec(name.as_bytes().to_vec()),
            unsafe { stat.assume_init() },
        )))
    }

    pub(crate) fn entries(&self) -> Result<Vec<SecureEntry>, SecureFsError> {
        // SAFETY: fcntl duplicates a valid descriptor and returns a separately owned descriptor.
        let duplicate = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            return Err(SecureFsError::from_io(
                "duplicate directory descriptor",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: duplicate is newly owned; fdopendir consumes it on success.
        let directory = unsafe { libc::fdopendir(duplicate) };
        if directory.is_null() {
            // SAFETY: duplicate remains owned here because fdopendir failed.
            unsafe { libc::close(duplicate) };
            return Err(SecureFsError::from_io(
                "open directory stream",
                io::Error::last_os_error(),
            ));
        }

        let result = (|| {
            let mut entries = Vec::new();
            loop {
                // SAFETY: directory is a live DIR pointer owned until closed below.
                let entry = unsafe { libc::readdir(directory) };
                if entry.is_null() {
                    break;
                }
                // SAFETY: d_name is NUL-terminated for a valid dirent.
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                if name.to_bytes() == b"." || name.to_bytes() == b".." {
                    continue;
                }
                let name = OsStr::from_bytes(name.to_bytes());
                if let Some(entry) = self.entry(name)? {
                    entries.push(entry);
                }
            }
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            Ok(entries)
        })();
        // SAFETY: directory is owned by this function and has not been closed yet.
        unsafe { libc::closedir(directory) };
        result
    }

    pub(crate) fn rename_child_noreplace(
        &self,
        old: &OsStr,
        new: &OsStr,
    ) -> Result<(), SecureFsError> {
        let old = component_cstring(old)?;
        let new = component_cstring(new)?;
        #[cfg(target_os = "linux")]
        {
            // SAFETY: both names are valid children of the held directory descriptor.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    self.file.as_raw_fd(),
                    old.as_ptr(),
                    self.file.as_raw_fd(),
                    new.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if result != 0 {
                return Err(SecureFsError::from_io(
                    "rename directory entry without replacement",
                    io::Error::last_os_error(),
                ));
            }
            self.sync()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (old, new);
            Err(SecureFsError::new(
                "atomic no-replace rename requires Linux renameat2",
            ))
        }
    }

    pub(crate) fn rename_child(&self, old: &OsStr, new: &OsStr) -> Result<(), SecureFsError> {
        let old = component_cstring(old)?;
        let new = component_cstring(new)?;
        // SAFETY: both names are valid children of the held directory descriptor.
        let result = unsafe {
            libc::renameat(
                self.file.as_raw_fd(),
                old.as_ptr(),
                self.file.as_raw_fd(),
                new.as_ptr(),
            )
        };
        if result != 0 {
            return Err(SecureFsError::from_io(
                "rename directory entry",
                io::Error::last_os_error(),
            ));
        }
        self.sync()
    }

    pub(crate) fn remove_child_tree(&self, name: &OsStr) -> Result<(), SecureFsError> {
        let Some(entry) = self.entry(name)? else {
            return Ok(());
        };
        if entry.kind == EntryKind::Directory {
            let child = self.open_child_dir(name, false)?;
            for nested in child.entries()? {
                child.remove_child_tree(&nested.name)?;
            }
            drop(child);
            self.unlink_child(name, true)
        } else {
            self.unlink_child(name, false)
        }
    }

    pub(crate) fn sync(&self) -> Result<(), SecureFsError> {
        self.file
            .sync_all()
            .map_err(|error| SecureFsError::from_io("sync directory", error))
    }

    fn open_new_file(&self, name: &CString) -> Result<File, SecureFsError> {
        let flags = libc::O_WRONLY
            | libc::O_CREAT
            | libc::O_EXCL
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK;
        // SAFETY: self holds a directory descriptor and name is one NUL-terminated component.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags,
                PRIVATE_FILE_MODE as libc::mode_t,
            )
        };
        if fd < 0 {
            return Err(SecureFsError::from_io(
                "create private file",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: fd is newly owned by this function.
        let file = unsafe { File::from_raw_fd(fd) };
        fchmod(&file, PRIVATE_FILE_MODE)?;
        validate_regular_file(&file)?;
        Ok(file)
    }

    fn unlink_child(&self, name: &OsStr, directory: bool) -> Result<(), SecureFsError> {
        let name = component_cstring(name)?;
        let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: name is a valid child of the held directory descriptor.
        let result = unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), flags) };
        if result != 0 {
            return Err(SecureFsError::from_io(
                "remove directory entry",
                io::Error::last_os_error(),
            ));
        }
        self.sync()
    }
}

fn component_cstring(name: &OsStr) -> Result<CString, SecureFsError> {
    validate_component(name)?;
    CString::new(name.as_bytes()).map_err(|_| SecureFsError::new("path component contains NUL"))
}

fn validate_component(name: &OsStr) -> Result<(), SecureFsError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(SecureFsError::new("path must be one safe component"));
    }
    Ok(())
}

fn validate_directory_file(file: &File, private: bool) -> Result<(), SecureFsError> {
    let metadata = file
        .metadata()
        .map_err(|error| SecureFsError::from_io("inspect directory descriptor", error))?;
    if !metadata.file_type().is_dir() {
        return Err(SecureFsError::new(
            "directory descriptor is not a real directory",
        ));
    }
    if private {
        validate_owner_mode(&metadata, PRIVATE_DIR_MODE, "directory")?;
    }
    Ok(())
}

fn validate_regular_file(file: &File) -> Result<(), SecureFsError> {
    let metadata = file
        .metadata()
        .map_err(|error| SecureFsError::from_io("inspect file descriptor", error))?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(SecureFsError::new(
            "file must be a regular single-link file",
        ));
    }
    validate_owner_mode(&metadata, PRIVATE_FILE_MODE, "file")
}

fn validate_owner_mode(
    metadata: &fs::Metadata,
    expected_mode: u32,
    kind: &str,
) -> Result<(), SecureFsError> {
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.uid() != uid || mode != expected_mode {
        return Err(SecureFsError::new(format!(
            "{kind} must be owned by the current user with mode {expected_mode:04o}"
        )));
    }
    Ok(())
}

fn fchmod(file: &File, mode: u32) -> Result<(), SecureFsError> {
    // SAFETY: fchmod operates on the valid descriptor owned by file.
    let result = unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) };
    if result != 0 {
        return Err(SecureFsError::from_io(
            "set descriptor permissions",
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

fn entry_from_stat(name: OsString, stat: libc::stat) -> SecureEntry {
    let file_type = stat.st_mode & libc::S_IFMT;
    let kind = if file_type == libc::S_IFDIR {
        EntryKind::Directory
    } else if file_type == libc::S_IFREG {
        EntryKind::Regular
    } else if file_type == libc::S_IFLNK {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    };
    SecureEntry {
        name,
        kind,
        uid: stat.st_uid,
        mode: stat.st_mode & 0o777,
        nlink: stat.st_nlink,
    }
}

fn temporary_name(target: &OsStr, tag: &str) -> OsString {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b".");
    bytes.extend_from_slice(target.as_bytes());
    bytes.extend_from_slice(
        format!(
            ".{tag}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        )
        .as_bytes(),
    );
    OsString::from_vec(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn non_private_directory_path_accepts_single_link_directory_fds() {
        let path = Path::new("/proc/sys");
        let metadata = fs::metadata(path).expect("/proc/sys should be available on Linux");
        assert!(metadata.is_dir());
        assert_eq!(metadata.nlink(), 1);

        open_dir_path(path, false, false).expect("single-link directory fd should be accepted");
    }
}
