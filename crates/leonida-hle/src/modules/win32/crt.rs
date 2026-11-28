//! Our own C runtime (`ucrtbase.dll` and its `api-ms-win-crt-*` sets) on
//! macOS, for when Microsoft's is not supplied: on a PC Windows gives it,
//! on a Mac nothing does. Built up from what the game calls, in the order it
//! calls it. When a real `ucrtbase.dll` is mapped, the runner gives it every
//! function it has, so two runtimes never share a heap or a table.
//!
//! The process start-up first: exit tables, initialiser tables, arguments,
//! the environment, errno, threads; then the heap.

use std::ffi::{c_char, c_void};
use std::sync::Mutex;

use crate::{HleModule, hle_export};

type Routine = unsafe extern "win64" fn();
type InitRoutine = unsafe extern "win64" fn() -> i32;

/// `_onexit_table_t`: first, last and end of an array of routines.
#[repr(C)]
pub struct OnExitTable {
    first: *mut usize,
    last: *mut usize,
    end: *mut usize,
}

#[allow(non_snake_case)]
pub extern "win64" fn _initialize_onexit_table(table: *mut OnExitTable) -> i32 {
    if table.is_null() {
        return -1;
    }
    // SAFETY: the title's table.
    unsafe {
        table.write(OnExitTable {
            first: std::ptr::null_mut(),
            last: std::ptr::null_mut(),
            end: std::ptr::null_mut(),
        })
    };
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn _register_onexit_function(table: *mut OnExitTable, routine: usize) -> i32 {
    if table.is_null() {
        return -1;
    }
    // SAFETY: the title's table, its array ours (made by realloc below).
    unsafe {
        let t = &mut *table;
        if t.last == t.end {
            let used = if t.first.is_null() {
                0
            } else {
                t.last.offset_from(t.first) as usize
            };
            let room = (used * 2).max(32);
            let grown = libc::realloc(t.first.cast(), room * size_of::<usize>()).cast::<usize>();
            if grown.is_null() {
                return -1;
            }
            t.first = grown;
            t.last = grown.add(used);
            t.end = grown.add(room);
        }
        t.last.write(routine);
        t.last = t.last.add(1);
    }
    0
}

/// Runs the table's routines last to first, then empties it.
#[allow(non_snake_case)]
pub extern "win64" fn _execute_onexit_table(table: *mut OnExitTable) -> i32 {
    if table.is_null() {
        return -1;
    }
    // SAFETY: the title's table; it is taken first, so a routine that
    // registers another does not run it in this pass.
    unsafe {
        let t = std::ptr::replace(
            table,
            OnExitTable {
                first: std::ptr::null_mut(),
                last: std::ptr::null_mut(),
                end: std::ptr::null_mut(),
            },
        );
        let mut at = t.last;
        while !t.first.is_null() && at > t.first {
            at = at.sub(1);
            let routine: Routine = std::mem::transmute(at.read());
            routine();
        }
        libc::free(t.first.cast());
    }
    0
}

/// The process's own table (`atexit` and `_crt_atexit`).
struct Global(OnExitTable);
// SAFETY: only touched under its lock.
unsafe impl Send for Global {}
static AT_EXIT: Mutex<Global> = Mutex::new(Global(OnExitTable {
    first: std::ptr::null_mut(),
    last: std::ptr::null_mut(),
    end: std::ptr::null_mut(),
}));

#[allow(non_snake_case)]
pub extern "win64" fn _crt_atexit(routine: usize) -> i32 {
    let mut g = AT_EXIT.lock().unwrap_or_else(|e| e.into_inner());
    _register_onexit_function(&mut g.0, routine)
}

/// shortcut: quick-exit routines are accepted and never run (nothing here
/// calls `quick_exit`).
#[allow(non_snake_case)]
pub extern "win64" fn _crt_at_quick_exit(_: usize) -> i32 {
    0
}

fn run_at_exit() {
    let mut table = {
        let mut g = AT_EXIT.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::replace(
            &mut g.0,
            OnExitTable {
                first: std::ptr::null_mut(),
                last: std::ptr::null_mut(),
                end: std::ptr::null_mut(),
            },
        )
    };
    _execute_onexit_table(&mut table);
}

#[allow(non_snake_case)]
pub extern "win64" fn _cexit() {
    run_at_exit();
}

#[allow(non_snake_case)]
pub extern "win64" fn _c_exit() {}

#[allow(non_snake_case)]
pub extern "win64" fn _exit(code: i32) -> ! {
    std::process::exit(code)
}

/// Calls every routine between `first` and `last` (nulls are skipped).
#[allow(non_snake_case)]
pub extern "win64" fn _initterm(first: *const usize, last: *const usize) {
    let mut at = first;
    while at < last {
        // SAFETY: the title's table of routines.
        let routine = unsafe { at.read() };
        if routine != 0 {
            // SAFETY: a routine of the title's.
            unsafe { std::mem::transmute::<usize, Routine>(routine)() };
        }
        // SAFETY: inside the table.
        at = unsafe { at.add(1) };
    }
}

/// As [`_initterm`], stopping at the first routine that fails.
#[allow(non_snake_case)]
pub extern "win64" fn _initterm_e(first: *const usize, last: *const usize) -> i32 {
    let mut at = first;
    while at < last {
        // SAFETY: the title's table of routines.
        let routine = unsafe { at.read() };
        if routine != 0 {
            // SAFETY: a routine of the title's.
            let r = unsafe { std::mem::transmute::<usize, InitRoutine>(routine)() };
            if r != 0 {
                tracing::warn!(
                    at = %leonida_loader::locate_address(routine),
                    result = r,
                    "a C initialiser of the title failed"
                );
                return r;
            }
        }
        // SAFETY: inside the table.
        at = unsafe { at.add(1) };
    }
    0
}

/// The C runtime's own exception filter: nothing for it to handle here.
#[allow(non_snake_case)]
pub extern "win64" fn _seh_filter_exe(_: u32, _: *const c_void) -> i32 {
    0 // EXCEPTION_CONTINUE_SEARCH
}

#[allow(non_snake_case)]
pub extern "win64" fn _seh_filter_dll(_: u32, _: *const c_void) -> i32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn _set_app_type(_: i32) {}

/// shortcut: thread-local exit callbacks (TLS destructors of the exe) are
/// not called at thread exit.
#[allow(non_snake_case)]
pub extern "win64" fn _register_thread_local_exe_atexit_callback(_: usize) {}

// Arguments and environment, narrow, from the title's command line.

struct Narrow {
    _strings: Vec<std::ffi::CString>,
    pointers: Vec<*mut c_char>,
}
// SAFETY: built once, then only read.
unsafe impl Send for Narrow {}
unsafe impl Sync for Narrow {}

fn argv() -> &'static Narrow {
    static ARGV: std::sync::OnceLock<Narrow> = std::sync::OnceLock::new();
    ARGV.get_or_init(|| {
        // SAFETY: our command line, NUL-terminated and kept.
        let line = unsafe { std::ffi::CStr::from_ptr(super::system::GetCommandLineA().cast()) };
        narrow(split(&line.to_string_lossy()))
    })
}

fn environment() -> &'static Narrow {
    static ENV: std::sync::OnceLock<Narrow> = std::sync::OnceLock::new();
    ENV.get_or_init(|| narrow(std::env::vars().map(|(k, v)| format!("{k}={v}")).collect()))
}

fn narrow(items: Vec<String>) -> Narrow {
    let strings: Vec<_> = items
        .into_iter()
        .filter_map(|s| std::ffi::CString::new(s).ok())
        .collect();
    let mut pointers: Vec<*mut c_char> = strings.iter().map(|s| s.as_ptr().cast_mut()).collect();
    pointers.push(std::ptr::null_mut());
    Narrow {
        _strings: strings,
        pointers,
    }
}

/// A command line split as the C runtime splits it: on spaces, with quotes
/// grouping (backslash rules kept simple: `\"` is a quote).
fn split(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
                any = true;
            }
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' | '\t' if !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

static ARGC: Mutex<i32> = Mutex::new(0);

#[allow(non_snake_case)]
pub extern "win64" fn _configure_narrow_argv(_mode: i32) -> i32 {
    *ARGC.lock().unwrap_or_else(|e| e.into_inner()) = (argv().pointers.len() - 1) as i32;
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn __p___argc() -> *mut i32 {
    static mut COUNT: i32 = 0;
    // SAFETY: a static of ours the title reads; set before it is handed out.
    unsafe {
        COUNT = (argv().pointers.len() - 1) as i32;
        &raw mut COUNT
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn __p___argv() -> *mut *const *mut c_char {
    static mut ARGV: *const *mut c_char = std::ptr::null();
    // SAFETY: a static of ours the title reads.
    unsafe {
        ARGV = argv().pointers.as_ptr();
        &raw mut ARGV
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn _initialize_narrow_environment() -> i32 {
    let _ = environment();
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn _get_initial_narrow_environment() -> *const *mut c_char {
    environment().pointers.as_ptr()
}

/// The command line after the program's name, as `WinMain` gets it.
#[allow(non_snake_case)]
pub extern "win64" fn _get_narrow_winmain_command_line() -> *const c_char {
    let line = super::system::GetCommandLineA();
    // SAFETY: our NUL-terminated command line.
    let bytes = unsafe { std::ffi::CStr::from_ptr(line.cast()) }.to_bytes();
    let mut i = 0;
    let quoted = bytes.first() == Some(&b'"');
    if quoted {
        i = 1 + bytes[1..]
            .iter()
            .position(|&b| b == b'"')
            .map_or(bytes.len() - 1, |p| p + 1);
    } else {
        while i < bytes.len() && bytes[i] != b' ' {
            i += 1;
        }
    }
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    // SAFETY: inside the command line, which stays.
    unsafe { line.add(i).cast() }
}

// errno: the host's, per thread (the low codes the title tests agree).

#[allow(non_snake_case)]
pub extern "win64" fn _errno() -> *mut i32 {
    // SAFETY: the calling thread's errno.
    unsafe { libc::__error() }
}

#[allow(non_snake_case)]
pub extern "win64" fn _get_errno(out: *mut i32) -> i32 {
    // SAFETY: the title's out pointer; this thread's errno.
    unsafe { *out = *libc::__error() };
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn _set_errno(value: i32) -> i32 {
    // SAFETY: this thread's errno.
    unsafe { *libc::__error() = value };
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn strerror(n: i32) -> *const c_char {
    // SAFETY: the host's message table, static.
    unsafe { libc::strerror(n) }
}

/// An invalid argument to a C runtime function ends the title, as a
/// release build of the console's runtime does by default.
#[allow(non_snake_case)]
pub extern "win64" fn _invalid_parameter_noinfo() {
    tracing::warn!("the title passed an invalid parameter to the C runtime");
}

#[allow(non_snake_case)]
pub extern "win64" fn _invoke_watson(
    _: *const u16,
    _: *const u16,
    _: *const u16,
    _: u32,
    _: usize,
) -> ! {
    eprintln!("Leonida Legacy: the title asked to end (_invoke_watson)");
    std::process::abort()
}

#[allow(non_snake_case)]
pub extern "win64" fn _set_invalid_parameter_handler(_: usize) -> usize {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn _set_new_handler(_: usize) -> usize {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn _wassert(message: *const u16, file: *const u16, line: u32) -> ! {
    let text = |s: *const u16| {
        // SAFETY: NUL-terminated strings of the title's.
        let n = (0..).take_while(|&i| unsafe { *s.add(i) } != 0).count();
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(s, n) })
    };
    eprintln!(
        "Leonida Legacy: assertion failed in the title: {} ({}:{line})",
        text(message),
        text(file)
    );
    std::process::abort()
}

/// Threads of the C runtime: `CreateThread` underneath, as on Windows.
#[allow(non_snake_case)]
pub extern "win64" fn _beginthreadex(
    security: *const c_void,
    stack: u32,
    start: usize,
    parameter: usize,
    flags: u32,
    id: *mut u32,
) -> usize {
    super::thread::CreateThread(security, stack as usize, start, parameter, flags, id)
}

#[allow(non_snake_case)]
pub extern "win64" fn _endthreadex(code: u32) -> ! {
    super::thread::ExitThread(code)
}

// The heap: the host's allocator, which every caller shares.

#[allow(non_snake_case)]
pub extern "win64" fn malloc(size: usize) -> *mut c_void {
    // SAFETY: plain allocation.
    unsafe { libc::malloc(size) }
}

#[allow(non_snake_case)]
pub extern "win64" fn calloc(count: usize, size: usize) -> *mut c_void {
    // SAFETY: plain allocation.
    unsafe { libc::calloc(count, size) }
}

#[allow(non_snake_case)]
pub extern "win64" fn realloc(block: *mut c_void, size: usize) -> *mut c_void {
    // SAFETY: a block of this heap, or null.
    unsafe { libc::realloc(block, size) }
}

#[allow(non_snake_case)]
pub extern "win64" fn free(block: *mut c_void) {
    // SAFETY: a block of this heap, or null.
    unsafe { libc::free(block) }
}

/// No new handler: a failed `operator new` is not retried.
#[allow(non_snake_case)]
pub extern "win64" fn _callnewh(_: usize) -> i32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn _set_new_mode(_: i32) -> i32 {
    0
}

pub fn module() -> HleModule {
    let mut exports = vec![
        hle_export!(_initialize_onexit_table),
        hle_export!(_register_onexit_function),
        hle_export!(_execute_onexit_table),
        hle_export!(_crt_atexit),
        hle_export!(_crt_at_quick_exit),
        hle_export!(_cexit),
        hle_export!(_c_exit),
        hle_export!(_exit),
        hle_export!(_initterm),
        hle_export!(_initterm_e),
        hle_export!(_seh_filter_exe),
        hle_export!(_seh_filter_dll),
        hle_export!(_set_app_type),
        hle_export!(_register_thread_local_exe_atexit_callback),
        hle_export!(_configure_narrow_argv),
        hle_export!(__p___argc),
        hle_export!(__p___argv),
        hle_export!(_initialize_narrow_environment),
        hle_export!(_get_initial_narrow_environment),
        hle_export!(_get_narrow_winmain_command_line),
        hle_export!(_errno),
        hle_export!(_get_errno),
        hle_export!(_set_errno),
        hle_export!(strerror),
        hle_export!(_invalid_parameter_noinfo),
        hle_export!(_invoke_watson),
        hle_export!(_set_invalid_parameter_handler),
        hle_export!(_set_new_handler),
        hle_export!(_wassert),
        hle_export!(_beginthreadex),
        hle_export!(_endthreadex),
        hle_export!(malloc),
        hle_export!(calloc),
        hle_export!(realloc),
        hle_export!(free),
        hle_export!(_callnewh),
        hle_export!(_set_new_mode),
    ];
    exports.extend(super::stdio::exports());
    exports.extend(super::libc_crt::exports());
    HleModule {
        name: "crt",
        dlls: &["ucrtbase.dll", "api-ms-win-crt-*"],
        exports,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    static ORDER: AtomicUsize = AtomicUsize::new(0);
    extern "win64" fn first() {
        ORDER.store(ORDER.load(SeqCst) * 10 + 1, SeqCst);
    }
    extern "win64" fn second() {
        ORDER.store(ORDER.load(SeqCst) * 10 + 2, SeqCst);
    }

    #[test]
    fn exit_tables_run_last_registered_first() {
        let mut t = std::mem::MaybeUninit::<OnExitTable>::uninit();
        assert_eq!(_initialize_onexit_table(t.as_mut_ptr()), 0);
        for f in [first as *const () as usize, second as *const () as usize] {
            assert_eq!(_register_onexit_function(t.as_mut_ptr(), f), 0);
        }
        assert_eq!(_execute_onexit_table(t.as_mut_ptr()), 0);
        assert_eq!(ORDER.load(SeqCst), 21, "second, then first");
    }

    #[test]
    fn initialiser_tables_skip_nulls_and_stop_on_failure() {
        static RAN: AtomicUsize = AtomicUsize::new(0);
        extern "win64" fn ok() -> i32 {
            RAN.fetch_add(1, SeqCst);
            0
        }
        extern "win64" fn fail() -> i32 {
            7
        }
        let table = [
            ok as *const () as usize,
            0,
            fail as *const () as usize,
            ok as *const () as usize,
        ];
        let r = _initterm_e(table.as_ptr(), table.as_ptr().wrapping_add(4));
        assert_eq!((r, RAN.load(SeqCst)), (7, 1));
    }

    #[test]
    fn command_lines_split_as_the_runtime_splits_them() {
        assert_eq!(
            split(r#"G:\game.exe -a "two words" \"q"#),
            ["G:\\game.exe", "-a", "two words", "\"q"]
        );
        let p = malloc(16);
        assert!(!p.is_null());
        free(realloc(p, 64));
    }
}
