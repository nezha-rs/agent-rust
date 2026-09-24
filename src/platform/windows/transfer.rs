use super::filesystem::{anchor_directory, open_regular, AnchoredDirectory};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

pub(crate) struct TransferTarget {
    parent: AnchoredDirectory,
    path: PathBuf,
}

impl TransferTarget {
    pub(crate) fn new(path: &Path, create_dirs: bool) -> io::Result<Self> {
        let parent = anchor_directory(
            path.parent()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?,
            create_dirs,
        )?;
        Ok(Self {
            parent,
            path: path.to_path_buf(),
        })
    }

    pub(crate) fn open_read(&self) -> io::Result<File> {
        open_regular(&self.path)
    }

    pub(crate) fn temporary(&self) -> io::Result<tempfile::NamedTempFile> {
        tempfile::Builder::new()
            .prefix(".mcp-xfer-")
            .tempfile_in(self.parent.path())
    }

    pub(crate) fn commit(&self, temporary: tempfile::NamedTempFile) -> io::Result<()> {
        temporary.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};

    #[test]
    fn held_parent_prevents_upload_redirection() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let target = TransferTarget::new(&parent.join("data"), false).unwrap();
        let mut temporary = target.temporary().unwrap();
        temporary.write_all(b"anchored").unwrap();
        assert!(fs::rename(&parent, root.path().join("moved")).is_err());
        target.commit(temporary).unwrap();
        assert_eq!(fs::read(parent.join("data")).unwrap(), b"anchored");
    }
}
