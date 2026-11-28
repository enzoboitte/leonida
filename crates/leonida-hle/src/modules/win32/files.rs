//! Files and paths on macOS. On the console title storage is a drive of its
//! own (`G:\` the package, `T:\` temporary files), and titles build paths on
//! that; here each drive is a host folder. A title path is turned into a host
//! path inside the folder behind its drive (`..` cannot climb out of it), and
//! files are host file descriptors behind Windows handles.
//!
//! shortcut: a title path on no mounted drive fails (`ERROR_PATH_NOT_FOUND`):
//! there is no `C:\` here. Names compare as the host volume does (APFS is
//! case-insensitive by default, as Windows is).

use std::collections::VecDeque;
use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use super::set_last_error;
use super::sync::{Object, changed, table};

pub(super) const INVALID_HANDLE: usize = usize::MAX;
const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const FILE_READ_DATA: u32 = 0x1;
const FILE_WRITE_DATA: u32 = 0x2;
const FILE_APPEND_DATA: u32 = 0x4;

const CREATE_NEW: u32 = 1;
const CREATE_ALWAYS: u32 = 2;
const OPEN_EXISTING: u32 = 3;
const OPEN_ALWAYS: u32 = 4;
const TRUNCATE_EXISTING: u32 = 5;

const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;

const FILE_ATTRIBUTE_READONLY: u32 = 0x01;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_TOO_MANY_OPEN_FILES: u32 = 4;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_NO_MORE_FILES: u32 = 18;
const ERROR_HANDLE_EOF: u32 = 38;
const ERROR_FILE_EXISTS: u32 = 80;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_DISK_FULL: u32 = 112;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_DIR_NOT_EMPTY: u32 = 145;
const ERROR_ALREADY_EXISTS: u32 = 183;

/// 100 ns intervals from 1601 (FILETIME) to 1970.
const FILETIME_TO_UNIX: u64 = 116_444_736_000_000_000;

/// Mounted drives: upper-case letter and host folder.
static DRIVES: Mutex<Vec<(char, PathBuf)>> = Mutex::new(Vec::new());
/// The title's current directory, as a title path.
static CURRENT: Mutex<String> = Mutex::new(String::new());

fn drives() -> std::sync::MutexGuard<'static, Vec<(char, PathBuf)>> {
    DRIVES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Gives the host folder `root` a drive letter (the same one if it already
/// has one): the highest letter no other mount uses.
pub fn mount(root: &Path) -> Option<char> {
    let mut drives = drives();
    if let Some((letter, _)) = drives.iter().find(|(_, r)| r == root) {
        return Some(*letter);
    }
    let letter = ('D'..='Z')
        .rev()
        .find(|l| !drives.iter().any(|(m, _)| m == l))?;
    tracing::info!(drive = %letter, root = %root.display(), "virtual drive");
    drives.push((letter, root.to_path_buf()));
    Some(letter)
}

/// Gives the host folder `root` a drive letter the console fixes.
pub fn mount_at(letter: char, root: &Path) {
    let letter = letter.to_ascii_uppercase();
    if !letter.is_ascii_uppercase() {
        return;
    }
    tracing::info!(drive = %letter, root = %root.display(), "virtual drive");
    let mut drives = drives();
    drives.retain(|(l, _)| *l != letter);
    drives.push((letter, root.to_path_buf()));
    if letter == 'G' {
        let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
        if current.is_empty() {
            *current = "G:\\".into();
        }
    }
}

/// A title path made whole: a drive letter, then its parts with `.` and
/// `..` resolved (never above the drive's root).
fn absolute(path: &str) -> Option<(char, Vec<String>)> {
    let path = path
        .strip_prefix(r"\\?\")
        .unwrap_or(path)
        .replace('/', "\\");
    let current = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (letter, rest) = match path.as_bytes() {
        [l, b':', ..] if l.is_ascii_alphabetic() => {
            let rest = &path[2..];
            let letter = (*l as char).to_ascii_uppercase();
            // `X:name` is relative to that drive's root here.
            (letter, rest.to_string())
        }
        [b'\\', ..] => (current.chars().next()?, path.clone()),
        _ => (
            current.chars().next()?,
            format!("{}\\{path}", current.get(2..)?),
        ),
    };
    let mut parts: Vec<String> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            // Windows drops trailing dots and spaces from names.
            name => parts.push(name.trim_end_matches(['.', ' ']).to_string()),
        }
    }
    Some((letter, parts))
}

/// The host path behind a title path, if it is on a mounted drive.
pub(crate) fn host_path(path: &str) -> Option<PathBuf> {
    let (letter, parts) = absolute(path)?;
    let drives = drives();
    let (_, root) = drives.iter().find(|(l, _)| *l == letter)?;
    let mut host = root.clone();
    host.extend(parts.iter().filter(|p| !p.is_empty()));
    Some(host)
}

/// The title path written back as Windows writes it.
fn title_path(letter: char, parts: &[String]) -> String {
    format!("{letter}:\\{}", parts.join("\\"))
}

/// # Safety
/// `s` is NUL-terminated UTF-16 (or null).
unsafe fn wide(s: *const u16) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let mut n = 0;
    // SAFETY: up to its terminator.
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` units before the terminator.
    Some(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(s, n)
    }))
}

/// # Safety
/// `s` is NUL-terminated (or null).
unsafe fn narrow(s: *const u8) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: as above.
    Some(
        unsafe { std::ffi::CStr::from_ptr(s.cast()) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// The host path for a title's wide path, or the last error set.
fn host_of(path: Option<String>) -> Option<PathBuf> {
    let host = path.as_deref().and_then(host_path);
    if host.is_none() {
        set_last_error(ERROR_PATH_NOT_FOUND);
    }
    host
}

/// The Windows error for a host one.
fn error_of(e: &std::io::Error) -> u32 {
    match e.raw_os_error() {
        Some(libc::ENOENT) => ERROR_FILE_NOT_FOUND,
        Some(libc::ENOTDIR) => ERROR_PATH_NOT_FOUND,
        Some(libc::EACCES | libc::EPERM | libc::EISDIR | libc::EROFS) => ERROR_ACCESS_DENIED,
        Some(libc::EEXIST) => ERROR_FILE_EXISTS,
        Some(libc::ENOTEMPTY) => ERROR_DIR_NOT_EMPTY,
        Some(libc::ENOSPC) => ERROR_DISK_FULL,
        Some(libc::EMFILE | libc::ENFILE) => ERROR_TOO_MANY_OPEN_FILES,
        Some(libc::EBADF) => ERROR_INVALID_HANDLE,
        _ => ERROR_ACCESS_DENIED,
    }
}

fn fail(e: &std::io::Error) {
    set_last_error(error_of(e));
}

/// A file's attributes as Windows gives them.
fn attributes(m: &std::fs::Metadata) -> u32 {
    let mut a = 0;
    if m.is_dir() {
        a |= FILE_ATTRIBUTE_DIRECTORY;
    }
    if m.permissions().readonly() {
        a |= FILE_ATTRIBUTE_READONLY;
    }
    if a == 0 { FILE_ATTRIBUTE_NORMAL } else { a }
}

fn filetime(t: std::io::Result<std::time::SystemTime>) -> [u32; 2] {
    let since = t
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    let ft = FILETIME_TO_UNIX + (since.as_nanos() / 100) as u64;
    [ft as u32, (ft >> 32) as u32]
}

// Handles: the standard ones, then files.

const STD_INPUT: usize = 0x10;
const STD_OUTPUT: usize = 0x14;
const STD_ERROR: usize = 0x18;

pub(super) fn close_std(handle: usize) -> bool {
    matches!(handle, STD_INPUT | STD_OUTPUT | STD_ERROR)
}

/// The host descriptor behind a handle, and whether it was opened for
/// overlapped I/O.
fn descriptor(handle: usize) -> Option<(i32, bool)> {
    match handle {
        STD_INPUT => return Some((0, false)),
        STD_OUTPUT => return Some((1, false)),
        STD_ERROR => return Some((2, false)),
        _ => {}
    }
    match table().get_mut(handle) {
        Some(Object::File { fd, overlapped, .. }) => Some((*fd, *overlapped)),
        _ => None,
    }
}

/// Closes what a file handle holds (called as its handle is closed).
pub(super) fn closed(fd: i32, delete: Option<PathBuf>) {
    // SAFETY: the descriptor this handle owned, closed once.
    unsafe { libc::close(fd) };
    if let Some(path) = delete {
        let _ = std::fs::remove_file(path);
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn GetStdHandle(which: u32) -> usize {
    match which as i32 {
        -10 => STD_INPUT,
        -11 => STD_OUTPUT,
        -12 => STD_ERROR,
        _ => INVALID_HANDLE,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFileType(handle: usize) -> u32 {
    if close_std(handle) {
        2 // FILE_TYPE_CHAR
    } else if descriptor(handle).is_some() {
        1 // FILE_TYPE_DISK
    } else {
        set_last_error(ERROR_INVALID_HANDLE);
        0
    }
}

fn open(path: Option<String>, access: u32, disposition: u32, flags: u32) -> usize {
    let Some(host) = host_of(path) else {
        return INVALID_HANDLE;
    };
    let read = access & (GENERIC_READ | GENERIC_ALL | FILE_READ_DATA) != 0;
    let write = access & (GENERIC_WRITE | GENERIC_ALL | FILE_WRITE_DATA | FILE_APPEND_DATA) != 0;
    let existed = host.exists();
    let mut o = std::fs::OpenOptions::new();
    // Metadata-only opens (attributes, directories) read.
    o.read(read || !write).write(write);
    match disposition {
        CREATE_NEW => o.create_new(true),
        CREATE_ALWAYS => o.create(true).truncate(true),
        OPEN_EXISTING => &mut o,
        OPEN_ALWAYS => o.create(true),
        TRUNCATE_EXISTING => o.truncate(true),
        _ => {
            set_last_error(ERROR_INVALID_PARAMETER);
            return INVALID_HANDLE;
        }
    };
    if !write && matches!(disposition, CREATE_NEW | CREATE_ALWAYS | OPEN_ALWAYS) {
        o.write(true);
    }
    let file = match o.open(&host) {
        Ok(f) => f,
        Err(e) => {
            // A missing folder on the way is "path", not "file", not found.
            let parent = host.parent().is_some_and(Path::is_dir);
            set_last_error(if e.kind() == std::io::ErrorKind::NotFound && !parent {
                ERROR_PATH_NOT_FOUND
            } else {
                error_of(&e)
            });
            return INVALID_HANDLE;
        }
    };
    use std::os::fd::IntoRawFd;
    let fd = file.into_raw_fd();
    let delete = (flags & FILE_FLAG_DELETE_ON_CLOSE != 0).then_some(host);
    let overlapped = flags & FILE_FLAG_OVERLAPPED != 0;
    let handle = table().add(Object::File {
        fd,
        overlapped,
        delete,
    });
    // Windows says so for these two even when they succeed.
    let said = if existed && matches!(disposition, CREATE_ALWAYS | OPEN_ALWAYS) {
        ERROR_ALREADY_EXISTS
    } else {
        0
    };
    set_last_error(said);
    handle
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateFileW(
    path: *const u16,
    access: u32,
    _share: u32,
    _security: *const c_void,
    disposition: u32,
    flags: u32,
    _template: usize,
) -> usize {
    // SAFETY: a NUL-terminated path.
    open(unsafe { wide(path) }, access, disposition, flags)
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateFileA(
    path: *const u8,
    access: u32,
    _share: u32,
    _security: *const c_void,
    disposition: u32,
    flags: u32,
    _template: usize,
) -> usize {
    // SAFETY: a NUL-terminated path.
    open(unsafe { narrow(path) }, access, disposition, flags)
}

/// `CREATEFILE2_EXTENDED_PARAMETERS`: size, attributes, flags, ...
#[allow(non_snake_case)]
pub extern "win64" fn CreateFile2(
    path: *const u16,
    access: u32,
    _share: u32,
    disposition: u32,
    parameters: *const u32,
) -> usize {
    let flags = if parameters.is_null() {
        0
    } else {
        // SAFETY: the title's parameters: dwSize, dwFileAttributes, dwFileFlags.
        unsafe { *parameters.add(1) | *parameters.add(2) }
    };
    // SAFETY: a NUL-terminated path.
    open(unsafe { wide(path) }, access, disposition, flags)
}

/// `OVERLAPPED`: status, bytes, the offset, the event.
#[repr(C)]
pub struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: usize,
}

/// Reads or writes through `handle`. With an `OVERLAPPED` the operation is
/// at its offset and completes at once (Windows allows that): its status
/// and size are in it and its event is set.
/// shortcut: no I/O completion ports; a title waiting on one waits forever.
fn transfer(
    handle: usize,
    overlapped: *mut Overlapped,
    io: impl Fn(i32, Option<i64>) -> isize,
) -> Option<usize> {
    let Some((fd, _)) = descriptor(handle) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return None;
    };
    let at = (!overlapped.is_null()).then(|| {
        // SAFETY: the title's OVERLAPPED.
        let o = unsafe { &*overlapped };
        i64::from(o.offset) | i64::from(o.offset_high) << 32
    });
    let n = io(fd, at);
    if n < 0 {
        fail(&std::io::Error::last_os_error());
        return None;
    }
    if !overlapped.is_null() {
        // SAFETY: the title's OVERLAPPED.
        let o = unsafe { &mut *overlapped };
        o.internal = 0;
        o.internal_high = n as usize;
        if o.event != 0 {
            super::sync::SetEvent(o.event);
        }
    }
    Some(n as usize)
}

#[allow(non_snake_case)]
pub extern "win64" fn ReadFile(
    handle: usize,
    buffer: *mut u8,
    length: u32,
    read: *mut u32,
    overlapped: *mut Overlapped,
) -> i32 {
    // SAFETY: the title's buffer of `length` bytes.
    let io = |fd, at: Option<i64>| unsafe {
        match at {
            Some(at) => libc::pread(fd, buffer.cast(), length as usize, at),
            None => libc::read(fd, buffer.cast(), length as usize),
        }
    };
    let Some(n) = transfer(handle, overlapped, io) else {
        return 0;
    };
    if !read.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *read = n as u32 };
    }
    if n == 0 && length > 0 && !overlapped.is_null() {
        set_last_error(ERROR_HANDLE_EOF);
        return 0;
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn WriteFile(
    handle: usize,
    buffer: *const u8,
    length: u32,
    written: *mut u32,
    overlapped: *mut Overlapped,
) -> i32 {
    // SAFETY: the title passes `length` readable bytes.
    let io = |fd, at: Option<i64>| unsafe {
        match at {
            Some(at) => libc::pwrite(fd, buffer.cast(), length as usize, at),
            None => libc::write(fd, buffer.cast(), length as usize),
        }
    };
    let Some(n) = transfer(handle, overlapped, io) else {
        return 0;
    };
    if !written.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *written = n as u32 };
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn WriteConsoleW(
    handle: usize,
    text: *const u16,
    length: u32,
    written: *mut u32,
    _: *mut c_void,
) -> i32 {
    // SAFETY: the title passes `length` UTF-16 units.
    let units = unsafe { std::slice::from_raw_parts(text, length as usize) };
    let text = String::from_utf16_lossy(units);
    let mut bytes = 0;
    let ok = WriteFile(
        handle,
        text.as_ptr(),
        text.len() as u32,
        &mut bytes,
        std::ptr::null_mut(),
    );
    if !written.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *written = length };
    }
    ok
}

#[allow(non_snake_case)]
pub extern "win64" fn GetOverlappedResult(
    _handle: usize,
    overlapped: *const Overlapped,
    bytes: *mut u32,
    _wait: i32,
) -> i32 {
    // SAFETY: the title's OVERLAPPED, completed by the call that took it.
    let o = unsafe { &*overlapped };
    if !bytes.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *bytes = o.internal_high as u32 };
    }
    (o.internal == 0).into()
}

#[allow(non_snake_case)]
pub extern "win64" fn SetFilePointerEx(
    handle: usize,
    distance: i64,
    new: *mut i64,
    method: u32,
) -> i32 {
    let Some((fd, _)) = descriptor(handle) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    let whence = match method {
        0 => libc::SEEK_SET,
        1 => libc::SEEK_CUR,
        2 => libc::SEEK_END,
        _ => {
            set_last_error(ERROR_INVALID_PARAMETER);
            return 0;
        }
    };
    // SAFETY: a descriptor of ours.
    let at = unsafe { libc::lseek(fd, distance, whence) };
    if at < 0 {
        set_last_error(131); // ERROR_NEGATIVE_SEEK
        return 0;
    }
    if !new.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *new = at };
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn SetFilePointer(handle: usize, low: i32, high: *mut i32, method: u32) -> u32 {
    let distance = if high.is_null() {
        i64::from(low)
    } else {
        // SAFETY: the title's in-out high half.
        (i64::from(unsafe { *high }) << 32) | i64::from(low as u32)
    };
    let mut at = 0;
    if SetFilePointerEx(handle, distance, &mut at, method) == 0 {
        return u32::MAX; // INVALID_SET_FILE_POINTER
    }
    if !high.is_null() {
        // SAFETY: as above.
        unsafe { *high = (at >> 32) as i32 };
    }
    set_last_error(0);
    at as u32
}

fn metadata(handle: usize) -> Option<std::fs::Metadata> {
    let Some((fd, _)) = descriptor(handle) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return None;
    };
    use std::os::fd::FromRawFd;
    // SAFETY: borrowed for the call only; ManuallyDrop keeps it open.
    let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
    file.metadata().map_err(|e| fail(&e)).ok()
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFileSizeEx(handle: usize, size: *mut i64) -> i32 {
    let Some(m) = metadata(handle) else {
        return 0;
    };
    // SAFETY: the title's out pointer.
    unsafe { *size = m.len() as i64 };
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFileSize(handle: usize, high: *mut u32) -> u32 {
    let Some(m) = metadata(handle) else {
        return u32::MAX;
    };
    if !high.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *high = (m.len() >> 32) as u32 };
    }
    m.len() as u32
}

/// `BY_HANDLE_FILE_INFORMATION`.
#[repr(C)]
pub struct HandleInformation {
    attributes: u32,
    created: [u32; 2],
    accessed: [u32; 2],
    written: [u32; 2],
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFileInformationByHandle(
    handle: usize,
    out: *mut HandleInformation,
) -> i32 {
    let Some(m) = metadata(handle) else {
        return 0;
    };
    let info = HandleInformation {
        attributes: attributes(&m),
        created: filetime(m.created()),
        accessed: filetime(m.accessed()),
        written: filetime(m.modified()),
        volume: m.dev() as u32,
        size_high: (m.len() >> 32) as u32,
        size_low: m.len() as u32,
        links: m.nlink() as u32,
        index_high: (m.ino() >> 32) as u32,
        index_low: m.ino() as u32,
    };
    // SAFETY: the title's out structure.
    unsafe { out.write(info) };
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFileInformationByHandleEx(
    handle: usize,
    class: u32,
    out: *mut u8,
    size: u32,
) -> i32 {
    let Some(m) = metadata(handle) else {
        return 0;
    };
    let ft = |t| {
        let [lo, hi] = filetime(t);
        i64::from(lo) | i64::from(hi) << 32
    };
    let mut bytes = Vec::new();
    match class {
        // FileBasicInfo: four times, then the attributes.
        0 => {
            for t in [m.created(), m.accessed(), m.modified(), m.modified()] {
                bytes.extend(ft(t).to_le_bytes());
            }
            bytes.extend(attributes(&m).to_le_bytes());
            bytes.extend([0; 4]);
        }
        // FileStandardInfo: allocation, end of file, links, delete pending, directory.
        1 => {
            bytes.extend((m.blocks() * 512).to_le_bytes());
            bytes.extend(m.len().to_le_bytes());
            bytes.extend((m.nlink() as u32).to_le_bytes());
            bytes.extend([0, m.is_dir().into(), 0, 0]);
        }
        _ => {
            set_last_error(ERROR_INVALID_PARAMETER);
            return 0;
        }
    }
    if (size as usize) < bytes.len() {
        set_last_error(24); // ERROR_BAD_LENGTH
        return 0;
    }
    // SAFETY: the title's buffer of `size` bytes.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn FlushFileBuffers(handle: usize) -> i32 {
    match descriptor(handle) {
        // SAFETY: a descriptor of ours.
        Some((fd, _)) => (unsafe { libc::fsync(fd) } == 0).into(),
        None => {
            set_last_error(ERROR_INVALID_HANDLE);
            0
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn SetEndOfFile(handle: usize) -> i32 {
    let Some((fd, _)) = descriptor(handle) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    // SAFETY: a descriptor of ours; the file ends where its pointer is.
    let ok = unsafe { libc::ftruncate(fd, libc::lseek(fd, 0, libc::SEEK_CUR)) } == 0;
    if !ok {
        fail(&std::io::Error::last_os_error());
    }
    ok.into()
}

// Paths.

fn attributes_of(path: Option<String>) -> u32 {
    let Some(host) = host_of(path) else {
        return INVALID_FILE_ATTRIBUTES;
    };
    match std::fs::metadata(&host) {
        Ok(m) => attributes(&m),
        Err(e) => {
            fail(&e);
            INVALID_FILE_ATTRIBUTES
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFileAttributesW(path: *const u16) -> u32 {
    // SAFETY: a NUL-terminated path.
    attributes_of(unsafe { wide(path) })
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFileAttributesA(path: *const u8) -> u32 {
    // SAFETY: a NUL-terminated path.
    attributes_of(unsafe { narrow(path) })
}

/// `WIN32_FILE_ATTRIBUTE_DATA`.
#[repr(C)]
pub struct AttributeData {
    attributes: u32,
    created: [u32; 2],
    accessed: [u32; 2],
    written: [u32; 2],
    size_high: u32,
    size_low: u32,
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFileAttributesExW(
    path: *const u16,
    _level: u32,
    out: *mut AttributeData,
) -> i32 {
    // SAFETY: a NUL-terminated path.
    let Some(host) = host_of(unsafe { wide(path) }) else {
        return 0;
    };
    match std::fs::metadata(&host) {
        Ok(m) => {
            let data = AttributeData {
                attributes: attributes(&m),
                created: filetime(m.created()),
                accessed: filetime(m.accessed()),
                written: filetime(m.modified()),
                size_high: (m.len() >> 32) as u32,
                size_low: m.len() as u32,
            };
            // SAFETY: the title's out structure.
            unsafe { out.write(data) };
            1
        }
        Err(e) => {
            fail(&e);
            0
        }
    }
}

/// shortcut: attributes cannot be set, beyond read-only; the call succeeds.
#[allow(non_snake_case)]
pub extern "win64" fn SetFileAttributesW(path: *const u16, attributes: u32) -> i32 {
    // SAFETY: a NUL-terminated path.
    let Some(host) = host_of(unsafe { wide(path) }) else {
        return 0;
    };
    let Ok(m) = std::fs::metadata(&host) else {
        set_last_error(ERROR_FILE_NOT_FOUND);
        return 0;
    };
    let mut p = m.permissions();
    p.set_readonly(attributes & FILE_ATTRIBUTE_READONLY != 0);
    let _ = std::fs::set_permissions(&host, p);
    1
}

fn result(r: std::io::Result<()>) -> i32 {
    match r {
        Ok(()) => 1,
        Err(e) => {
            fail(&e);
            0
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateDirectoryW(path: *const u16, _security: *const c_void) -> i32 {
    // SAFETY: a NUL-terminated path.
    let Some(host) = host_of(unsafe { wide(path) }) else {
        return 0;
    };
    if host.exists() {
        set_last_error(ERROR_ALREADY_EXISTS);
        return 0;
    }
    if !host.parent().is_some_and(Path::is_dir) {
        set_last_error(ERROR_PATH_NOT_FOUND);
        return 0;
    }
    result(std::fs::create_dir(&host))
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateDirectoryA(path: *const u8, security: *const c_void) -> i32 {
    // SAFETY: a NUL-terminated path.
    let wide_path: Vec<u16> = unsafe { narrow(path) }
        .unwrap_or_default()
        .encode_utf16()
        .chain([0])
        .collect();
    CreateDirectoryW(wide_path.as_ptr(), security)
}

#[allow(non_snake_case)]
pub extern "win64" fn RemoveDirectoryW(path: *const u16) -> i32 {
    // SAFETY: a NUL-terminated path.
    host_of(unsafe { wide(path) }).map_or(0, |h| result(std::fs::remove_dir(h)))
}

#[allow(non_snake_case)]
pub extern "win64" fn DeleteFileW(path: *const u16) -> i32 {
    // SAFETY: a NUL-terminated path.
    host_of(unsafe { wide(path) }).map_or(0, |h| result(std::fs::remove_file(h)))
}

#[allow(non_snake_case)]
pub extern "win64" fn DeleteFileA(path: *const u8) -> i32 {
    // SAFETY: a NUL-terminated path.
    host_of(unsafe { narrow(path) }).map_or(0, |h| result(std::fs::remove_file(h)))
}

#[allow(non_snake_case)]
pub extern "win64" fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32 {
    // SAFETY: NUL-terminated paths.
    let (Some(from), Some(to)) = (host_of(unsafe { wide(from) }), host_of(unsafe { wide(to) }))
    else {
        return 0;
    };
    // MOVEFILE_REPLACE_EXISTING
    if flags & 1 == 0 && to.exists() {
        set_last_error(ERROR_ALREADY_EXISTS);
        return 0;
    }
    result(std::fs::rename(from, to))
}

/// Gives a title path as Windows' path getters do (see `system::give`).
fn give_path(path: &str, out: *mut u16, size: u32) -> u32 {
    let units: Vec<u16> = path.encode_utf16().collect();
    if (units.len() as u32) < size && !out.is_null() {
        // SAFETY: the title's buffer of `size` units.
        unsafe {
            std::ptr::copy_nonoverlapping(units.as_ptr(), out, units.len());
            *out.add(units.len()) = 0;
        }
        units.len() as u32
    } else {
        units.len() as u32 + 1
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFullPathNameW(
    path: *const u16,
    size: u32,
    out: *mut u16,
    file: *mut *mut u16,
) -> u32 {
    // SAFETY: a NUL-terminated path.
    let Some((letter, parts)) = unsafe { wide(path) }.as_deref().and_then(absolute) else {
        set_last_error(ERROR_PATH_NOT_FOUND);
        return 0;
    };
    let full = title_path(letter, &parts);
    let n = give_path(&full, out, size);
    if n < size && !file.is_null() {
        let name = parts.last().map_or(0, |p| p.encode_utf16().count());
        // SAFETY: inside the buffer just written; null for a root.
        unsafe {
            *file = if name == 0 {
                std::ptr::null_mut()
            } else {
                out.add(n as usize - name)
            }
        };
    }
    n
}

#[allow(non_snake_case)]
pub extern "win64" fn GetCurrentDirectoryW(size: u32, out: *mut u16) -> u32 {
    let current = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    give_path(&current, out, size)
}

#[allow(non_snake_case)]
pub extern "win64" fn SetCurrentDirectoryW(path: *const u16) -> i32 {
    // SAFETY: a NUL-terminated path.
    let Some((letter, parts)) = unsafe { wide(path) }.as_deref().and_then(absolute) else {
        set_last_error(ERROR_PATH_NOT_FOUND);
        return 0;
    };
    let full = title_path(letter, &parts);
    if !host_path(&full).is_some_and(|h| h.is_dir()) {
        set_last_error(ERROR_PATH_NOT_FOUND);
        return 0;
    }
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = full;
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn GetTempPathW(size: u32, out: *mut u16) -> u32 {
    give_path("T:\\", out, size)
}

#[allow(non_snake_case)]
pub extern "win64" fn GetDiskFreeSpaceExW(
    path: *const u16,
    available: *mut u64,
    total: *mut u64,
    free: *mut u64,
) -> i32 {
    // SAFETY: a NUL-terminated path (null: the current drive).
    let path = unsafe { wide(path) }.unwrap_or_else(|| "\\".into());
    let Some(host) = host_of(Some(path)) else {
        return 0;
    };
    let c = std::ffi::CString::new(host.as_os_str().as_bytes()).unwrap_or_default();
    // SAFETY: plain data filled in by the call.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a valid out pointer.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        fail(&std::io::Error::last_os_error());
        return 0;
    }
    let unit = s.f_frsize as u64;
    for (out, value) in [
        (available, s.f_bavail as u64),
        (total, s.f_blocks as u64),
        (free, s.f_bfree as u64),
    ] {
        if !out.is_null() {
            // SAFETY: the title's out pointers.
            unsafe { *out = value * unit };
        }
    }
    1
}

// Finding files.

/// `WIN32_FIND_DATAW`.
#[repr(C)]
pub struct FindData {
    attributes: u32,
    created: [u32; 2],
    accessed: [u32; 2],
    written: [u32; 2],
    size_high: u32,
    size_low: u32,
    reserved: [u32; 2],
    name: [u16; 260],
    short_name: [u16; 14],
}

/// Whether `name` matches a Windows wildcard (`*`, `?`), ignoring case.
fn matches(mask: &[char], name: &[char]) -> bool {
    match (mask.first(), name.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            matches(&mask[1..], name) || (!name.is_empty() && matches(mask, &name[1..]))
        }
        (Some('?'), Some(_)) => matches(&mask[1..], &name[1..]),
        // `*.*` and `name.` match names without a dot too.
        (Some('.'), None) => mask[1..].iter().all(|&c| c == '*'),
        (Some(m), Some(n)) => m.eq_ignore_ascii_case(n) && matches(&mask[1..], &name[1..]),
        _ => false,
    }
}

fn find_data(name: &str, m: Option<&std::fs::Metadata>) -> FindData {
    let mut data = FindData {
        attributes: m.map_or(FILE_ATTRIBUTE_DIRECTORY, attributes),
        created: m.map_or([0; 2], |m| filetime(m.created())),
        accessed: m.map_or([0; 2], |m| filetime(m.accessed())),
        written: m.map_or([0; 2], |m| filetime(m.modified())),
        size_high: m.map_or(0, |m| (m.len() >> 32) as u32),
        size_low: m.map_or(0, |m| m.len() as u32),
        reserved: [0; 2],
        name: [0; 260],
        short_name: [0; 14],
    };
    for (slot, unit) in data.name.iter_mut().zip(name.encode_utf16().take(259)) {
        *slot = unit;
    }
    data
}

fn first_file(pattern: Option<String>, out: *mut FindData) -> usize {
    let Some(pattern) = pattern else {
        set_last_error(ERROR_PATH_NOT_FOUND);
        return INVALID_HANDLE;
    };
    let pattern = pattern.replace('/', "\\");
    let (folder, mask) = match pattern.rfind('\\') {
        Some(i) => (&pattern[..=i], &pattern[i + 1..]),
        None => ("", pattern.as_str()),
    };
    let Some(host) = host_of(Some(if folder.is_empty() {
        ".".into()
    } else {
        folder.into()
    })) else {
        return INVALID_HANDLE;
    };
    let entries = match std::fs::read_dir(&host) {
        Ok(e) => e,
        Err(e) => {
            set_last_error(if e.kind() == std::io::ErrorKind::NotFound {
                ERROR_PATH_NOT_FOUND
            } else {
                error_of(&e)
            });
            return INVALID_HANDLE;
        }
    };
    let mask: Vec<char> = mask.chars().collect();
    let mut found = VecDeque::new();
    // Windows lists these two in every folder below a drive's root.
    if host_path(folder).is_some_and(|h| drives().iter().all(|(_, r)| *r != h)) {
        for dot in [".", ".."] {
            if matches(&mask, &dot.chars().collect::<Vec<_>>()) {
                found.push_back(find_data(dot, None));
            }
        }
    }
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches(&mask, &name.chars().collect::<Vec<_>>()) {
            found.push_back(find_data(&name, entry.metadata().ok().as_ref()));
        }
    }
    let Some(first) = found.pop_front() else {
        set_last_error(ERROR_FILE_NOT_FOUND);
        return INVALID_HANDLE;
    };
    // SAFETY: the title's out structure.
    unsafe { out.write(first) };
    table().add(Object::Find { found })
}

#[allow(non_snake_case)]
pub extern "win64" fn FindFirstFileW(pattern: *const u16, out: *mut FindData) -> usize {
    // SAFETY: a NUL-terminated pattern.
    first_file(unsafe { wide(pattern) }, out)
}

#[allow(non_snake_case)]
pub extern "win64" fn FindFirstFileExW(
    pattern: *const u16,
    _level: u32,
    out: *mut FindData,
    _search: u32,
    _filter: *const c_void,
    _flags: u32,
) -> usize {
    FindFirstFileW(pattern, out)
}

#[allow(non_snake_case)]
pub extern "win64" fn FindNextFileW(handle: usize, out: *mut FindData) -> i32 {
    let next = match table().get_mut(handle) {
        Some(Object::Find { found }) => found.pop_front(),
        _ => {
            set_last_error(ERROR_INVALID_HANDLE);
            return 0;
        }
    };
    match next {
        Some(data) => {
            // SAFETY: the title's out structure.
            unsafe { out.write(data) };
            1
        }
        None => {
            set_last_error(ERROR_NO_MORE_FILES);
            0
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn FindClose(handle: usize) -> i32 {
    super::sync::CloseHandle(handle)
}

#[allow(non_snake_case)]
pub extern "win64" fn GetLogicalDrives() -> u32 {
    let _ = changed;
    drives()
        .iter()
        .fold(0, |m, (l, _)| m | 1 << (*l as u32 - 'A' as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    #[test]
    fn paths_stay_inside_their_drive() {
        let dir = tempfile::tempdir().unwrap();
        mount_at('Q', dir.path());
        assert_eq!(
            host_path(r"Q:\a\b.txt").unwrap(),
            dir.path().join("a").join("b.txt")
        );
        assert_eq!(
            host_path("q:/a/./c/../b").unwrap(),
            dir.path().join("a").join("b")
        );
        assert_eq!(host_path(r"\\?\Q:\x").unwrap(), dir.path().join("x"));
        assert_eq!(
            host_path(r"Q:\..\..\etc\passwd").unwrap(),
            dir.path().join("etc").join("passwd")
        );
        assert!(host_path(r"C:\Windows").is_none(), "no host drive C:");
    }

    #[test]
    fn files_are_created_written_read_and_found() {
        let dir = tempfile::tempdir().unwrap();
        mount_at('R', dir.path());
        let path = w(r"R:\saves\slot1.dat");
        let h = CreateFileW(
            path.as_ptr(),
            GENERIC_WRITE,
            0,
            std::ptr::null(),
            CREATE_ALWAYS,
            0,
            0,
        );
        assert_eq!(h, INVALID_HANDLE, "the folder does not exist");
        assert_eq!(super::super::GetLastError(), ERROR_PATH_NOT_FOUND);
        assert_eq!(
            CreateDirectoryW(w(r"R:\saves").as_ptr(), std::ptr::null()),
            1
        );
        assert_eq!(
            CreateDirectoryW(w(r"R:\saves").as_ptr(), std::ptr::null()),
            0
        );
        assert_eq!(super::super::GetLastError(), ERROR_ALREADY_EXISTS);

        let h = CreateFileW(
            path.as_ptr(),
            GENERIC_WRITE,
            0,
            std::ptr::null(),
            CREATE_ALWAYS,
            0,
            0,
        );
        assert_ne!(h, INVALID_HANDLE);
        let mut n = 0;
        assert_eq!(
            WriteFile(h, b"hello world".as_ptr(), 11, &mut n, std::ptr::null_mut()),
            1
        );
        assert_eq!(n, 11);
        assert_eq!(super::super::sync::CloseHandle(h), 1);

        let h = CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            1,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            0,
        );
        let mut size = 0;
        assert_eq!(GetFileSizeEx(h, &mut size), 1);
        assert_eq!(size, 11);
        let event = super::super::sync::CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
        let mut o = Overlapped {
            internal: 1,
            internal_high: 0,
            offset: 6,
            offset_high: 0,
            event,
        };
        let mut buf = [0u8; 16];
        assert_eq!(
            ReadFile(h, buf.as_mut_ptr(), 16, std::ptr::null_mut(), &mut o),
            1
        );
        let mut got = 0;
        assert_eq!(GetOverlappedResult(h, &o, &mut got, 1), 1);
        assert_eq!(&buf[..got as usize], b"world");
        assert_eq!(
            super::super::sync::WaitForSingleObject(event, 0),
            0,
            "its event is set"
        );
        o.offset = 11;
        assert_eq!(
            ReadFile(h, buf.as_mut_ptr(), 16, std::ptr::null_mut(), &mut o),
            0
        );
        assert_eq!(super::super::GetLastError(), ERROR_HANDLE_EOF);
        super::super::sync::CloseHandle(h);

        let mut data = std::mem::MaybeUninit::<FindData>::uninit();
        let f = FindFirstFileW(w(r"R:\saves\*.DAT").as_ptr(), data.as_mut_ptr());
        assert_ne!(f, INVALID_HANDLE);
        // SAFETY: filled in by the call.
        let data = unsafe { data.assume_init() };
        assert_eq!(String::from_utf16_lossy(&data.name[..9]), "slot1.dat");
        assert_eq!(data.size_low, 11);
        let mut next = std::mem::MaybeUninit::<FindData>::uninit();
        assert_eq!(FindNextFileW(f, next.as_mut_ptr()), 0);
        assert_eq!(super::super::GetLastError(), ERROR_NO_MORE_FILES);
        assert_eq!(FindClose(f), 1);

        assert_eq!(
            GetFileAttributesW(w(r"R:\saves").as_ptr()) & FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_DIRECTORY
        );
        assert_eq!(
            MoveFileExW(path.as_ptr(), w(r"R:\saves\slot2.dat").as_ptr(), 0),
            1
        );
        assert_eq!(GetFileAttributesW(path.as_ptr()), INVALID_FILE_ATTRIBUTES);
        assert_eq!(super::super::GetLastError(), ERROR_FILE_NOT_FOUND);
        assert_eq!(DeleteFileW(w(r"R:\saves\slot2.dat").as_ptr()), 1);
        assert_eq!(RemoveDirectoryW(w(r"R:\saves").as_ptr()), 1);
    }

    #[test]
    fn wildcards_match_as_on_windows() {
        let m = |mask: &str, name: &str| {
            matches(
                &mask.chars().collect::<Vec<_>>(),
                &name.chars().collect::<Vec<_>>(),
            )
        };
        assert!(m("*", "a.txt") && m("*.*", "noext") && m("*.TXT", "a.txt"));
        assert!(m("a?c", "abc") && !m("a?c", "ac") && !m("*.dat", "a.txt"));
    }

    #[test]
    fn full_paths_and_the_current_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("data")).unwrap();
        mount_at('S', dir.path());
        assert_eq!(SetCurrentDirectoryW(w(r"S:\data").as_ptr()), 1);
        let mut buf = [0u16; 64];
        let mut file = std::ptr::null_mut();
        let n = GetFullPathNameW(w(r"x\..\y.bin").as_ptr(), 64, buf.as_mut_ptr(), &mut file);
        assert_eq!(
            String::from_utf16_lossy(&buf[..n as usize]),
            r"S:\data\y.bin"
        );
        // SAFETY: points at the name inside `buf`.
        assert_eq!(unsafe { *file }, u16::from(b'y'));
        assert_eq!(
            GetCurrentDirectoryW(0, std::ptr::null_mut()),
            8,
            "S:\\data and its NUL"
        );
        assert_eq!(SetCurrentDirectoryW(w(r"S:\missing").as_ptr()), 0);
    }
}
