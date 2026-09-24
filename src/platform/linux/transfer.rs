use super::filesystem::{
    anchor_directory, open_named_temp, open_regular_at, path_component, validate_final_target,
    TempEntry,
};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Write},
    path::Path,
};
#[cfg(test)]
use std::{os::fd::AsRawFd, path::PathBuf};

pub(crate) struct TransferTemp {
    file: File,
    entry: TempEntry,
    #[cfg(test)]
    path: PathBuf,
}

impl TransferTemp {
    pub(crate) fn as_file(&self) -> &File {
        &self.file
    }

    pub(crate) fn reopen(&self) -> io::Result<File> {
        self.file.try_clone()
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Write for TransferTemp {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

pub(crate) struct TransferTarget {
    parent: File,
    name: OsString,
}

impl TransferTarget {
    pub(crate) fn new(path: &Path, create_dirs: bool) -> io::Result<Self> {
        let parent = anchor_directory(
            path.parent()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?,
            create_dirs,
        )?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?
            .to_os_string();
        Ok(Self { parent, name })
    }

    #[cfg(test)]
    fn parent_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.parent.as_raw_fd()))
    }

    pub(crate) fn open_read(&self) -> io::Result<File> {
        open_regular_at(&self.parent, Path::new(&self.name))
    }

    pub(crate) fn temporary(&self) -> io::Result<TransferTemp> {
        let (file, entry) = open_named_temp(&self.parent, 0o600, ".mcp-xfer-")?;
        Ok(TransferTemp {
            file,
            #[cfg(test)]
            path: self.parent_path().join(entry.name.to_str().unwrap()),
            entry,
        })
    }

    pub(crate) fn commit(&self, mut temporary: TransferTemp) -> io::Result<()> {
        validate_final_target(&self.parent, Path::new(&self.name))?;
        temporary
            .entry
            .rename_to(&path_component(Path::new(&self.name))?)?;
        self.parent.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};

    #[test]
    fn parent_replacement_does_not_redirect_upload() {
        let root = tempfile::tempdir().unwrap();
        let visible = root.path().join("visible");
        let replacement = root.path().join("replacement");
        fs::create_dir(&visible).unwrap();
        fs::create_dir(&replacement).unwrap();
        let target = TransferTarget::new(&visible.join("data"), false).unwrap();
        let mut temporary = target.temporary().unwrap();
        temporary.write_all(b"original parent").unwrap();
        fs::rename(&visible, root.path().join("moved")).unwrap();
        fs::rename(&replacement, &visible).unwrap();
        target.commit(temporary).unwrap();
        assert_eq!(
            fs::read(root.path().join("moved/data")).unwrap(),
            b"original parent"
        );
        assert!(!visible.join("data").exists());
    }

    #[test]
    fn create_dirs_rejects_symlink_ancestor() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let link = root.path().join("link");
        symlink(&outside, &link).unwrap();
        assert!(TransferTarget::new(&link.join("nested/data"), true).is_err());
        assert!(!outside.join("nested").exists());
    }

    #[test]
    fn upload_commit_refuses_existing_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        let link = root.path().join("link");
        fs::write(&outside, b"unchanged").unwrap();
        symlink(&outside, &link).unwrap();
        let target = TransferTarget::new(&link, false).unwrap();
        let mut temporary = target.temporary().unwrap();
        temporary.write_all(b"replacement").unwrap();
        assert!(target.commit(temporary).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"unchanged");
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    }

    #[test]
    fn replaced_temporary_name_is_not_committed_or_cleaned() {
        let root = tempfile::tempdir().unwrap();
        let target_path = root.path().join("target");
        fs::write(&target_path, b"old").unwrap();
        let target = TransferTarget::new(&target_path, false).unwrap();
        let mut temporary = target.temporary().unwrap();
        temporary.write_all(b"uploaded").unwrap();
        let temporary_path = temporary.path().to_owned();
        let moved = root.path().join("moved-temp");
        fs::rename(&temporary_path, &moved).unwrap();
        fs::write(&temporary_path, b"replacement").unwrap();

        assert!(target.commit(temporary).is_err());
        assert_eq!(fs::read(&target_path).unwrap(), b"old");
        assert_eq!(fs::read(&temporary_path).unwrap(), b"replacement");
        assert_eq!(fs::read(&moved).unwrap(), b"uploaded");
    }
}
