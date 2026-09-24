use std::{
    fs::{self, File},
    path::{Component, Path, PathBuf},
};

pub(crate) struct AnchoredDirectory {
    path: PathBuf,
    _ancestors: Vec<File>,
}

impl AnchoredDirectory {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn anchor_directory(
    path: &Path,
    create_dirs: bool,
) -> std::io::Result<AnchoredDirectory> {
    let mut current = PathBuf::new();
    let mut ancestors = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => current.push(component),
            Component::Normal(_) => {
                current.push(component);
                if create_dirs {
                    match fs::create_dir(&current) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                }
                ancestors.push(open_directory(&current)?);
            }
            Component::CurDir | Component::ParentDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "directory path must be normalized",
                ));
            }
        }
    }
    if ancestors.is_empty() {
        ancestors.push(open_directory(&current)?);
    }
    Ok(AnchoredDirectory {
        path: current,
        _ancestors: ancestors,
    })
}

pub(crate) fn open_directory(path: &Path) -> std::io::Result<File> {
    open_no_reparse(path, true)
}

pub(crate) fn open_regular(path: &Path) -> std::io::Result<File> {
    open_no_reparse(path, false)
}

pub(crate) fn entry_mode(meta: &fs::Metadata) -> String {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    let attributes = meta.file_attributes();
    let mut bits = if attributes & FILE_ATTRIBUTE_READONLY != 0 {
        0o444
    } else {
        0o666
    };
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
    {
        bits |= 0o111;
    }
    format!("0{bits:o}")
}

fn open_no_reparse(path: &Path, directory: bool) -> std::io::Result<File> {
    use std::os::windows::{ffi::OsStrExt, io::FromRawHandle};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_CANT_ACCESS_FILE, GENERIC_READ, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        },
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | if directory { 0 } else { FILE_SHARE_DELETE },
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let inspected = unsafe { GetFileInformationByHandle(handle, &mut info) };
    if inspected == 0 {
        let error = std::io::Error::last_os_error();
        unsafe { CloseHandle(handle) };
        return Err(error);
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        unsafe { CloseHandle(handle) };
        return Err(std::io::Error::from_raw_os_error(
            ERROR_CANT_ACCESS_FILE as i32,
        ));
    }
    if (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory {
        unsafe { CloseHandle(handle) };
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            if directory {
                "path is not a directory"
            } else {
                "path is not a regular file"
            },
        ));
    }
    // SAFETY: CreateFileW returned an owned handle, transferred to File.
    Ok(unsafe { File::from_raw_handle(handle) })
}

pub(crate) fn delete_tree(path: &Path) -> std::io::Result<usize> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    let meta = fs::symlink_metadata(path)?;
    let attributes = meta.file_attributes();
    let is_directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        if is_directory {
            fs::remove_dir(path)?;
        } else {
            fs::remove_file(path)?;
        }
        return Ok(1);
    }
    if !is_directory {
        fs::remove_file(path)?;
        return Ok(1);
    }
    let mut count = 1usize;
    for child in fs::read_dir(path)? {
        count = count
            .checked_add(delete_tree(&child?.path())?)
            .ok_or_else(|| std::io::Error::other("deleted entry count overflow"))?;
    }
    fs::remove_dir(path)?;
    Ok(count)
}
