use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};

pub(crate) fn anchor_directory(path: &Path, create_dirs: bool) -> std::io::Result<File> {
    let mut parent = open_directory(Path::new("/"))?;
    for component in path.components() {
        let name = match component {
            Component::Normal(name) => name,
            Component::RootDir | Component::CurDir => continue,
            Component::ParentDir | Component::Prefix(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "directory path must be absolute and normalized",
                ));
            }
        };
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        parent = match open_child_directory(&parent, &name) {
            Ok(child) => child,
            Err(error) if create_dirs && error.kind() == std::io::ErrorKind::NotFound => {
                if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                }
                open_child_directory(&parent, &name)?
            }
            Err(error) => return Err(error),
        };
    }
    Ok(parent)
}

fn open_child_directory(parent: &File, name: &CString) -> std::io::Result<File> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

pub(crate) fn open_directory(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a directory",
        ));
    }
    Ok(file)
}

pub(crate) fn entry_mode(meta: &fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bits = meta.permissions().mode() & 0o777;
    if bits == 0 {
        "0".into()
    } else {
        format!("0{bits:o}")
    }
}

pub(crate) fn path_component(path: &Path) -> std::io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(
        path.file_name()
            .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))?
            .as_bytes(),
    )
    .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
}

pub(crate) fn open_regular_at(parent: &File, path: &Path) -> std::io::Result<File> {
    open_regular_at_with(parent, path, || {})
}

pub(crate) fn validate_final_target(parent: &File, path: &Path) -> std::io::Result<()> {
    let name = path_component(path)?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(error);
    }
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    Ok(())
}

pub(crate) fn final_target_is_directory(
    parent: &File,
    path: &Path,
) -> std::io::Result<Option<bool>> {
    let name = path_component(path)?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error);
    }
    let metadata = unsafe { metadata.assume_init() };
    Ok(Some(metadata.st_mode & libc::S_IFMT == libc::S_IFDIR))
}

fn open_regular_at_with(
    parent: &File,
    path: &Path,
    mut before_open: impl FnMut(),
) -> std::io::Result<File> {
    let name = path_component(path)?;
    for _ in 0..4 {
        let mut before = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                before.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let before = unsafe { before.assume_init() };
        if before.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is not a regular file",
            ));
        }
        before_open();
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                continue;
            }
            return Err(error);
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let after = file.metadata()?;
        if !after.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is not a regular file",
            ));
        }
        if before.st_dev == after.dev() && before.st_ino == after.ino() {
            return Ok(file);
        }
    }
    Err(std::io::Error::other("final target changed during open"))
}

pub(crate) struct TempEntry {
    parent: File,
    pub(crate) name: CString,
    device: u64,
    inode: u64,
    committed: bool,
}

impl TempEntry {
    fn owns_name(&self) -> std::io::Result<bool> {
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(error)
            };
        }
        let metadata = unsafe { metadata.assume_init() };
        Ok(metadata.st_dev == self.device
            && metadata.st_ino == self.inode
            && metadata.st_mode & libc::S_IFMT == libc::S_IFREG)
    }

    pub(crate) fn rename_to(&mut self, target: &CString) -> std::io::Result<()> {
        if !self.owns_name()? {
            return Err(std::io::Error::other(
                "temporary file changed before commit",
            ));
        }
        rename_at(self.parent.as_raw_fd(), &self.name, target)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for TempEntry {
    fn drop(&mut self) {
        if !self.committed && self.owns_name().unwrap_or(false) {
            unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0) };
        }
    }
}

pub(crate) fn open_write_temp(parent: &File, mode: u32) -> std::io::Result<(File, TempEntry)> {
    open_named_temp(parent, mode, ".mcp-write-")
}

pub(crate) fn open_named_temp(
    parent: &File,
    mode: u32,
    prefix: &str,
) -> std::io::Result<(File, TempEntry)> {
    let owned_parent = parent.try_clone()?;
    for _ in 0..64 {
        let name = CString::new(format!("{prefix}{}", uuid::Uuid::new_v4()))
            .expect("generated temporary name has no NUL");
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::mode_t,
            )
        };
        if fd >= 0 {
            let file = unsafe { File::from_raw_fd(fd) };
            let metadata = file.metadata()?;
            return Ok((
                file,
                TempEntry {
                    parent: owned_parent,
                    name,
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    committed: false,
                },
            ));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate temporary write file",
    ))
}

pub(crate) fn rename_at(parent: RawFd, old: &CString, new: &CString) -> std::io::Result<()> {
    if unsafe { libc::renameat(parent, old.as_ptr(), parent, new.as_ptr()) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn unlink_at(parent: RawFd, name: &CString, flags: libc::c_int) -> std::io::Result<()> {
    if unsafe { libc::unlinkat(parent, name.as_ptr(), flags) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn delete_tree_at(parent: &File, name: &CString) -> std::io::Result<usize> {
    let child_fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )
    };
    if child_fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(0);
        }
        if error.kind() != std::io::ErrorKind::NotADirectory {
            return Err(error);
        }
        if let Err(error) = unlink_at(parent.as_raw_fd(), name, 0) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(0);
            }
            return Err(error);
        }
        return Ok(1);
    }
    let child = unsafe { File::from_raw_fd(child_fd) };
    let entries = fs::read_dir(format!("/proc/self/fd/{}", child.as_raw_fd()))?;
    let mut count = 1usize;
    for entry in entries {
        let child_name = path_component(&entry?.path())?;
        count = count
            .checked_add(delete_tree_at(&child, &child_name)?)
            .ok_or_else(|| std::io::Error::other("deleted entry count overflow"))?;
    }
    drop(child);
    if let Err(error) = unlink_at(parent.as_raw_fd(), name, libc::AT_REMOVEDIR) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(count - 1);
        }
        return Err(error);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::{ffi::OsStrExt, fs::symlink},
    };

    #[test]
    fn final_open_rejects_symlink_fifo_and_directory() {
        let root = tempfile::tempdir().unwrap();
        let parent = anchor_directory(root.path(), false).unwrap();
        let target = root.path().join("regular");
        let link = root.path().join("link");
        let fifo = root.path().join("fifo");
        let directory = root.path().join("directory");
        File::create(&target)
            .unwrap()
            .write_all(b"original")
            .unwrap();
        symlink(&target, &link).unwrap();
        fs::create_dir(&directory).unwrap();
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);

        assert!(open_regular_at(&parent, &target).is_ok());
        for path in [&link, &fifo, &directory] {
            assert!(open_regular_at(&parent, path).is_err());
        }
    }

    #[test]
    fn final_open_retries_when_entry_changes_between_stat_and_open() {
        let root = tempfile::tempdir().unwrap();
        let parent = anchor_directory(root.path(), false).unwrap();
        let target = root.path().join("target");
        let replacement = root.path().join("replacement");
        fs::write(&target, b"first").unwrap();
        fs::write(&replacement, b"second").unwrap();
        let mut swapped = false;
        let mut opened = open_regular_at_with(&parent, &target, || {
            if !swapped {
                fs::rename(&target, root.path().join("old")).unwrap();
                fs::rename(&replacement, &target).unwrap();
                swapped = true;
            }
        })
        .unwrap();
        let mut content = String::new();
        opened.read_to_string(&mut content).unwrap();
        assert_eq!(content, "second");
    }

    #[test]
    fn recursive_delete_tolerates_disappearing_entry() {
        let root = tempfile::tempdir().unwrap();
        let parent = anchor_directory(root.path(), false).unwrap();
        let missing = CString::new("missing").unwrap();
        assert_eq!(delete_tree_at(&parent, &missing).unwrap(), 0);
    }

    #[test]
    fn temporary_replacement_is_neither_committed_nor_cleaned_up() {
        let root = tempfile::tempdir().unwrap();
        let parent = anchor_directory(root.path(), false).unwrap();
        let (mut file, mut entry) = open_write_temp(&parent, 0o600).unwrap();
        file.write_all(b"original").unwrap();
        let temporary_path = root.path().join(entry.name.to_str().unwrap());
        let moved = root.path().join("moved");
        fs::rename(&temporary_path, &moved).unwrap();
        fs::write(&temporary_path, b"replacement").unwrap();
        let target = CString::new("target").unwrap();
        assert!(entry.rename_to(&target).is_err());
        drop(entry);
        assert_eq!(fs::read(&temporary_path).unwrap(), b"replacement");
        assert_eq!(fs::read(&moved).unwrap(), b"original");
        assert!(!root.path().join("target").exists());
    }
}
