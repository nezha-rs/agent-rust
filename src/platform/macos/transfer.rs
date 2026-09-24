use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

pub(crate) struct TransferTarget {
    path: PathBuf,
}

impl TransferTarget {
    pub(crate) fn new(path: &Path, create_dirs: bool) -> io::Result<Self> {
        if create_dirs {
            fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?,
            )?;
        }
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    pub(crate) fn open_read(&self) -> io::Result<File> {
        crate::mcp_fs::open_regular(&self.path)
    }

    pub(crate) fn temporary(&self) -> io::Result<tempfile::NamedTempFile> {
        tempfile::Builder::new().prefix(".mcp-xfer-").tempfile_in(
            self.path
                .parent()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?,
        )
    }

    pub(crate) fn commit(&self, temporary: tempfile::NamedTempFile) -> io::Result<()> {
        temporary.persist(&self.path).map_err(|error| error.error)?;
        super::sync_transfer_parent(&self.path)
    }
}
