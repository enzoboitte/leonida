//! The C runtime's streams: `FILE`s over our file handles (title paths, as
//! `CreateFile` takes them), the standard streams, descriptors (`_open`),
//! and the `__stdio_common_*` entry points every `printf` and `scanf` of the
//! title goes through.

use std::ffi::{c_char, c_void};
use std::sync::Mutex;

use super::files;
use super::format::{Args, format, scan};

const EOF: i32 = -1;
const ENOENT: i32 = 2;
const EBADF: i32 = 9;
const EINVAL: i32 = 22;
const ERANGE: i32 = 34;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

fn set_errno(e: i32) {
    // SAFETY: this thread's errno.
    unsafe { *libc::__error() = e };
}

/// A stream: our handle, its state, and three words titles may read
/// (`_get_stream_buffer_pointers`).
#[repr(C)]
pub struct File {
    handle: usize,
    append: bool,
    eof: bool,
    error: bool,
    unget: Option<u8>,
    buffer: [usize; 3],
}

impl File {
    const fn new(handle: usize) -> Self {
        File {
            handle,
            append: false,
            eof: false,
            error: false,
            unget: None,
            buffer: [0; 3],
        }
    }
}

struct Standard([File; 3]);
// SAFETY: the standard streams are used from any thread, as on Windows.
unsafe impl Sync for Standard {}
static mut STANDARD: Standard = Standard([File::new(0x10), File::new(0x14), File::new(0x18)]);

#[allow(non_snake_case)]
pub extern "win64" fn __acrt_iob_func(i: u32) -> *mut File {
    // SAFETY: the three streams live for the process.
    unsafe { (&raw mut STANDARD.0[i.min(2) as usize]).cast() }
}

/// What `fopen` modes ask: access, disposition, append.
fn mode(text: &str) -> Option<(u32, u32, bool)> {
    let plus = text.contains('+');
    Some(match text.chars().next()? {
        'r' => (
            if plus {
                GENERIC_READ | GENERIC_WRITE
            } else {
                GENERIC_READ
            },
            3,
            false,
        ),
        'w' => (
            if plus {
                GENERIC_READ | GENERIC_WRITE
            } else {
                GENERIC_WRITE
            },
            if text.contains('x') { 1 } else { 2 },
            false,
        ),
        'a' => (
            if plus {
                GENERIC_READ | GENERIC_WRITE
            } else {
                GENERIC_WRITE
            },
            4,
            true,
        ),
        _ => return None,
    })
}

fn open_stream(path: Option<String>, mode_text: Option<String>) -> *mut File {
    let (Some(path), Some(m)) = (path, mode_text) else {
        set_errno(EINVAL);
        return std::ptr::null_mut();
    };
    let Some((access, disposition, append)) = mode(&m) else {
        set_errno(EINVAL);
        return std::ptr::null_mut();
    };
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let handle = files::CreateFileW(
        wide.as_ptr(),
        access,
        3,
        std::ptr::null(),
        disposition,
        0,
        0,
    );
    if handle == files::INVALID_HANDLE {
        set_errno(ENOENT);
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(File {
        append,
        ..File::new(handle)
    }))
}

/// # Safety
/// `s` is null or NUL-terminated.
unsafe fn narrow(s: *const c_char) -> Option<String> {
    // SAFETY: the caller's guarantee.
    (!s.is_null()).then(|| {
        unsafe { std::ffi::CStr::from_ptr(s) }
            .to_string_lossy()
            .into_owned()
    })
}

/// # Safety
/// `s` is null or NUL-terminated UTF-16.
unsafe fn wide(s: *const u16) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: up to the terminator.
    let n = (0..).take_while(|&i| unsafe { *s.add(i) } != 0).count();
    Some(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(s, n)
    }))
}

#[allow(non_snake_case)]
pub extern "win64" fn fopen(path: *const c_char, m: *const c_char) -> *mut File {
    // SAFETY: NUL-terminated strings.
    unsafe { open_stream(narrow(path), narrow(m)) }
}

#[allow(non_snake_case)]
pub extern "win64" fn _fsopen(path: *const c_char, m: *const c_char, _share: i32) -> *mut File {
    fopen(path, m)
}

#[allow(non_snake_case)]
pub extern "win64" fn _wfopen(path: *const u16, m: *const u16) -> *mut File {
    // SAFETY: NUL-terminated strings.
    unsafe { open_stream(wide(path), wide(m)) }
}

#[allow(non_snake_case)]
pub extern "win64" fn _wfsopen(path: *const u16, m: *const u16, _share: i32) -> *mut File {
    _wfopen(path, m)
}

fn is_standard(f: *mut File) -> bool {
    (0..3).any(|i| __acrt_iob_func(i) == f)
}

#[allow(non_snake_case)]
pub extern "win64" fn fclose(f: *mut File) -> i32 {
    if f.is_null() {
        set_errno(EINVAL);
        return EOF;
    }
    if is_standard(f) {
        return 0;
    }
    // SAFETY: a stream `fopen` made, closed once.
    let f = unsafe { Box::from_raw(f) };
    super::sync::CloseHandle(f.handle);
    0
}

/// Writes all of `bytes` to the stream; false on an error.
fn put(f: &mut File, bytes: &[u8]) -> bool {
    if f.append {
        let mut at = 0;
        files::SetFilePointerEx(f.handle, 0, &mut at, 2);
    }
    let mut n = 0;
    let ok = files::WriteFile(
        f.handle,
        bytes.as_ptr(),
        bytes.len() as u32,
        &mut n,
        std::ptr::null_mut(),
    ) != 0
        && n as usize == bytes.len();
    f.error |= !ok;
    ok
}

fn put_ok(f: &mut File, s: &str) -> bool {
    put(f, s.as_bytes())
}

/// Reads up to `out.len()` bytes; how many.
fn get(f: &mut File, out: &mut [u8]) -> usize {
    let mut done = 0;
    if let Some(b) = f.unget.take()
        && !out.is_empty()
    {
        out[0] = b;
        done = 1;
    }
    while done < out.len() {
        let mut n = 0;
        let rest = &mut out[done..];
        if files::ReadFile(
            f.handle,
            rest.as_mut_ptr(),
            rest.len() as u32,
            &mut n,
            std::ptr::null_mut(),
        ) == 0
        {
            f.error = true;
            break;
        }
        if n == 0 {
            f.eof = true;
            break;
        }
        done += n as usize;
    }
    done
}

/// # Safety
/// `f` is a stream of ours.
unsafe fn stream<'a>(f: *mut File) -> Option<&'a mut File> {
    // SAFETY: the caller's guarantee.
    unsafe { f.as_mut() }
}

#[allow(non_snake_case)]
pub extern "win64" fn fwrite(
    data: *const c_void,
    size: usize,
    count: usize,
    f: *mut File,
) -> usize {
    // SAFETY: a stream of ours and `size * count` bytes of the title's.
    let (Some(f), bytes) = (unsafe { stream(f) }, size.saturating_mul(count)) else {
        return 0;
    };
    if bytes == 0 {
        return 0;
    }
    // SAFETY: as above.
    let data = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), bytes) };
    if put(f, data) { count } else { 0 }
}

#[allow(non_snake_case)]
pub extern "win64" fn fread(out: *mut c_void, size: usize, count: usize, f: *mut File) -> usize {
    // SAFETY: a stream of ours and the title's buffer of `size * count` bytes.
    let (Some(f), bytes) = (unsafe { stream(f) }, size.saturating_mul(count)) else {
        return 0;
    };
    if bytes == 0 {
        return 0;
    }
    // SAFETY: as above.
    let out = unsafe { std::slice::from_raw_parts_mut(out.cast::<u8>(), bytes) };
    get(f, out) / size
}

#[allow(non_snake_case)]
pub extern "win64" fn fputc(c: i32, f: *mut File) -> i32 {
    // SAFETY: a stream of ours.
    match unsafe { stream(f) }.map(|f| put(f, &[c as u8])) {
        Some(true) => c & 0xFF,
        _ => EOF,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn fputs(s: *const c_char, f: *mut File) -> i32 {
    // SAFETY: a NUL-terminated string and a stream of ours.
    match (unsafe { narrow(s) }, unsafe { stream(f) }) {
        (Some(s), Some(f)) => {
            if put_ok(f, &s) {
                0
            } else {
                EOF
            }
        }
        _ => EOF,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn fgetc(f: *mut File) -> i32 {
    // SAFETY: a stream of ours.
    let Some(f) = (unsafe { stream(f) }) else {
        return EOF;
    };
    let mut b = [0u8];
    if get(f, &mut b) == 1 {
        b[0].into()
    } else {
        EOF
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn ungetc(c: i32, f: *mut File) -> i32 {
    // SAFETY: a stream of ours.
    match unsafe { stream(f) } {
        Some(f) if c != EOF => {
            f.unget = Some(c as u8);
            f.eof = false;
            c & 0xFF
        }
        _ => EOF,
    }
}

/// shortcut: wide characters go through the stream as UTF-8 text (binary
/// UTF-16 streams are not told apart).
#[allow(non_snake_case)]
pub extern "win64" fn fputwc(c: u16, f: *mut File) -> u16 {
    let s = char::from_u32(c.into()).unwrap_or('\u{FFFD}').to_string();
    // SAFETY: a stream of ours.
    match unsafe { stream(f) }.map(|f| put(f, s.as_bytes())) {
        Some(true) => c,
        _ => 0xFFFF,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn fgetwc(f: *mut File) -> u16 {
    match fgetc(f) {
        EOF => 0xFFFF,
        c => c as u16,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn ungetwc(c: u16, f: *mut File) -> u16 {
    if c == 0xFFFF || ungetc(i32::from(c as u8), f) == EOF {
        0xFFFF
    } else {
        c
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn _fseeki64(f: *mut File, offset: i64, origin: i32) -> i32 {
    // SAFETY: a stream of ours.
    let Some(f) = (unsafe { stream(f) }) else {
        set_errno(EINVAL);
        return -1;
    };
    f.unget = None;
    f.eof = false;
    let mut at = 0;
    if files::SetFilePointerEx(f.handle, offset, &mut at, origin as u32) == 0 {
        set_errno(EINVAL);
        return -1;
    }
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn fseek(f: *mut File, offset: i32, origin: i32) -> i32 {
    _fseeki64(f, offset.into(), origin)
}

#[allow(non_snake_case)]
pub extern "win64" fn _ftelli64(f: *mut File) -> i64 {
    // SAFETY: a stream of ours.
    let Some(f) = (unsafe { stream(f) }) else {
        set_errno(EINVAL);
        return -1;
    };
    let mut at = 0;
    if files::SetFilePointerEx(f.handle, 0, &mut at, 1) == 0 {
        return -1;
    }
    at - i64::from(f.unget.is_some())
}

#[allow(non_snake_case)]
pub extern "win64" fn ftell(f: *mut File) -> i32 {
    match _ftelli64(f) {
        at if at > i64::from(i32::MAX) => {
            set_errno(EINVAL);
            -1
        }
        at => at as i32,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn rewind(f: *mut File) {
    _fseeki64(f, 0, 0);
    // SAFETY: a stream of ours.
    if let Some(f) = unsafe { stream(f) } {
        f.error = false;
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn fgetpos(f: *mut File, pos: *mut i64) -> i32 {
    let at = _ftelli64(f);
    if at < 0 {
        return -1;
    }
    // SAFETY: the title's fpos_t.
    unsafe { *pos = at };
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn fsetpos(f: *mut File, pos: *const i64) -> i32 {
    // SAFETY: the title's fpos_t.
    _fseeki64(f, unsafe { *pos }, 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn fflush(_f: *mut File) -> i32 {
    0 // nothing is buffered here
}

#[allow(non_snake_case)]
pub extern "win64" fn ferror(f: *mut File) -> i32 {
    // SAFETY: a stream of ours.
    unsafe { stream(f) }.is_some_and(|f| f.error).into()
}

#[allow(non_snake_case)]
pub extern "win64" fn setvbuf(_: *mut File, _: *mut c_char, _: i32, _: usize) -> i32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn _get_stream_buffer_pointers(
    f: *mut File,
    base: *mut *mut usize,
    ptr: *mut *mut usize,
    count: *mut *mut usize,
) -> i32 {
    // SAFETY: a stream of ours: its three words stand for its buffer.
    let Some(f) = (unsafe { stream(f) }) else {
        return EINVAL;
    };
    let words = f.buffer.as_mut_ptr();
    // SAFETY: the title's out pointers, each optional.
    unsafe {
        for (out, k) in [(base, 0), (ptr, 1), (count, 2)] {
            if !out.is_null() {
                *out = words.add(k);
            }
        }
    }
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn _set_fmode(_: i32) -> i32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn __p__commode() -> *mut i32 {
    static mut COMMODE: i32 = 0;
    &raw mut COMMODE
}

// Descriptors: 0 to 2 are the standard handles, the rest index handles.

static DESCRIPTORS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn handle_of(fd: i32) -> Option<usize> {
    match fd {
        0..=2 => Some(0x10 + 4 * fd as usize),
        _ => DESCRIPTORS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get((fd - 3) as usize)
            .copied()
            .filter(|&h| h != 0),
    }
}

fn open_descriptor(path: Option<String>, flags: i32) -> i32 {
    let Some(path) = path else {
        set_errno(EINVAL);
        return -1;
    };
    let access = match flags & 3 {
        0 => GENERIC_READ,
        1 => GENERIC_WRITE,
        _ => GENERIC_READ | GENERIC_WRITE,
    };
    let disposition = match (flags & 0x100 != 0, flags & 0x200 != 0, flags & 0x400 != 0) {
        (true, _, true) => 1,
        (true, true, _) => 2,
        (true, false, _) => 4,
        (false, true, _) => 5,
        _ => 3,
    };
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let handle = files::CreateFileW(
        wide.as_ptr(),
        access,
        3,
        std::ptr::null(),
        disposition,
        0,
        0,
    );
    if handle == files::INVALID_HANDLE {
        set_errno(ENOENT);
        return -1;
    }
    let mut fds = DESCRIPTORS.lock().unwrap_or_else(|e| e.into_inner());
    fds.push(handle);
    (fds.len() + 2) as i32
}

#[allow(non_snake_case)]
pub extern "win64" fn _open(path: *const c_char, flags: i32, _mode: i32) -> i32 {
    // SAFETY: a NUL-terminated path.
    open_descriptor(unsafe { narrow(path) }, flags)
}

#[allow(non_snake_case)]
pub extern "win64" fn _wopen(path: *const u16, flags: i32, _mode: i32) -> i32 {
    // SAFETY: a NUL-terminated path.
    open_descriptor(unsafe { wide(path) }, flags)
}

#[allow(non_snake_case)]
pub extern "win64" fn _read(fd: i32, out: *mut c_void, count: u32) -> i32 {
    let Some(h) = handle_of(fd) else {
        set_errno(EBADF);
        return -1;
    };
    let mut n = 0;
    if files::ReadFile(h, out.cast(), count, &mut n, std::ptr::null_mut()) == 0 {
        return -1;
    }
    n as i32
}

#[allow(non_snake_case)]
pub extern "win64" fn _write(fd: i32, data: *const c_void, count: u32) -> i32 {
    let Some(h) = handle_of(fd) else {
        set_errno(EBADF);
        return -1;
    };
    let mut n = 0;
    if files::WriteFile(h, data.cast(), count, &mut n, std::ptr::null_mut()) == 0 {
        return -1;
    }
    n as i32
}

#[allow(non_snake_case)]
pub extern "win64" fn _lseeki64(fd: i32, offset: i64, origin: i32) -> i64 {
    let Some(h) = handle_of(fd) else {
        set_errno(EBADF);
        return -1;
    };
    let mut at = 0;
    if files::SetFilePointerEx(h, offset, &mut at, origin as u32) == 0 {
        return -1;
    }
    at
}

#[allow(non_snake_case)]
pub extern "win64" fn _close(fd: i32) -> i32 {
    if fd < 3 {
        return 0;
    }
    let mut fds = DESCRIPTORS.lock().unwrap_or_else(|e| e.into_inner());
    match fds.get_mut((fd - 3) as usize) {
        Some(h) if *h != 0 => {
            super::sync::CloseHandle(std::mem::take(h));
            0
        }
        _ => {
            set_errno(EBADF);
            -1
        }
    }
}

// The formatted entry points.

/// `_CRT_INTERNAL_PRINTF_STANDARD_SNPRINTF_BEHAVIOR`: on truncation, return
/// the length the text needs (C99) instead of -1.
const STANDARD_SNPRINTF: u64 = 2;

fn text(format_at: *const u8, wide_format: bool, va: *const u64) -> String {
    // SAFETY: the title's format and va_list.
    unsafe { format(format_at, wide_format, &mut Args::new(va)) }
}

#[allow(non_snake_case)]
pub extern "win64" fn __stdio_common_vfprintf(
    _: u64,
    f: *mut File,
    fmt: *const c_char,
    _: usize,
    va: *const u64,
) -> i32 {
    let s = text(fmt.cast(), false, va);
    // SAFETY: a stream of ours.
    match unsafe { stream(f) }.map(|f| put(f, s.as_bytes())) {
        Some(true) => s.len() as i32,
        _ => -1,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn __stdio_common_vfwprintf(
    _: u64,
    f: *mut File,
    fmt: *const u16,
    _: usize,
    va: *const u64,
) -> i32 {
    let s = text(fmt.cast(), true, va);
    // SAFETY: a stream of ours.
    match unsafe { stream(f) }.map(|f| put(f, s.as_bytes())) {
        Some(true) => s.encode_utf16().count() as i32,
        _ => -1,
    }
}

/// Copies `units` into `out` of `count` units as the `vsprintf` family
/// does: the length on success; when it does not fit, `standard` gives the
/// length it needs (after writing what fits and a NUL), else -1.
fn copy_out<T: Copy + Default>(units: &[T], out: *mut T, count: usize, standard: bool) -> i32 {
    if out.is_null() {
        return units.len() as i32;
    }
    let n = units.len();
    // SAFETY: the title's buffer of `count` units.
    unsafe {
        if n < count {
            std::ptr::copy_nonoverlapping(units.as_ptr(), out, n);
            *out.add(n) = T::default();
            return n as i32;
        }
        if count > 0 {
            std::ptr::copy_nonoverlapping(units.as_ptr(), out, count - 1);
            *out.add(count - 1) = T::default();
        }
    }
    if standard { n as i32 } else { -1 }
}

#[allow(non_snake_case)]
pub extern "win64" fn __stdio_common_vsprintf(
    options: u64,
    out: *mut u8,
    count: usize,
    fmt: *const c_char,
    _: usize,
    va: *const u64,
) -> i32 {
    let s = text(fmt.cast(), false, va);
    copy_out(s.as_bytes(), out, count, options & STANDARD_SNPRINTF != 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn __stdio_common_vswprintf(
    options: u64,
    out: *mut u16,
    count: usize,
    fmt: *const u16,
    _: usize,
    va: *const u64,
) -> i32 {
    let s: Vec<u16> = text(fmt.cast(), true, va).encode_utf16().collect();
    copy_out(&s, out, count, options & STANDARD_SNPRINTF != 0)
}

/// The `_s` forms: an empty buffer and `ERANGE` when the text does not fit.
fn copy_out_s<T: Copy + Default>(units: &[T], out: *mut T, count: usize) -> i32 {
    if out.is_null() || count == 0 {
        set_errno(EINVAL);
        return -1;
    }
    if units.len() >= count {
        // SAFETY: at least one unit.
        unsafe { *out = T::default() };
        set_errno(ERANGE);
        return -1;
    }
    copy_out(units, out, count, false)
}

#[allow(non_snake_case)]
pub extern "win64" fn __stdio_common_vsprintf_s(
    _: u64,
    out: *mut u8,
    count: usize,
    fmt: *const c_char,
    _: usize,
    va: *const u64,
) -> i32 {
    copy_out_s(text(fmt.cast(), false, va).as_bytes(), out, count)
}

#[allow(non_snake_case)]
pub extern "win64" fn __stdio_common_vswprintf_s(
    _: u64,
    out: *mut u16,
    count: usize,
    fmt: *const u16,
    _: usize,
    va: *const u64,
) -> i32 {
    let s: Vec<u16> = text(fmt.cast(), true, va).encode_utf16().collect();
    copy_out_s(&s, out, count)
}

/// `_TRUNCATE` (`usize::MAX`) as the most: what fits is kept, and -1 says
/// it was cut.
fn copy_out_n<T: Copy + Default>(units: &[T], out: *mut T, size: usize, most: usize) -> i32 {
    if most == usize::MAX {
        let r = copy_out(units, out, size, false);
        return if units.len() >= size { -1 } else { r };
    }
    let units = &units[..units.len().min(most)];
    copy_out_s(units, out, size)
}

#[allow(non_snake_case)]
pub extern "win64" fn __stdio_common_vsnprintf_s(
    _: u64,
    out: *mut u8,
    size: usize,
    most: usize,
    fmt: *const c_char,
    _: usize,
    va: *const u64,
) -> i32 {
    copy_out_n(text(fmt.cast(), false, va).as_bytes(), out, size, most)
}

#[allow(non_snake_case)]
pub extern "win64" fn __stdio_common_vsnwprintf_s(
    _: u64,
    out: *mut u16,
    size: usize,
    most: usize,
    fmt: *const u16,
    _: usize,
    va: *const u64,
) -> i32 {
    let s: Vec<u16> = text(fmt.cast(), true, va).encode_utf16().collect();
    copy_out_n(&s, out, size, most)
}

#[allow(non_snake_case)]
pub extern "win64" fn __stdio_common_vsscanf(
    _: u64,
    input: *const c_char,
    length: usize,
    fmt: *const c_char,
    _: usize,
    va: *const u64,
) -> i32 {
    // SAFETY: the title's input, `length` bytes or NUL-terminated.
    let input = unsafe {
        if length == usize::MAX {
            narrow(input).unwrap_or_default()
        } else {
            String::from_utf8_lossy(std::slice::from_raw_parts(input.cast::<u8>(), length))
                .into_owned()
        }
    };
    // SAFETY: the title's format and va_list.
    unsafe { scan(&input, fmt.cast(), false, &mut Args::new(va)) }
}

#[allow(non_snake_case)]
pub extern "win64" fn __stdio_common_vswscanf(
    _: u64,
    input: *const u16,
    length: usize,
    fmt: *const u16,
    _: usize,
    va: *const u64,
) -> i32 {
    // SAFETY: the title's input, `length` units or NUL-terminated.
    let input = unsafe {
        if length == usize::MAX {
            wide(input).unwrap_or_default()
        } else {
            String::from_utf16_lossy(std::slice::from_raw_parts(input, length))
        }
    };
    // SAFETY: the title's format and va_list.
    unsafe { scan(&input, fmt.cast(), true, &mut Args::new(va)) }
}

pub fn exports() -> Vec<crate::HleExport> {
    use crate::hle_export;
    vec![
        hle_export!(__acrt_iob_func),
        hle_export!(fopen),
        hle_export!(_fsopen),
        hle_export!(_wfopen),
        hle_export!(_wfsopen),
        hle_export!(fclose),
        hle_export!(fwrite),
        hle_export!(fread),
        hle_export!(fputc),
        hle_export!(fputs),
        hle_export!(fgetc),
        hle_export!(ungetc),
        hle_export!(fputwc),
        hle_export!(fgetwc),
        hle_export!(ungetwc),
        hle_export!(_fseeki64),
        hle_export!(fseek),
        hle_export!(_ftelli64),
        hle_export!(ftell),
        hle_export!(rewind),
        hle_export!(fgetpos),
        hle_export!(fsetpos),
        hle_export!(fflush),
        hle_export!(ferror),
        hle_export!(setvbuf),
        hle_export!(_get_stream_buffer_pointers),
        hle_export!(_set_fmode),
        hle_export!(__p__commode),
        hle_export!(_open),
        hle_export!(_wopen),
        hle_export!(_read),
        hle_export!(_write),
        hle_export!(_lseeki64),
        hle_export!(_close),
        hle_export!(__stdio_common_vfprintf),
        hle_export!(__stdio_common_vfwprintf),
        hle_export!(__stdio_common_vsprintf),
        hle_export!(__stdio_common_vswprintf),
        hle_export!(__stdio_common_vsprintf_s),
        hle_export!(__stdio_common_vswprintf_s),
        hle_export!(__stdio_common_vsnprintf_s),
        hle_export!(__stdio_common_vsnwprintf_s),
        hle_export!(__stdio_common_vsscanf),
        hle_export!(__stdio_common_vswscanf),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_write_seek_and_read_files() {
        let dir = tempfile::tempdir().unwrap();
        files::mount_at('U', dir.path());
        let f = fopen(c"U:\\log.txt".as_ptr(), c"w+".as_ptr());
        assert!(!f.is_null());
        assert_eq!(fwrite(b"hello".as_ptr().cast(), 1, 5, f), 5);
        assert_eq!(ftell(f), 5);
        rewind(f);
        let mut buf = [0u8; 8];
        assert_eq!(fread(buf.as_mut_ptr().cast(), 1, 8, f), 5);
        assert_eq!(&buf[..5], b"hello");
        assert_eq!(fgetc(f), EOF);
        assert_eq!(fclose(f), 0);
        assert!(fopen(c"U:\\missing.txt".as_ptr(), c"r".as_ptr()).is_null());
    }

    #[test]
    fn sprintf_truncates_as_microsoft_s_does() {
        let slots = [42u64];
        let mut out = [0u8; 4];
        let fmt = c"n=%d!";
        let legacy =
            __stdio_common_vsprintf(0, out.as_mut_ptr(), 4, fmt.as_ptr(), 0, slots.as_ptr());
        assert_eq!((legacy, &out), (-1, b"n=4\0"));
        let c99 = __stdio_common_vsprintf(
            STANDARD_SNPRINTF,
            out.as_mut_ptr(),
            4,
            fmt.as_ptr(),
            0,
            slots.as_ptr(),
        );
        assert_eq!(c99, 5);
        assert_eq!(
            __stdio_common_vsprintf(0, std::ptr::null_mut(), 0, fmt.as_ptr(), 0, slots.as_ptr()),
            5
        );
        let s = __stdio_common_vsprintf_s(0, out.as_mut_ptr(), 4, fmt.as_ptr(), 0, slots.as_ptr());
        assert_eq!((s, out[0]), (-1, 0));
        let t = __stdio_common_vsnprintf_s(
            0,
            out.as_mut_ptr(),
            4,
            usize::MAX,
            fmt.as_ptr(),
            0,
            slots.as_ptr(),
        );
        assert_eq!((t, &out), (-1, b"n=4\0"));
    }
}
