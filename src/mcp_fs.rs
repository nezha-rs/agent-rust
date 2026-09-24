use crate::{
    config::AgentConfig,
    proto::{Task, TaskResult},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::UNIX_EPOCH,
};

#[cfg(target_os = "macos")]
use std::fs::OpenOptions;

#[cfg(target_os = "linux")]
use crate::platform::final_target_is_directory;
#[cfg(target_os = "linux")]
use crate::platform::{
    anchor_directory, delete_tree_at, open_regular_at, open_write_temp, path_component, unlink_at,
    validate_final_target,
};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(target_os = "macos")]
use std::{
    ffi::CString,
    os::fd::{FromRawFd, RawFd},
};

const MAX_LIST: usize = 5000;
const MAX_READ: usize = 1024 * 1024;
const MAX_WRITE: usize = 8 * 1024 * 1024;
const LOCK_COUNT: usize = 1024;

#[derive(Default, Deserialize)]
#[serde(default)]
struct ListRequest {
    path: String,
    show_hidden: bool,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct ReadRequest {
    path: String,
    offset: i64,
    length: i64,
    encoding: String,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct WriteRequest {
    path: String,
    content: String,
    encoding: String,
    mode: String,
    if_match_sha256: String,
    create_dirs: bool,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct DeleteRequest {
    path: String,
    recursive: bool,
}

#[derive(Serialize)]
struct Entry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    size: u64,
    mode: String,
    mtime: i64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    is_symlink: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    link_target: String,
}
#[derive(Default, Serialize)]
struct ListResult {
    entries: Vec<Entry>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    #[serde(skip_serializing_if = "is_zero_usize")]
    total: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}
#[derive(Default, Serialize)]
struct ReadResult {
    content: String,
    encoding: String,
    size: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    sha256: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}
#[derive(Default, Serialize)]
struct WriteResult {
    size: usize,
    sha256: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}
#[derive(Default, Serialize)]
struct DeleteResult {
    deleted_count: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}
fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

fn reply<T: Serialize>(task: Task, result: T) -> TaskResult {
    TaskResult {
        id: task.id,
        r#type: task.r#type,
        data: serde_json::to_string(&result)
            .unwrap_or_else(|_| "{\"error\":\"marshal failed\"}".into()),
        successful: true,
        ..Default::default()
    }
}

pub(crate) fn resolve(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("path required".into());
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("path must be absolute".into());
    }
    #[cfg(windows)]
    if path
        .components()
        .any(|component| matches!(component, Component::Normal(name) if name.to_string_lossy().contains(':')))
    {
        return Err("alternate data stream paths are not supported".into());
    }
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                clean.push(component)
            }
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
        }
    }
    Ok(clean)
}

pub(crate) fn is_root(path: &Path) -> bool {
    path.parent().is_none_or(|parent| parent == path)
}
fn hash(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn fs_io_error(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "file or directory does not exist".into(),
        std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        _ if error.to_string().contains("is not a regular file") => {
            "path is not a regular file".into()
        }
        _ => "internal agent error".into(),
    }
}

fn fs_path_error(error: &str) -> String {
    match error {
        "path required" | "path must be absolute" => error.into(),
        _ if error.contains("alternate data stream") => "path is not supported".into(),
        _ => "internal agent error".into(),
    }
}

fn hash_file(mut file: File) -> std::io::Result<String> {
    let mut digest = Sha256::new();
    let mut chunk = [0u8; 8192];
    loop {
        match file.read(&mut chunk)? {
            0 => break,
            count => digest.update(&chunk[..count]),
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(crate) fn locks() -> &'static [Mutex<()>] {
    static LOCKS: OnceLock<Vec<Mutex<()>>> = OnceLock::new();
    LOCKS.get_or_init(|| (0..LOCK_COUNT).map(|_| Mutex::new(())).collect())
}
pub(crate) fn stripe(path: &Path) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    #[cfg(any(windows, target_os = "macos"))]
    path.to_string_lossy().to_lowercase().hash(&mut hasher);
    #[cfg(not(any(windows, target_os = "macos")))]
    path.hash(&mut hasher);
    (hasher.finish() as usize) % LOCK_COUNT
}

fn entry(path: &Path) -> std::io::Result<Entry> {
    let meta = fs::symlink_metadata(path)?;
    let kind = if meta.is_symlink() {
        "symlink"
    } else if meta.is_dir() {
        "dir"
    } else if meta.is_file() {
        "file"
    } else {
        "special"
    };
    let mode = crate::platform::entry_mode(&meta);
    Ok(Entry {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        kind: kind.into(),
        size: meta.len(),
        mode,
        mtime: meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs() as i64),
        is_symlink: meta.is_symlink(),
        link_target: if meta.is_symlink() {
            fs::read_link(path)
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        },
    })
}

#[cfg(target_os = "macos")]
fn open_directory(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a directory",
        ));
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn open_directory(path: &Path) -> std::io::Result<File> {
    anchor_directory(path, false)
}

#[cfg(windows)]
fn open_directory(path: &Path) -> std::io::Result<File> {
    crate::platform::open_directory(path)
}

fn list(request: ListRequest) -> ListResult {
    let mut result = ListResult::default();
    let path = match resolve(&request.path) {
        Ok(path) => path,
        Err(error) => {
            result.error = error;
            return result;
        }
    };
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            result.error = "path is not a directory".into();
            return result;
        }
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    }
    #[cfg(windows)]
    let anchor = match crate::platform::anchor_directory(&path, false) {
        Ok(anchor) => anchor,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    #[cfg(any(unix, windows))]
    let directory = match open_directory(&path) {
        Ok(file) => file,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    #[cfg(target_os = "linux")]
    let read_path = {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
    };
    #[cfg(not(target_os = "linux"))]
    let read_path = path;
    let dir = match fs::read_dir(read_path) {
        Ok(dir) => dir,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    for item in dir {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                result.error = fs_io_error(&error);
                return result;
            }
        };
        if !request.show_hidden && item.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        result.total += 1;
        if result.entries.len() >= MAX_LIST {
            result.truncated = true;
            continue;
        }
        if let Ok(info) = entry(&item.path()) {
            result.entries.push(info);
        }
    }
    #[cfg(any(unix, windows))]
    drop(directory);
    #[cfg(windows)]
    drop(anchor);
    result
}

#[cfg(windows)]
pub(crate) fn open_regular(path: &Path) -> std::io::Result<File> {
    crate::platform::open_regular(path)
}

#[cfg(target_os = "macos")]
pub(crate) fn open_regular(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(target_os = "macos")]
fn path_component(path: &Path) -> std::io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(
        path.file_name()
            .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))?
            .as_bytes(),
    )
    .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
}

#[cfg(target_os = "macos")]
fn open_regular_at(parent: &File, path: &Path) -> std::io::Result<File> {
    let name = path_component(path)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openat returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(target_os = "macos")]
struct TempEntry<'a> {
    parent: &'a File,
    name: CString,
    committed: bool,
}

#[cfg(target_os = "macos")]
impl Drop for TempEntry<'_> {
    fn drop(&mut self) {
        if !self.committed {
            unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0) };
        }
    }
}

#[cfg(target_os = "macos")]
fn open_write_temp(parent: &File, mode: u32) -> std::io::Result<(File, TempEntry<'_>)> {
    for _ in 0..64 {
        let name = CString::new(format!(".mcp-write-{}", uuid::Uuid::new_v4()))
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
            // SAFETY: openat returned a new owned descriptor.
            return Ok((
                unsafe { File::from_raw_fd(fd) },
                TempEntry {
                    parent,
                    name,
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

#[cfg(target_os = "macos")]
fn rename_at(parent: RawFd, old: &CString, new: &CString) -> std::io::Result<()> {
    if unsafe { libc::renameat(parent, old.as_ptr(), parent, new.as_ptr()) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn read(request: ReadRequest) -> ReadResult {
    let mut result = ReadResult::default();
    let path = match resolve(&request.path) {
        Ok(path) => path,
        Err(error) => {
            result.error = fs_path_error(&error);
            return result;
        }
    };
    if request.offset < 0 {
        result.error = "offset must be >= 0".into();
        return result;
    }
    #[cfg(windows)]
    let _anchor = match crate::platform::anchor_directory(
        path.parent().expect("absolute path has parent"),
        false,
    ) {
        Ok(anchor) => anchor,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    #[cfg(unix)]
    let parent = match open_directory(path.parent().expect("absolute path has parent")) {
        Ok(parent) => parent,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    #[cfg(unix)]
    let mut file = match open_regular_at(&parent, &path) {
        Ok(file) => file,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    #[cfg(not(unix))]
    let mut file = match open_regular(&path) {
        Ok(file) => file,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    let total = match file.metadata() {
        Ok(meta) => meta.len(),
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    if let Err(error) = file.seek(SeekFrom::Start(request.offset as u64)) {
        result.error = fs_io_error(&error);
        return result;
    }
    let length = if request.length <= 0 {
        MAX_READ
    } else {
        (request.length as usize).min(MAX_READ)
    };
    let mut data = Vec::with_capacity(length);
    let count = match file.take(length as u64).read_to_end(&mut data) {
        Ok(count) => count,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    result.encoding = if request.encoding.is_empty() {
        "utf8".into()
    } else {
        request.encoding
    };
    result.content = match result.encoding.as_str() {
        "utf8" => String::from_utf8_lossy(&data).to_string(),
        "base64" => STANDARD.encode(&data),
        other => {
            result.error = format!("unknown encoding: {other}");
            return result;
        }
    };
    result.size = count;
    result.sha256 = hash(&data);
    result.truncated = (request.offset as u64).saturating_add(count as u64) < total;
    result
}

fn write(request: WriteRequest) -> WriteResult {
    let mut result = WriteResult::default();
    let path = match resolve(&request.path) {
        Ok(path) => path,
        Err(error) => {
            result.error = fs_path_error(&error);
            return result;
        }
    };
    if is_root(&path) {
        result.error = "path targets a filesystem root".into();
        return result;
    }
    let _guard = locks()[stripe(&path)]
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let encoding = if request.encoding.is_empty() {
        "utf8"
    } else {
        request.encoding.as_str()
    };
    let data = match encoding {
        "utf8" => request.content.into_bytes(),
        "base64" => match STANDARD.decode(request.content) {
            Ok(data) => data,
            Err(error) => {
                result.error = format!("invalid base64: {error}");
                return result;
            }
        },
        other => {
            result.error = format!("unknown encoding: {other}");
            return result;
        }
    };
    if data.len() > MAX_WRITE {
        result.error = "content exceeds max write size".into();
        return result;
    }
    #[cfg(target_os = "macos")]
    if request.create_dirs {
        let parent_path = path.parent().expect("absolute non-root path has parent");
        if let Err(error) = fs::create_dir_all(parent_path) {
            result.error = fs_io_error(&error);
            return result;
        }
    }
    let parent_path = path.parent().expect("absolute non-root path has parent");
    #[cfg(windows)]
    let anchor = match crate::platform::anchor_directory(parent_path, request.create_dirs) {
        Ok(anchor) => anchor,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    #[cfg(windows)]
    let parent_path = anchor.path();
    #[cfg(target_os = "linux")]
    let parent_open = anchor_directory(parent_path, request.create_dirs);
    #[cfg(target_os = "macos")]
    let parent_open = open_directory(parent_path);
    #[cfg(unix)]
    let parent = match parent_open {
        Ok(parent) => parent,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    if !request.if_match_sha256.is_empty() {
        #[cfg(unix)]
        let current = open_regular_at(&parent, &path);
        #[cfg(not(unix))]
        let current = open_regular(&path);
        let current = match current {
            Ok(file) => file,
            Err(error) => {
                result.error = if error.kind() == std::io::ErrorKind::NotFound {
                    "if_match precondition failed: file does not exist".into()
                } else {
                    "if_match precondition failed: target is not a regular file".into()
                };
                return result;
            }
        };
        let current_hash = match hash_file(current) {
            Ok(hash) => hash,
            Err(error) => {
                result.error = fs_io_error(&error);
                return result;
            }
        };
        if current_hash != request.if_match_sha256 {
            result.error = "if_match precondition failed: sha256 mismatch".into();
            return result;
        }
    }
    let mode = if request.mode.is_empty() {
        0o644
    } else {
        match u32::from_str_radix(&request.mode, 8) {
            Ok(mode) => mode & 0o777,
            Err(error) => {
                result.error = format!("invalid mode: {error}");
                return result;
            }
        }
    };
    #[cfg(not(unix))]
    let _ = mode;
    #[cfg(unix)]
    let (mut temporary, mut temporary_entry) = match open_write_temp(&parent, mode) {
        Ok(file) => file,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    #[cfg(not(unix))]
    let mut temporary = match tempfile::Builder::new()
        .prefix(".mcp-write-")
        .tempfile_in(parent_path)
    {
        Ok(file) => file,
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    if let Err(error) = temporary.write_all(&data) {
        result.error = fs_io_error(&error);
        return result;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = temporary.set_permissions(fs::Permissions::from_mode(mode)) {
            result.error = fs_io_error(&error);
            return result;
        }
    }
    #[cfg(unix)]
    let sync_result = temporary.sync_all();
    #[cfg(not(unix))]
    let sync_result = temporary.as_file().sync_all();
    if let Err(error) = sync_result {
        result.error = fs_io_error(&error);
        return result;
    }
    #[cfg(not(unix))]
    if !request.if_match_sha256.is_empty() {
        match open_regular(&path).and_then(hash_file) {
            Ok(hash) if hash == request.if_match_sha256 => {}
            Ok(_) => {
                result.error = "if_match precondition failed: sha256 mismatch".into();
                return result;
            }
            Err(error) => {
                result.error = if error.kind() == std::io::ErrorKind::NotFound {
                    "if_match precondition failed: file does not exist".into()
                } else {
                    "if_match precondition failed: target is not a regular file".into()
                };
                return result;
            }
        }
    }
    #[cfg(unix)]
    {
        let target_name = match path_component(&path) {
            Ok(name) => name,
            Err(error) => {
                result.error = fs_io_error(&error);
                return result;
            }
        };
        if !request.if_match_sha256.is_empty() {
            let current_hash = open_regular_at(&parent, &path).and_then(hash_file);
            match current_hash {
                Ok(hash) if hash == request.if_match_sha256 => {}
                Ok(_) => {
                    result.error = "if_match precondition failed: sha256 mismatch".into();
                    return result;
                }
                Err(error) => {
                    result.error = if error.kind() == std::io::ErrorKind::NotFound {
                        "if_match precondition failed: file does not exist".into()
                    } else {
                        "if_match precondition failed: target is not a regular file".into()
                    };
                    return result;
                }
            }
        }
        #[cfg(target_os = "linux")]
        if let Err(error) = validate_final_target(&parent, &path) {
            result.error = fs_io_error(&error);
            return result;
        }
        #[cfg(target_os = "linux")]
        let rename_result = temporary_entry.rename_to(&target_name);
        #[cfg(target_os = "macos")]
        let rename_result = rename_at(parent.as_raw_fd(), &temporary_entry.name, &target_name);
        if let Err(error) = rename_result {
            result.error = fs_io_error(&error);
            return result;
        }
        #[cfg(target_os = "macos")]
        {
            temporary_entry.committed = true;
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = temporary.persist(&path) {
        result.error = fs_io_error(&error.error);
        return result;
    }
    #[cfg(unix)]
    if let Err(error) = parent.sync_all() {
        result.error = fs_io_error(&error);
        return result;
    }
    #[cfg(not(unix))]
    if let Ok(directory) = File::open(parent_path) {
        let _ = directory.sync_all();
    }
    result.size = data.len();
    result.sha256 = hash(&data);
    result
}

#[cfg(all(not(unix), not(windows)))]
fn count_tree(path: &Path) -> std::io::Result<usize> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Ok(1);
    }
    let mut count = 1;
    for child in fs::read_dir(path)? {
        count += count_tree(&child?.path())?;
    }
    Ok(count)
}

#[cfg(target_os = "macos")]
fn unlink_at(parent: RawFd, name: &CString, flags: libc::c_int) -> std::io::Result<()> {
    if unsafe { libc::unlinkat(parent, name.as_ptr(), flags) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn delete_tree_at(parent: &File, name: &CString) -> std::io::Result<usize> {
    use std::os::fd::AsRawFd;

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
        if error.kind() != std::io::ErrorKind::NotADirectory {
            return Err(error);
        }
        unlink_at(parent.as_raw_fd(), name, 0)?;
        return Ok(1);
    }
    // SAFETY: openat returned a new owned descriptor.
    let child = unsafe { File::from_raw_fd(child_fd) };
    let entries = fs::read_dir(format!("/proc/self/fd/{}", child.as_raw_fd()))?;
    let mut count = 1usize;
    for entry in entries {
        let entry = entry?;
        let child_name = path_component(&entry.path())?;
        count = count
            .checked_add(delete_tree_at(&child, &child_name)?)
            .ok_or_else(|| std::io::Error::other("deleted entry count overflow"))?;
    }
    drop(child);
    unlink_at(parent.as_raw_fd(), name, libc::AT_REMOVEDIR)?;
    Ok(count)
}

fn delete(request: DeleteRequest) -> DeleteResult {
    let mut result = DeleteResult::default();
    let path = match resolve(&request.path) {
        Ok(path) => path,
        Err(error) => {
            result.error = fs_path_error(&error);
            return result;
        }
    };
    if is_root(&path) {
        result.error = "refusing to delete root".into();
        return result;
    }
    let _guard = locks()[stripe(&path)]
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    #[cfg(windows)]
    let _anchor = match crate::platform::anchor_directory(
        path.parent().expect("absolute non-root path has parent"),
        false,
    ) {
        Ok(anchor) => anchor,
        Err(error) if request.recursive && error.kind() == std::io::ErrorKind::NotFound => {
            return result;
        }
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    #[cfg(not(unix))]
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if request.recursive && error.kind() == std::io::ErrorKind::NotFound => {
            return result;
        }
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    #[cfg(target_os = "macos")]
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if request.recursive && error.kind() == std::io::ErrorKind::NotFound => {
            return result;
        }
        Err(error) => {
            result.error = fs_io_error(&error);
            return result;
        }
    };
    #[cfg(unix)]
    let removed: std::io::Result<usize> = (|| {
        let parent_path = path.parent().expect("absolute non-root path has parent");
        #[cfg(target_os = "linux")]
        let parent = match anchor_directory(parent_path, false) {
            Ok(parent) => parent,
            Err(error) if request.recursive && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(0);
            }
            Err(error) => return Err(error),
        };
        #[cfg(target_os = "macos")]
        let parent = open_directory(parent_path)?;
        let name = path_component(&path)?;
        #[cfg(target_os = "linux")]
        let is_directory = match final_target_is_directory(&parent, &path)? {
            Some(is_directory) => is_directory,
            None if request.recursive => return Ok(0),
            None => return Err(std::io::ErrorKind::NotFound.into()),
        };
        #[cfg(target_os = "macos")]
        let is_directory = meta.is_dir();
        if request.recursive {
            let count = delete_tree_at(&parent, &name)?;
            Ok(count)
        } else {
            unlink_at(
                parent.as_raw_fd(),
                &name,
                if is_directory { libc::AT_REMOVEDIR } else { 0 },
            )?;
            Ok(1)
        }
    })();
    #[cfg(not(unix))]
    let removed = {
        let count = if request.recursive {
            #[cfg(windows)]
            {
                0
            }
            #[cfg(not(windows))]
            {
                count_tree(&path).unwrap_or(0)
            }
        } else {
            1
        };
        if request.recursive {
            #[cfg(windows)]
            {
                crate::platform::delete_tree_windows(&path)
            }
            #[cfg(not(windows))]
            {
                if meta.is_dir() {
                    fs::remove_dir_all(&path).map(|()| count)
                } else {
                    fs::remove_file(&path).map(|()| count)
                }
            }
        } else if meta.is_dir() {
            fs::remove_dir(&path).map(|()| count)
        } else {
            fs::remove_file(&path).map(|()| count)
        }
    };
    match removed {
        Ok(count) => result.deleted_count = count,
        Err(error) => {
            result.error = fs_io_error(&error);
        }
    }
    result
}

pub async fn run(task: Task, config: &AgentConfig) -> TaskResult {
    if config.disable_command_execute {
        let error = "agent disabled file operations".to_string();
        return match task.r#type {
            16 => reply(
                task,
                ListResult {
                    error,
                    ..Default::default()
                },
            ),
            17 => reply(
                task,
                ReadResult {
                    error,
                    ..Default::default()
                },
            ),
            18 => reply(
                task,
                WriteResult {
                    error,
                    ..Default::default()
                },
            ),
            _ => reply(
                task,
                DeleteResult {
                    error,
                    ..Default::default()
                },
            ),
        };
    }
    let kind = task.r#type;
    let payload = task.data.clone();
    tokio::task::spawn_blocking(move || match kind {
        16 => match serde_json::from_str::<ListRequest>(&payload) {
            Ok(req) => reply(task, list(req)),
            Err(error) => invalid(task, error),
        },
        17 => match serde_json::from_str::<ReadRequest>(&payload) {
            Ok(req) => reply(task, read(req)),
            Err(error) => invalid(task, error),
        },
        18 => match serde_json::from_str::<WriteRequest>(&payload) {
            Ok(req) => reply(task, write(req)),
            Err(error) => invalid(task, error),
        },
        _ => match serde_json::from_str::<DeleteRequest>(&payload) {
            Ok(req) => reply(task, delete(req)),
            Err(error) => invalid(task, error),
        },
    })
    .await
    .unwrap_or_else(|error| TaskResult {
        r#type: kind,
        data: error.to_string(),
        ..Default::default()
    })
}

fn invalid(task: Task, error: serde_json::Error) -> TaskResult {
    TaskResult {
        id: task.id,
        r#type: task.r#type,
        data: format!("invalid request: {error}"),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_dot_segments_before_root_guard() {
        let root = std::env::current_dir()
            .unwrap()
            .ancestors()
            .last()
            .unwrap()
            .to_path_buf();
        let dotted = root.join("tmp").join("..").to_string_lossy().to_string();
        assert_eq!(resolve(&dotted).unwrap(), root);
        assert_eq!(
            delete(DeleteRequest {
                path: dotted.clone(),
                recursive: true
            })
            .error,
            "refusing to delete root"
        );
        assert_eq!(
            write(WriteRequest {
                path: dotted,
                ..Default::default()
            })
            .error,
            "path targets a filesystem root"
        );
    }

    #[test]
    fn filesystem_errors_use_upstream_public_messages() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.txt");
        let path = missing.to_string_lossy().into_owned();
        assert_eq!(
            read(ReadRequest {
                path: path.clone(),
                ..Default::default()
            })
            .error,
            "file or directory does not exist"
        );
        assert_eq!(
            delete(DeleteRequest {
                path: path.clone(),
                recursive: false,
            })
            .error,
            "file or directory does not exist"
        );
        let absent_tree = delete(DeleteRequest {
            path,
            recursive: true,
        });
        assert_eq!(absent_tree.error, "");
        assert_eq!(absent_tree.deleted_count, 0);

        let missing_descendant = dir.path().join("missing/parent/child");
        let absent_descendant = delete(DeleteRequest {
            path: missing_descendant.to_string_lossy().into_owned(),
            recursive: true,
        });
        assert_eq!(absent_descendant.error, "");
        assert_eq!(absent_descendant.deleted_count, 0);
        assert_eq!(
            delete(DeleteRequest {
                path: missing_descendant.to_string_lossy().into_owned(),
                recursive: false,
            })
            .error,
            "file or directory does not exist"
        );

        let empty = dir.path().join("empty");
        fs::create_dir(&empty).unwrap();
        let removed_empty = delete(DeleteRequest {
            path: empty.to_string_lossy().into_owned(),
            recursive: false,
        });
        assert_eq!(removed_empty.error, "");
        assert_eq!(removed_empty.deleted_count, 1);
        assert!(!empty.exists());

        let nested = dir.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("file.txt"), b"content").unwrap();
        assert_eq!(
            delete(DeleteRequest {
                path: nested.to_string_lossy().into_owned(),
                recursive: false,
            })
            .error,
            "internal agent error"
        );
    }

    #[test]
    fn round_trip_with_hash_precondition_and_base64() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/data.txt");
        let written = write(WriteRequest {
            path: path.to_string_lossy().to_string(),
            content: "aGVsbG8=".into(),
            encoding: "base64".into(),
            create_dirs: true,
            ..Default::default()
        });
        assert_eq!(written.error, "");
        assert_eq!(written.size, 5);
        let read_back = read(ReadRequest {
            path: path.to_string_lossy().to_string(),
            ..Default::default()
        });
        assert_eq!(read_back.content, "hello");
        assert_eq!(read_back.sha256, written.sha256);
        let conflict = write(WriteRequest {
            path: path.to_string_lossy().to_string(),
            content: "changed".into(),
            if_match_sha256: "0".repeat(64),
            ..Default::default()
        });
        assert!(conflict.error.contains("sha256 mismatch"));
        let listed = list(ListRequest {
            path: path.parent().unwrap().to_string_lossy().to_string(),
            ..Default::default()
        });
        assert_eq!(listed.total, 1);
        assert_eq!(listed.entries[0].name, "data.txt");
        let removed = delete(DeleteRequest {
            path: dir.path().join("nested").to_string_lossy().to_string(),
            recursive: true,
        });
        assert_eq!(removed.deleted_count, 2);
    }

    #[cfg(unix)]
    #[test]
    fn read_refuses_symlink_and_fifo() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, b"secret").unwrap();
        let link = dir.path().join("link");
        symlink(&file, &link).unwrap();
        assert!(!read(ReadRequest {
            path: link.to_string_lossy().to_string(),
            ..Default::default()
        })
        .error
        .is_empty());

        let real_dir = dir.path().join("real_dir");
        fs::create_dir(&real_dir).unwrap();
        let dir_link = dir.path().join("dir_link");
        symlink(&real_dir, &dir_link).unwrap();
        assert!(open_directory(&dir_link).is_err());

        let fifo = dir.path().join("fifo");
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(open_directory(&fifo).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn anchored_parent_survives_path_replacement() {
        let root = tempfile::tempdir().unwrap();
        let visible = root.path().join("visible");
        let original = root.path().join("original");
        let replacement = root.path().join("replacement");
        fs::create_dir(&visible).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(visible.join("data"), b"A").unwrap();
        fs::write(replacement.join("data"), b"B").unwrap();
        let parent = open_directory(&visible).unwrap();
        fs::rename(&visible, &original).unwrap();
        fs::rename(&replacement, &visible).unwrap();

        let mut file = open_regular_at(&parent, &visible.join("data")).unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).unwrap();
        assert_eq!(content, "A");

        let (mut temporary, mut entry) = open_write_temp(&parent, 0o600).unwrap();
        temporary.write_all(b"new A").unwrap();
        temporary.sync_all().unwrap();
        let target = path_component(&visible.join("data")).unwrap();
        entry.rename_to(&target).unwrap();
        assert_eq!(fs::read(original.join("data")).unwrap(), b"new A");
        assert_eq!(fs::read(visible.join("data")).unwrap(), b"B");

        let (_, unused) = open_write_temp(&parent, 0o600).unwrap();
        let unused_name = unused.name.to_str().unwrap().to_owned();
        drop(unused);
        assert!(!original.join(unused_name).exists());
    }

    #[cfg(unix)]
    #[test]
    fn recursive_delete_does_not_follow_symlink_children() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let tree = root.path().join("tree");
        let outside = root.path().join("outside");
        fs::create_dir(&tree).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"must survive").unwrap();
        fs::write(tree.join("local"), b"remove").unwrap();
        symlink(&outside, tree.join("link")).unwrap();

        let deleted = delete(DeleteRequest {
            path: tree.to_string_lossy().into_owned(),
            recursive: true,
        });
        assert_eq!(deleted.error, "");
        assert!(deleted.deleted_count >= 3);
        assert!(!tree.exists());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"must survive");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn write_refuses_existing_symlink_and_keeps_its_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        let link = root.path().join("link");
        fs::write(&outside, b"unchanged").unwrap();
        symlink(&outside, &link).unwrap();
        let response = write(WriteRequest {
            path: link.to_string_lossy().into_owned(),
            content: "replacement".into(),
            ..Default::default()
        });
        assert_eq!(response.error, "path is not a regular file");
        assert_eq!(fs::read(&outside).unwrap(), b"unchanged");
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_fs_operations_reject_symlink_ancestors() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        let nested = outside.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("keep"), b"outside data").unwrap();
        let link = root.path().join("link");
        symlink(&outside, &link).unwrap();
        let through = link.join("nested/keep").to_string_lossy().into_owned();

        assert!(!read(ReadRequest {
            path: through.clone(),
            ..Default::default()
        })
        .error
        .is_empty());
        assert!(!list(ListRequest {
            path: link.join("nested").to_string_lossy().into_owned(),
            ..Default::default()
        })
        .error
        .is_empty());
        assert!(!delete(DeleteRequest {
            path: through,
            recursive: false,
        })
        .error
        .is_empty());
        assert!(!write(WriteRequest {
            path: link.join("new/file").to_string_lossy().into_owned(),
            content: "blocked".into(),
            create_dirs: true,
            ..Default::default()
        })
        .error
        .is_empty());
        assert!(!outside.join("new").exists());
        assert_eq!(fs::read(nested.join("keep")).unwrap(), b"outside data");

        let direct = root.path().join("direct/new/file");
        let accepted = write(WriteRequest {
            path: direct.to_string_lossy().into_owned(),
            content: "created".into(),
            create_dirs: true,
            ..Default::default()
        });
        assert!(accepted.error.is_empty(), "{}", accepted.error);
        assert_eq!(fs::read(direct).unwrap(), b"created");
    }

    #[cfg(windows)]
    #[test]
    fn windows_regular_open_rejects_final_symlink() {
        use std::os::windows::fs::symlink_file;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        fs::write(&target, b"private").unwrap();
        if let Err(error) = symlink_file(&target, &link) {
            eprintln!("Windows file-symlink test skipped: {error}");
            return;
        }

        assert_eq!(fs::read(&target).unwrap(), b"private");
        assert!(open_regular(&target).is_ok());
        assert!(open_regular(&link).is_err());
        let response = read(ReadRequest {
            path: link.to_string_lossy().into_owned(),
            ..Default::default()
        });
        assert!(!response.error.is_empty());
        assert!(response.content.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_directory_open_rejects_junction() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real");
        let junction = dir.path().join("junction");
        fs::create_dir(&target).unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success(), "could not create a Windows junction");
        assert!(open_directory(&target).is_ok());
        assert!(open_directory(&junction).is_err());
        let response = list(ListRequest {
            path: junction.to_string_lossy().into_owned(),
            show_hidden: true,
        });
        assert!(!response.error.is_empty());
        fs::remove_dir(&junction).unwrap();
        assert!(target.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_anchor_blocks_parent_replacement() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("data.txt"), b"original").unwrap();
        let anchor = crate::platform::anchor_directory(&parent, false).unwrap();
        assert!(fs::rename(&parent, root.path().join("moved")).is_err());
        assert_eq!(fs::read(parent.join("data.txt")).unwrap(), b"original");
        drop(anchor);
        fs::rename(&parent, root.path().join("moved")).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_create_dirs_rejects_junction_ancestor() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        let junction = root.path().join("junction");
        fs::create_dir(&outside).unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .status()
            .unwrap();
        assert!(status.success());
        let result = write(WriteRequest {
            path: junction
                .join("nested")
                .join("data.txt")
                .to_string_lossy()
                .into_owned(),
            content: "blocked".into(),
            create_dirs: true,
            ..Default::default()
        });
        assert!(!result.error.is_empty());
        assert!(!outside.join("nested").exists());
        fs::remove_dir(&junction).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_recursive_delete_does_not_follow_junction_children() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        let target = dir.path().join("outside");
        let junction = tree.join("junction");
        fs::create_dir(&tree).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep.txt"), b"must survive").unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success(), "could not create a Windows junction");

        let deleted = delete(DeleteRequest {
            path: tree.to_string_lossy().into_owned(),
            recursive: true,
        });
        assert_eq!(deleted.error, "");
        assert!(!tree.exists());
        assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"must survive");
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_ads_and_serializes_case_variants() {
        let upper = PathBuf::from(r"C:\Data\A.TXT");
        let lower = PathBuf::from(r"c:\data\a.txt");
        assert_eq!(stripe(&upper), stripe(&lower));
        assert!(resolve(r"C:\Data\a.txt:stream").is_err());
    }
}
