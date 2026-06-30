use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::core::error::{ArchivaError, Result};
use crate::core::time::{now_utc_millis, parse_utc_millis};

const STALE_LOCK_AGE_MILLIS: i128 = 2 * 60 * 1000;
const LOCK_RETRY_TIMEOUT_MILLIS: u64 = 1_000;
const LOCK_RETRY_SLEEP_MILLIS: u64 = 20;
const LOCK_METADATA_MAX_BYTES: usize = 64 * 1024;
pub const TEXT_FILE_MAX_BYTES: usize = 10 * 1024 * 1024;
pub const SOURCE_FILE_MAX_BYTES: usize = 128 * 1024 * 1024;
static LOCK_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

const SKIPPED_WALK_DIRS: &[&str] = &[
    ".git",
    ".next",
    ".turbo",
    ".cache",
    "coverage",
    "dist",
    "build",
    "out",
    "node_modules",
];

pub fn path_exists(path: &Path) -> Result<bool> {
    path.try_exists().map_err(|source| {
        ArchivaError::io(
            Some(path.to_path_buf()),
            "check whether path exists",
            source,
        )
    })
}

pub fn ensure_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent).map_err(|source| {
        ArchivaError::io(
            Some(parent.to_path_buf()),
            "create parent directory",
            source,
        )
    })
}

pub fn read_text_if_exists(path: &Path) -> Result<Option<String>> {
    read_text_if_exists_with_limit(path, TEXT_FILE_MAX_BYTES, "read file")
}

pub fn read_text_if_exists_with_limit(
    path: &Path,
    max_bytes: usize,
    action: &'static str,
) -> Result<Option<String>> {
    match File::open(path) {
        Ok(file) => read_open_text_file_with_limit(file, path, max_bytes, action).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ArchivaError::io(Some(path.to_path_buf()), action, source)),
    }
}

pub fn read_text_file_with_limit(
    path: &Path,
    max_bytes: usize,
    action: &'static str,
) -> Result<String> {
    let file = File::open(path)
        .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))?;
    read_open_text_file_with_limit(file, path, max_bytes, action)
}

fn read_open_text_file_with_limit(
    mut file: File,
    path: &Path,
    max_bytes: usize,
    action: &'static str,
) -> Result<String> {
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))?;
    if bytes.len() > max_bytes {
        return Err(ArchivaError::FileTooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
        });
    }
    String::from_utf8(bytes).map_err(|source| {
        ArchivaError::io(
            Some(path.to_path_buf()),
            action,
            io::Error::new(io::ErrorKind::InvalidData, source),
        )
    })
}

pub fn atomic_write_text(path: &Path, content: &str) -> Result<()> {
    atomic_write_bytes(path, content.as_bytes())
}

pub fn atomic_write_bytes(path: &Path, content: &[u8]) -> Result<()> {
    atomic_write_bytes_impl(path, content, None)
}

fn atomic_write_bytes_impl(
    path: &Path,
    content: &[u8],
    fault: Option<AtomicWriteTestFault>,
) -> Result<()> {
    ensure_parent_dir(path)?;
    let (temp_path, temp_file) = create_temp_sibling(path)?;
    let mut temp_file = Some(temp_file);
    let result = (|| {
        maybe_fail_atomic_write(path, fault, AtomicWriteTestStage::Create)?;
        let file = temp_file.as_mut().expect("temporary file handle exists");
        file.write_all(content).map_err(|source| {
            ArchivaError::io(Some(temp_path.clone()), "write temporary file", source)
        })?;
        maybe_fail_atomic_write(path, fault, AtomicWriteTestStage::Write)?;
        let file = temp_file.as_mut().expect("temporary file handle exists");
        file.sync_all().map_err(|source| {
            ArchivaError::io(Some(temp_path.clone()), "flush temporary file", source)
        })?;
        maybe_fail_atomic_write(path, fault, AtomicWriteTestStage::Sync)?;
        drop(temp_file.take());
        replace_file(&temp_path, path)?;
        maybe_fail_atomic_write(path, fault, AtomicWriteTestStage::Replace)?;
        best_effort_flush_parent_dir(path);
        Ok(())
    })();

    if result.is_err() {
        drop(temp_file);
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
enum AtomicWriteTestFault {
    Return(AtomicWriteTestStage),
    Abort(AtomicWriteTestStage),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
enum AtomicWriteTestStage {
    Create,
    Write,
    Sync,
    Replace,
}

#[cfg_attr(not(test), allow(dead_code))]
impl AtomicWriteTestStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "after-create",
            Self::Write => "after-write",
            Self::Sync => "after-sync",
            Self::Replace => "after-replace",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "after-create" => Some(Self::Create),
            "after-write" => Some(Self::Write),
            "after-sync" => Some(Self::Sync),
            "after-replace" => Some(Self::Replace),
            _ => None,
        }
    }
}

fn maybe_fail_atomic_write(
    target: &Path,
    configured: Option<AtomicWriteTestFault>,
    stage: AtomicWriteTestStage,
) -> Result<()> {
    match configured {
        Some(AtomicWriteTestFault::Return(configured_stage)) if configured_stage == stage => {
            return Err(ArchivaError::cli(format!(
                "Injected atomic write failure at {:?} for {}",
                stage,
                target.display()
            )));
        }
        Some(AtomicWriteTestFault::Abort(configured_stage)) if configured_stage == stage => {
            std::process::abort();
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
fn atomic_write_text_with_test_fault(
    path: &Path,
    content: &str,
    fault: AtomicWriteTestFault,
) -> Result<()> {
    atomic_write_bytes_impl(path, content.as_bytes(), Some(fault))
}

#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    token: String,
    released: bool,
}

impl FileLock {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn release(mut self) -> Result<()> {
        self.release_inner()?;
        self.released = true;
        Ok(())
    }

    fn release_inner(&mut self) -> Result<()> {
        let content = match read_lock_file(&self.path, "read lock file before release")? {
            LockFileRead::Missing | LockFileRead::Oversized => return Ok(()),
            LockFileRead::Content(content) => content,
        };
        if parse_lock_metadata(&content).token.as_deref() != Some(self.token.as_str()) {
            return Ok(());
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(ArchivaError::io(
                Some(self.path.clone()),
                "release lock file",
                source,
            )),
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.release_inner();
            self.released = true;
        }
    }
}

pub fn acquire_file_lock_now(lock_path: &Path, command: &str) -> Result<FileLock> {
    let timestamp = now_utc_millis()
        .map_err(|source| ArchivaError::cli(format!("Failed to read system time: {source}")))?;
    acquire_file_lock(lock_path, command, &timestamp)
}

pub fn acquire_file_lock(lock_path: &Path, command: &str, timestamp: &str) -> Result<FileLock> {
    ensure_parent_dir(lock_path)?;
    let token = next_lock_token(timestamp);
    let metadata = format_lock_metadata(command, timestamp, &token);
    let started = Instant::now();

    loop {
        if create_lock_file(lock_path, &metadata)? {
            return Ok(FileLock {
                path: lock_path.to_path_buf(),
                token,
                released: false,
            });
        }
        if recover_stale_lock(lock_path, timestamp)? {
            continue;
        }
        if started.elapsed() >= Duration::from_millis(LOCK_RETRY_TIMEOUT_MILLIS) {
            return Err(existing_lock_error(lock_path));
        }
        thread::sleep(Duration::from_millis(LOCK_RETRY_SLEEP_MILLIS));
    }
}

fn create_lock_file(lock_path: &Path, metadata: &str) -> Result<bool> {
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => {
            if error.kind() == io::ErrorKind::PermissionDenied && path_exists(lock_path)? {
                return Ok(false);
            }
            return Err(ArchivaError::io(
                Some(lock_path.to_path_buf()),
                "create lock file",
                error,
            ));
        }
    };

    let result: Result<()> = (|| {
        file.write_all(metadata.as_bytes()).map_err(|source| {
            ArchivaError::io(Some(lock_path.to_path_buf()), "write lock file", source)
        })?;
        file.sync_all().map_err(|source| {
            ArchivaError::io(Some(lock_path.to_path_buf()), "flush lock file", source)
        })?;
        Ok(())
    })();

    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(lock_path);
    }
    result?;

    Ok(true)
}

fn recover_stale_lock(lock_path: &Path, contender_timestamp: &str) -> Result<bool> {
    let content = match read_lock_file(lock_path, "read lock file")? {
        LockFileRead::Missing => return Ok(true),
        content => content,
    };
    if !lock_file_read_is_recoverable(lock_path, &content, contender_timestamp)? {
        return Ok(false);
    }

    let Some(_recovery_lock) = acquire_stale_recovery_lock(lock_path, contender_timestamp)? else {
        return Ok(false);
    };
    let current_content = match read_lock_file(lock_path, "read lock file before stale recovery")? {
        LockFileRead::Missing => return Ok(true),
        content => content,
    };
    if !lock_file_read_is_recoverable(lock_path, &current_content, contender_timestamp)? {
        return Ok(false);
    }
    remove_stale_lock(lock_path)
}

#[derive(Debug)]
enum LockFileRead {
    Missing,
    Content(String),
    Oversized,
}

fn read_lock_file(lock_path: &Path, action: &'static str) -> Result<LockFileRead> {
    let file = match File::open(lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(LockFileRead::Missing),
        Err(source) => {
            return Err(ArchivaError::io(
                Some(lock_path.to_path_buf()),
                action,
                source,
            ));
        }
    };
    let mut bytes = Vec::new();
    file.take((LOCK_METADATA_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| ArchivaError::io(Some(lock_path.to_path_buf()), action, source))?;
    if bytes.len() > LOCK_METADATA_MAX_BYTES {
        return Ok(LockFileRead::Oversized);
    }
    Ok(LockFileRead::Content(
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
}

fn lock_file_read_is_recoverable(
    lock_path: &Path,
    content: &LockFileRead,
    contender_timestamp: &str,
) -> Result<bool> {
    match content {
        LockFileRead::Missing => Ok(true),
        LockFileRead::Content(content) => {
            lock_is_recoverable(lock_path, content, contender_timestamp)
        }
        LockFileRead::Oversized => lock_file_modified_is_expired(lock_path, contender_timestamp),
    }
}

fn lock_is_recoverable(lock_path: &Path, content: &str, contender_timestamp: &str) -> Result<bool> {
    let lock = parse_lock_metadata(content);
    if lock_owner_is_live(lock.pid) {
        return Ok(false);
    }
    if lock_timestamp_is_expired(lock.timestamp.as_deref(), contender_timestamp) {
        return Ok(true);
    }
    if lock
        .timestamp
        .as_deref()
        .and_then(parse_utc_millis)
        .is_some()
    {
        return Ok(false);
    }
    lock_file_modified_is_expired(lock_path, contender_timestamp)
}

fn lock_file_modified_is_expired(lock_path: &Path, contender_timestamp: &str) -> Result<bool> {
    let Some(contender_millis) = parse_utc_millis(contender_timestamp) else {
        return Ok(false);
    };
    let metadata = fs::metadata(lock_path).map_err(|source| {
        ArchivaError::io(
            Some(lock_path.to_path_buf()),
            "read lock file metadata",
            source,
        )
    })?;
    let modified = metadata.modified().map_err(|source| {
        ArchivaError::io(
            Some(lock_path.to_path_buf()),
            "read lock file modified time",
            source,
        )
    })?;
    let Ok(duration) = modified.duration_since(UNIX_EPOCH) else {
        return Ok(false);
    };
    let modified_millis = duration.as_millis() as i128;
    Ok(contender_millis - modified_millis >= STALE_LOCK_AGE_MILLIS)
}

fn acquire_stale_recovery_lock(
    lock_path: &Path,
    contender_timestamp: &str,
) -> Result<Option<FileLock>> {
    let recovery_path = stale_recovery_lock_path(lock_path);
    let token = next_lock_token(contender_timestamp);
    let metadata = format_lock_metadata("recover-stale-lock", contender_timestamp, &token);
    if !create_lock_file(&recovery_path, &metadata)? {
        let content = match read_lock_file(&recovery_path, "read stale recovery lock file")? {
            LockFileRead::Missing => {
                if !create_lock_file(&recovery_path, &metadata)? {
                    return Ok(None);
                }
                return Ok(Some(FileLock {
                    path: recovery_path,
                    token,
                    released: false,
                }));
            }
            content => content,
        };
        if !lock_file_read_is_recoverable(&recovery_path, &content, contender_timestamp)? {
            return Ok(None);
        }
        remove_stale_lock(&recovery_path)?;
        if !create_lock_file(&recovery_path, &metadata)? {
            return Ok(None);
        }
    }
    Ok(Some(FileLock {
        path: recovery_path,
        token,
        released: false,
    }))
}

fn stale_recovery_lock_path(lock_path: &Path) -> PathBuf {
    let file_name = lock_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("archiva.lock"));
    let mut recovery_name = file_name;
    recovery_name.push(".recover");
    lock_path.with_file_name(recovery_name)
}

fn remove_stale_lock(lock_path: &Path) -> Result<bool> {
    match fs::remove_file(lock_path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(source) => Err(ArchivaError::io(
            Some(lock_path.to_path_buf()),
            "remove stale lock file",
            source,
        )),
    }
}

fn existing_lock_error(lock_path: &Path) -> ArchivaError {
    ArchivaError::cli(format!(
        "Archiva lock already exists at {}. Another Archiva process may be writing; retry later.",
        lock_path.display()
    ))
}

#[derive(Debug, Default)]
struct LockMetadata {
    token: Option<String>,
    timestamp: Option<String>,
    pid: Option<u32>,
}

fn parse_lock_metadata(content: &str) -> LockMetadata {
    let mut metadata = LockMetadata::default();
    for line in content.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "pid" => metadata.pid = parse_lock_pid(value.trim()),
            "token" => metadata.token = Some(value.trim().to_string()),
            "timestamp" => metadata.timestamp = Some(value.trim().to_string()),
            _ => {}
        }
    }
    metadata
}

fn parse_lock_pid(value: &str) -> Option<u32> {
    let pid = value.parse::<u32>().ok()?;
    (pid > 0).then_some(pid)
}

fn lock_timestamp_is_expired(lock_timestamp: Option<&str>, contender_timestamp: &str) -> bool {
    let Some(lock_millis) = lock_timestamp.and_then(parse_utc_millis) else {
        return false;
    };
    let Some(contender_millis) = parse_utc_millis(contender_timestamp) else {
        return false;
    };
    contender_millis - lock_millis >= STALE_LOCK_AGE_MILLIS
}

fn lock_owner_is_live(pid: Option<u32>) -> bool {
    pid.is_some_and(process_is_live)
}

#[cfg(unix)]
fn process_is_live(pid: u32) -> bool {
    if pid > i32::MAX as u32 {
        return false;
    }

    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    let result = unsafe { kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }

    const ESRCH: i32 = 3;
    io::Error::last_os_error().raw_os_error() != Some(ESRCH)
}

#[cfg(windows)]
fn process_is_live(pid: u32) -> bool {
    use std::ffi::c_void;

    type Handle = *mut c_void;

    unsafe extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const ERROR_INVALID_PARAMETER: i32 = 87;

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if !handle.is_null() {
        let _ = unsafe { CloseHandle(handle) };
        return true;
    }

    io::Error::last_os_error().raw_os_error() != Some(ERROR_INVALID_PARAMETER)
}

#[cfg(not(any(unix, windows)))]
fn process_is_live(_pid: u32) -> bool {
    false
}

pub fn list_files(root: &Path, predicate: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>> {
    let mut output = Vec::new();
    walk(root, &predicate, true, &mut output)?;
    output.sort();
    Ok(output)
}

pub fn list_storage_files(root: &Path, predicate: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>> {
    let mut output = Vec::new();
    walk(root, &predicate, false, &mut output)?;
    output.sort();
    Ok(output)
}

fn create_temp_sibling(target: &Path) -> Result<(PathBuf, File)> {
    let parent = writable_parent(target);
    for attempt in 0..1000_u32 {
        let temp_path = temp_sibling_path(target, attempt);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(ArchivaError::io(
                    Some(parent.to_path_buf()),
                    "create temporary file",
                    source,
                ));
            }
        }
    }

    Err(ArchivaError::cli(format!(
        "Failed to create temporary sibling for {} after 1000 attempts",
        target.display()
    )))
}

fn temp_sibling_path(target: &Path, attempt: u32) -> PathBuf {
    let parent = writable_parent(target);
    let file_name = target
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("archiva"));
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".archiva-tmp-{}-{attempt}", std::process::id()));
    parent.join(temp_name)
}

fn next_lock_token(timestamp: &str) -> String {
    let counter = LOCK_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{}-{counter}",
        std::process::id(),
        lock_field(timestamp)
            .chars()
            .map(|character| if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            })
            .collect::<String>()
    )
}

fn format_lock_metadata(command: &str, timestamp: &str, token: &str) -> String {
    format!(
        "version=1\npid={}\ntoken={}\ncommand={}\ntimestamp={}\n",
        std::process::id(),
        lock_field(token),
        lock_field(command),
        lock_field(timestamp)
    )
}

fn lock_field(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\r' | '\n' => ' ',
            _ => character,
        })
        .collect()
}

fn writable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, target: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    let existing = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let new = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let ok = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            new.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(ArchivaError::io(
            Some(target.to_path_buf()),
            "atomically replace file",
            io::Error::last_os_error(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, target: &Path) -> Result<()> {
    fs::rename(temp_path, target).map_err(|source| {
        ArchivaError::io(
            Some(target.to_path_buf()),
            "atomically replace file",
            source,
        )
    })
}

#[cfg(unix)]
fn best_effort_flush_parent_dir(path: &Path) {
    let parent = writable_parent(path);
    let _ = File::open(parent).and_then(|directory| directory.sync_all());
}

#[cfg(not(unix))]
fn best_effort_flush_parent_dir(_path: &Path) {}

fn walk(
    dir: &Path,
    predicate: &impl Fn(&Path) -> bool,
    skip_generated_dirs: bool,
    output: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ArchivaError::io(
                Some(dir.to_path_buf()),
                "read directory",
                source,
            ));
        }
    };

    for entry in entries {
        let entry = entry.map_err(|source| {
            ArchivaError::io(Some(dir.to_path_buf()), "read directory entry", source)
        })?;
        let path = entry.path();
        let name = entry.file_name();
        if skip_generated_dirs
            && SKIPPED_WALK_DIRS
                .iter()
                .any(|skipped| name.as_os_str() == *skipped)
        {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|source| ArchivaError::io(Some(path.clone()), "read file type", source))?;
        if file_type.is_dir() {
            walk(&path, predicate, skip_generated_dirs, output)?;
        } else if file_type.is_file() && predicate(&path) {
            output.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_file_lock, acquire_file_lock_now, acquire_stale_recovery_lock, atomic_write_text,
        atomic_write_text_with_test_fault, ensure_parent_dir, list_files, list_storage_files,
        path_exists, read_text_file_with_limit, read_text_if_exists,
        read_text_if_exists_with_limit, recover_stale_lock, stale_recovery_lock_path,
        AtomicWriteTestFault, AtomicWriteTestStage, LOCK_METADATA_MAX_BYTES,
    };
    use crate::core::dlog::parse_dlog_yaml;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[test]
    fn reads_optional_text_and_creates_parent_dirs() {
        let root = unique_temp_dir("archiva-fs-read");
        let file = root.join("a").join("b.txt");
        assert!(!path_exists(&file).unwrap());
        assert_eq!(read_text_if_exists(&file).unwrap(), None);
        ensure_parent_dir(&file).unwrap();
        fs::write(&file, "hello").unwrap();
        assert!(path_exists(&file).unwrap());
        assert_eq!(
            read_text_if_exists(&file).unwrap(),
            Some("hello".to_string())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn optional_read_and_directory_walk_surface_real_io_errors() {
        let root = unique_temp_dir("archiva-fs-io-errors");
        fs::create_dir_all(&root).unwrap();

        let read_error = read_text_if_exists(&root).unwrap_err().user_message();
        assert!(read_error.contains("Failed to read file"));

        let file_root = root.join("not-a-directory");
        fs::write(&file_root, "").unwrap();
        let walk_error = list_files(&file_root, |_| true).unwrap_err().user_message();
        assert!(walk_error.contains("Failed to read directory"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bounded_text_reads_reject_files_larger_than_limit() {
        let root = unique_temp_dir("archiva-fs-bounded-read");
        let file = root.join("oversized.txt");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "abcdef").unwrap();

        let optional_error = read_text_if_exists_with_limit(&file, 5, "read file")
            .unwrap_err()
            .user_message();
        let required_error = read_text_file_with_limit(&file, 5, "read file")
            .unwrap_err()
            .user_message();

        assert!(optional_error.contains("exceeds configured byte limit of 5 bytes"));
        assert_eq!(optional_error, required_error);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn acquires_lock_file_with_token_pid_command_and_timestamp_metadata() {
        let root = unique_temp_dir("archiva-fs-lock");
        let lock_path = root.join(".decisions").join("src").join("a.ts.lock");

        let lock =
            acquire_file_lock(&lock_path, "write-decision", "2026-06-26T20:31:18.340Z").unwrap();

        assert_eq!(lock.path(), lock_path.as_path());
        let content = fs::read_to_string(&lock_path).unwrap();
        assert!(content.starts_with(&format!("version=1\npid={}\n", std::process::id())));
        assert!(content.contains("\ntoken="));
        assert!(content.contains("\ncommand=write-decision\n"));
        assert!(content.ends_with("timestamp=2026-06-26T20:31:18.340Z\n"));

        lock.release().unwrap();
        assert!(!lock_path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_existing_lock_without_overwriting_metadata() {
        let root = unique_temp_dir("archiva-fs-lock-existing");
        let lock_path = root.join("file.lock");
        fs::create_dir_all(&root).unwrap();
        let existing = format!(
            "version=1\npid={}\ntoken=active\ncommand=other\ntimestamp=2026-06-26T20:31:00.000Z\n",
            std::process::id()
        );
        fs::write(&lock_path, &existing).unwrap();

        let error = acquire_file_lock(&lock_path, "write-decision", "2026-06-26T20:31:18.340Z")
            .unwrap_err()
            .user_message();

        assert!(error.contains("Archiva lock already exists"));
        assert_eq!(fs::read_to_string(&lock_path).unwrap(), existing);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovers_expired_lock_and_replaces_metadata() {
        let root = unique_temp_dir("archiva-fs-lock-expired");
        let lock_path = root.join("file.lock");
        let recovery_path = stale_recovery_lock_path(&lock_path);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &lock_path,
            format!(
                "pid={}\ntoken=stale\ncommand=other\ntimestamp=2026-06-26T20:00:00.000Z\n",
                dead_pid_for_test()
            ),
        )
        .unwrap();

        let lock =
            acquire_file_lock(&lock_path, "write-decision", "2026-06-26T20:03:00.000Z").unwrap();

        let content = fs::read_to_string(&lock_path).unwrap();
        assert!(content.starts_with(&format!("version=1\npid={}\n", std::process::id())));
        assert!(content.contains("\ntoken="));
        assert!(content.contains("\ncommand=write-decision\n"));
        assert!(content.ends_with("timestamp=2026-06-26T20:03:00.000Z\n"));
        assert!(!recovery_path.exists());
        lock.release().unwrap();
        assert!(!lock_path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn does_not_recover_expired_lock_while_owner_pid_is_live() {
        let root = unique_temp_dir("archiva-fs-lock-expired-live-pid");
        let lock_path = root.join("file.lock");
        fs::create_dir_all(&root).unwrap();
        let existing = format!(
            "pid={}\ntoken=active\ncommand=other\ntimestamp=2026-06-26T20:00:00.000Z\n",
            std::process::id()
        );
        fs::write(&lock_path, &existing).unwrap();

        let recovered = recover_stale_lock(&lock_path, "2026-06-26T20:03:00.000Z").unwrap();

        assert!(!recovered);
        assert_eq!(fs::read_to_string(&lock_path).unwrap(), existing);
        let _ = fs::remove_file(&lock_path);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovers_pidless_expired_lock_by_timestamp() {
        let root = unique_temp_dir("archiva-fs-lock-expired-pidless");
        let lock_path = root.join("file.lock");
        let recovery_path = stale_recovery_lock_path(&lock_path);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &lock_path,
            "command=other\ntimestamp=2026-06-26T20:00:00.000Z\n",
        )
        .unwrap();

        let lock =
            acquire_file_lock(&lock_path, "write-decision", "2026-06-26T20:03:00.000Z").unwrap();

        let content = fs::read_to_string(&lock_path).unwrap();
        assert!(content.starts_with(&format!("version=1\npid={}\n", std::process::id())));
        assert!(content.contains("\ntoken="));
        assert!(content.contains("\ncommand=write-decision\n"));
        assert!(content.ends_with("timestamp=2026-06-26T20:03:00.000Z\n"));
        assert!(!recovery_path.exists());
        lock.release().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovers_malformed_lock_by_modified_time_without_live_owner() {
        for (name, content) in [
            ("empty", ""),
            ("partial", "token=stale\ncommand=other\n"),
            (
                "bad-timestamp",
                "pid=999999999\ntoken=stale\ncommand=other\ntimestamp=not-a-time\n",
            ),
        ] {
            let root = unique_temp_dir(&format!("archiva-fs-lock-malformed-{name}"));
            let lock_path = root.join("file.lock");
            fs::create_dir_all(&root).unwrap();
            fs::write(&lock_path, content).unwrap();

            let lock = acquire_file_lock(&lock_path, "write-decision", "2099-01-01T00:00:00.000Z")
                .unwrap();

            let recovered = fs::read_to_string(&lock_path).unwrap();
            assert!(recovered.starts_with(&format!("version=1\npid={}\n", std::process::id())));
            assert!(recovered.contains("\ncommand=write-decision\n"));
            assert!(recovered.ends_with("timestamp=2099-01-01T00:00:00.000Z\n"));
            lock.release().unwrap();
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn recovers_oversized_lock_by_modified_time_without_full_metadata_read() {
        let root = unique_temp_dir("archiva-fs-lock-oversized");
        let lock_path = root.join("file.lock");
        fs::create_dir_all(&root).unwrap();
        fs::write(&lock_path, vec![b'x'; LOCK_METADATA_MAX_BYTES + 1]).unwrap();

        let lock =
            acquire_file_lock(&lock_path, "write-decision", "2099-01-01T00:00:00.000Z").unwrap();

        let recovered = fs::read_to_string(&lock_path).unwrap();
        assert!(recovered.starts_with(&format!("version=1\npid={}\n", std::process::id())));
        assert!(recovered.contains("\ncommand=write-decision\n"));
        assert!(recovered.ends_with("timestamp=2099-01-01T00:00:00.000Z\n"));
        lock.release().unwrap();
        assert!(!lock_path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovers_expired_stale_recovery_guard() {
        let root = unique_temp_dir("archiva-fs-lock-expired-recovery-guard");
        let lock_path = root.join("file.lock");
        let recovery_path = stale_recovery_lock_path(&lock_path);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &lock_path,
            format!(
                "pid={}\ntoken=stale\ncommand=other\ntimestamp=2026-06-26T20:00:00.000Z\n",
                dead_pid_for_test()
            ),
        )
        .unwrap();
        fs::write(
            &recovery_path,
            format!(
                "pid={}\ntoken=recover-stale\ncommand=recover-stale-lock\ntimestamp=2026-06-26T20:00:00.000Z\n",
                dead_pid_for_test()
            ),
        )
        .unwrap();

        let lock =
            acquire_file_lock(&lock_path, "write-decision", "2026-06-26T20:03:00.000Z").unwrap();

        let content = fs::read_to_string(&lock_path).unwrap();
        assert!(content.starts_with(&format!("version=1\npid={}\n", std::process::id())));
        assert!(content.contains("\ncommand=write-decision\n"));
        assert!(!recovery_path.exists());
        lock.release().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_recovery_guard_blocks_competing_stale_breakers() {
        let root = unique_temp_dir("archiva-fs-lock-recovery-guard");
        let lock_path = root.join("file.lock");
        fs::create_dir_all(&root).unwrap();
        let stale = format!(
            "pid={}\ntoken=stale\ncommand=other\ntimestamp=2026-06-26T20:00:00.000Z\n",
            dead_pid_for_test()
        );
        fs::write(&lock_path, &stale).unwrap();
        let recovery_lock =
            acquire_stale_recovery_lock(&lock_path, "2026-06-26T20:03:00.000Z").unwrap();
        assert!(recovery_lock.is_some());

        let recovered = recover_stale_lock(&lock_path, "2026-06-26T20:03:00.000Z").unwrap();

        assert!(!recovered);
        assert_eq!(fs::read_to_string(&lock_path).unwrap(), stale);
        drop(recovery_lock);
        let recovered = recover_stale_lock(&lock_path, "2026-06-26T20:03:00.000Z").unwrap();
        assert!(recovered);
        assert!(!lock_path.exists());
        assert!(!stale_recovery_lock_path(&lock_path).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn drops_lock_file_on_scope_exit_and_sanitizes_metadata_lines() {
        let root = unique_temp_dir("archiva-fs-lock-drop");
        let lock_path = root.join("file.lock");

        {
            let _lock = acquire_file_lock(
                &lock_path,
                "write-decision\nextra",
                "2026-06-26T20:31:18.340Z\r\nignored",
            )
            .unwrap();
            let content = fs::read_to_string(&lock_path).unwrap();
            assert!(content.starts_with(&format!("version=1\npid={}\n", std::process::id())));
            assert!(content.contains("\ncommand=write-decision extra\n"));
            assert!(content.ends_with("timestamp=2026-06-26T20:31:18.340Z  ignored\n"));
        }

        assert!(!lock_path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn drop_does_not_remove_replaced_lock_with_different_token() {
        let root = unique_temp_dir("archiva-fs-lock-replaced");
        let lock_path = root.join("file.lock");

        {
            let _lock = acquire_file_lock(&lock_path, "write-decision", "2026-06-26T20:31:18.340Z")
                .unwrap();
            fs::write(
                &lock_path,
                "version=1\npid=999\ntoken=replacement\ncommand=other\ntimestamp=2026-06-26T20:31:19.000Z\n",
            )
            .unwrap();
        }

        assert_eq!(
            fs::read_to_string(&lock_path).unwrap(),
            "version=1\npid=999\ntoken=replacement\ncommand=other\ntimestamp=2026-06-26T20:31:19.000Z\n"
        );
        let _ = fs::remove_file(&lock_path);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn acquires_lock_with_current_timestamp_for_future_workflow_callers() {
        let root = unique_temp_dir("archiva-fs-lock-now");
        let lock_path = root.join("file.lock");

        let lock = acquire_file_lock_now(&lock_path, "init").unwrap();
        let content = fs::read_to_string(&lock_path).unwrap();

        assert!(content.starts_with(&format!("version=1\npid={}\n", std::process::id())));
        assert!(content.contains("\ntoken="));
        assert!(content.contains("\ncommand=init\n"));
        assert!(content.ends_with("Z\n"));
        lock.release().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomically_writes_text_by_replacing_existing_files() {
        let root = unique_temp_dir("archiva-fs-atomic-replace");
        let file = root.join("nested").join("state.txt");

        atomic_write_text(&file, "old").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "old");

        atomic_write_text(&file, "new").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        assert_no_temp_siblings(file.parent().unwrap());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_write_uses_later_temp_name_when_first_candidate_exists() {
        let root = unique_temp_dir("archiva-fs-atomic-collision");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("state.txt");
        let collision = root.join(format!(".state.txt.archiva-tmp-{}-0", std::process::id()));
        fs::write(&collision, "collision").unwrap();

        atomic_write_text(&file, "content").unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "content");
        assert_eq!(fs::read_to_string(&collision).unwrap(), "collision");
        assert!(!root
            .join(format!(".state.txt.archiva-tmp-{}-1", std::process::id()))
            .exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_write_cleans_temp_file_when_replace_fails() {
        let root = unique_temp_dir("archiva-fs-atomic-failure");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();

        let error = atomic_write_text(&target, "content").unwrap_err();

        assert!(error.user_message().contains("atomically replace file"));
        assert!(target.is_dir());
        assert_no_temp_siblings(&root);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_write_faults_before_replace_preserve_old_complete_file() {
        for fault in [
            AtomicWriteTestFault::Return(AtomicWriteTestStage::Create),
            AtomicWriteTestFault::Return(AtomicWriteTestStage::Write),
            AtomicWriteTestFault::Return(AtomicWriteTestStage::Sync),
        ] {
            let root = unique_temp_dir("archiva-fs-atomic-pre-replace-fault");
            let target = root.join("src").join("state.dlog");
            let old_content = empty_dlog_content();
            let new_content = single_decision_dlog_content();
            atomic_write_text(&target, old_content).unwrap();

            let error = atomic_write_text_with_test_fault(&target, new_content, fault)
                .unwrap_err()
                .user_message();

            assert!(error.contains("Injected atomic write failure"));
            assert_eq!(fs::read_to_string(&target).unwrap(), old_content);
            parse_dlog_yaml(&fs::read_to_string(&target).unwrap()).unwrap();
            assert_no_temp_siblings(target.parent().unwrap());

            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn atomic_write_fault_after_replace_leaves_new_complete_file() {
        let root = unique_temp_dir("archiva-fs-atomic-after-replace-fault");
        let target = root.join("src").join("state.dlog");
        let old_content = empty_dlog_content();
        let new_content = single_decision_dlog_content();
        atomic_write_text(&target, old_content).unwrap();

        let error = atomic_write_text_with_test_fault(
            &target,
            new_content,
            AtomicWriteTestFault::Return(AtomicWriteTestStage::Replace),
        )
        .unwrap_err()
        .user_message();

        assert!(error.contains("Injected atomic write failure"));
        assert_eq!(fs::read_to_string(&target).unwrap(), new_content);
        parse_dlog_yaml(&fs::read_to_string(&target).unwrap()).unwrap();
        assert_no_temp_siblings(target.parent().unwrap());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_write_killed_child_never_leaves_truncated_target() {
        for stage in [
            AtomicWriteTestStage::Create,
            AtomicWriteTestStage::Write,
            AtomicWriteTestStage::Sync,
            AtomicWriteTestStage::Replace,
        ] {
            let root = unique_temp_dir("archiva-fs-atomic-killed-child");
            let target = root.join("src").join("state.dlog");
            let old_content = empty_dlog_content();
            let new_content = single_decision_dlog_content();
            atomic_write_text(&target, old_content).unwrap();

            let output = Command::new(std::env::current_exe().unwrap())
                .arg("atomic_write_child_process_entrypoint")
                .env("ARCHIVA_ATOMIC_WRITE_CHILD_TARGET", &target)
                .env("ARCHIVA_ATOMIC_WRITE_CHILD_STAGE", stage.as_str())
                .output()
                .unwrap();

            assert!(
                !output.status.success(),
                "child unexpectedly succeeded for {:?}: stdout={} stderr={}",
                stage,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let expected = match stage {
                AtomicWriteTestStage::Replace => new_content,
                _ => old_content,
            };
            assert_eq!(fs::read_to_string(&target).unwrap(), expected);
            parse_dlog_yaml(&fs::read_to_string(&target).unwrap()).unwrap();

            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn atomic_write_child_process_entrypoint() {
        let Some(target) = std::env::var_os("ARCHIVA_ATOMIC_WRITE_CHILD_TARGET") else {
            return;
        };
        let Some(stage) = std::env::var("ARCHIVA_ATOMIC_WRITE_CHILD_STAGE")
            .ok()
            .and_then(|value| AtomicWriteTestStage::parse(&value))
        else {
            return;
        };

        atomic_write_text_with_test_fault(
            &PathBuf::from(target),
            single_decision_dlog_content(),
            AtomicWriteTestFault::Abort(stage),
        )
        .unwrap();
        panic!("atomic write child did not abort at {stage:?}");
    }

    #[test]
    fn lists_files_sorted_and_skips_generated_directories() {
        let root = unique_temp_dir("archiva-fs-list");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("src").join("b.ts"), "").unwrap();
        fs::write(root.join("src").join("a.js"), "").unwrap();
        fs::write(root.join("node_modules").join("hidden.ts"), "").unwrap();
        fs::write(root.join(".git").join("hidden.ts"), "").unwrap();

        let files = list_files(&root, |path| {
            matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("ts" | "js")
            )
        })
        .unwrap();
        assert_eq!(
            relative_strings(&root, &files),
            vec!["src/a.js".to_string(), "src/b.ts".to_string()]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn storage_walk_includes_generated_directory_names() {
        let root = unique_temp_dir("archiva-fs-storage-list");
        fs::create_dir_all(root.join("src").join("build")).unwrap();
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::write(root.join("src").join("build").join("a.ts.dlog"), "").unwrap();
        fs::write(root.join("node_modules").join("b.ts.dlog"), "").unwrap();

        let files = list_storage_files(&root, |path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".dlog"))
        })
        .unwrap();

        assert_eq!(
            relative_strings(&root, &files),
            vec![
                "node_modules/b.ts.dlog".to_string(),
                "src/build/a.ts.dlog".to_string()
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn scan_ignores_symlinked_files_and_directories() {
        use std::os::unix::fs::symlink;

        let root = unique_temp_dir("archiva-fs-symlink-root");
        let outside = unique_temp_dir("archiva-fs-symlink-outside");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(root.join("src").join("real.ts"), "").unwrap();
        fs::write(outside.join("linked.ts"), "").unwrap();
        symlink(
            outside.join("linked.ts"),
            root.join("src").join("linked.ts"),
        )
        .unwrap();
        symlink(&outside, root.join("linked-dir")).unwrap();

        let files = list_files(&root, |path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("ts")
        })
        .unwrap();
        assert_eq!(
            relative_strings(&root, &files),
            vec!["src/real.ts".to_string()]
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    fn assert_no_temp_siblings(dir: &Path) {
        let entries = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".archiva-tmp-"))
            .collect::<Vec<_>>();
        assert_eq!(entries, Vec::<String>::new());
    }

    fn empty_dlog_content() -> &'static str {
        "file: src/state.ts\nschema: 1\ndecisions: {}\n"
    }

    fn single_decision_dlog_content() -> &'static str {
        "file: src/state.ts\nschema: 1\ndecisions:\n  fn:new:\n    id: dec_001\n    lines_hint:\n      - 1\n      - 3\n    fingerprint: '11111111'\n    chose: new write\n    because: crash simulation fixture\n    rejected: []\n    timestamp: '2026-06-26T20:31:18.340Z'\n    history: []\n"
    }

    fn relative_strings(root: &Path, files: &[PathBuf]) -> Vec<String> {
        files
            .iter()
            .map(|file| {
                file.strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect()
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("{prefix}-{}-{nanos}", std::process::id()));
        path
    }

    fn dead_pid_for_test() -> u32 {
        999_999_999
    }
}
