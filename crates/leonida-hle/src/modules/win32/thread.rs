//! Threads, thread ids, TLS and FLS slots, and the standard handles.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};

use super::sync::{CURRENT_PROCESS, CURRENT_THREAD, Object, changed, table};
use super::{set_last_error, teb};

const STILL_ACTIVE: u32 = 259;
const CREATE_SUSPENDED: u32 = 4;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_ACCESS_DENIED: u32 = 5;
/// TEB fields: the 64 TLS slots, and the pointer to 1024 more.
const TEB_TLS_SLOTS: usize = 0x1480;
const TEB_TLS_EXPANSION: usize = 0x1780;
const TLS_SLOTS: u32 = 64;
const TLS_EXPANSION: u32 = 1024;
const TLS_OUT_OF_INDEXES: u32 = u32::MAX;
const FLS_SLOTS: u32 = 4096;

/// Thread ids are ours, as Windows' are its own: multiples of 4.
static NEXT_ID: AtomicU32 = AtomicU32::new(0x104);

thread_local! {
    static ID: Cell<u32> = const { Cell::new(0) };
    /// This thread's handle, for a thread `CreateThread` started.
    static HANDLE: Cell<usize> = const { Cell::new(0) };
    static FLS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn current_thread_id() -> u32 {
    ID.with(|id| {
        if id.get() == 0 {
            id.set(NEXT_ID.fetch_add(4, SeqCst));
        }
        id.get()
    })
}

#[allow(non_snake_case)]
pub extern "win64" fn GetCurrentThreadId() -> u32 {
    current_thread_id()
}

#[allow(non_snake_case)]
pub extern "win64" fn GetCurrentProcessId() -> u32 {
    std::process::id()
}

#[allow(non_snake_case)]
pub extern "win64" fn GetCurrentThread() -> usize {
    CURRENT_THREAD
}

#[allow(non_snake_case)]
pub extern "win64" fn GetCurrentProcess() -> usize {
    CURRENT_PROCESS
}

/// A thread made suspended waits here until resumed.
struct Gate {
    suspended: Mutex<u32>,
    resumed: Condvar,
}

static GATES: Mutex<Vec<(usize, Arc<Gate>)>> = Mutex::new(Vec::new());

fn gate_of(handle: usize) -> Option<Arc<Gate>> {
    let gates = GATES.lock().unwrap_or_else(|e| e.into_inner());
    gates
        .iter()
        .find(|(h, _)| *h == handle)
        .map(|(_, g)| g.clone())
}

struct Start {
    start: usize,
    parameter: usize,
    handle: usize,
    id: u32,
    gate: Arc<Gate>,
}

/// What every title thread runs first: its id, its suspension, its procedure.
extern "win64" fn trampoline(start: usize) -> u32 {
    // SAFETY: the Box `CreateThread` leaked for this thread alone.
    let start = unsafe { Box::from_raw(start as *mut Start) };
    ID.with(|id| id.set(start.id));
    HANDLE.with(|h| h.set(start.handle));
    {
        let mut suspended = start
            .gate
            .suspended
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        while *suspended > 0 {
            suspended = start
                .gate
                .resumed
                .wait(suspended)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
    type Procedure = unsafe extern "win64" fn(usize) -> u32;
    // SAFETY: the title's thread procedure, as it passed it to CreateThread.
    let procedure: Procedure = unsafe { std::mem::transmute(start.start) };
    // SAFETY: as above.
    let code = unsafe { procedure(start.parameter) };
    ended(start.handle, code);
    code
}

fn ended(handle: usize, code: u32) {
    if let Some(Object::Thread { ended, .. }) = table().get_mut(handle) {
        *ended = Some(code);
    }
    changed();
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateThread(
    _attributes: *const c_void,
    stack: usize,
    start: usize,
    parameter: usize,
    flags: u32,
    id_out: *mut u32,
) -> usize {
    let id = NEXT_ID.fetch_add(4, SeqCst);
    let handle = table().add(Object::Thread { id, ended: None });
    let gate = Arc::new(Gate {
        suspended: Mutex::new(u32::from(flags & CREATE_SUSPENDED != 0)),
        resumed: Condvar::new(),
    });
    GATES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((handle, gate.clone()));
    let start = Box::into_raw(Box::new(Start {
        start,
        parameter,
        handle,
        id,
        gate,
    }));
    // SAFETY: `trampoline` is a thread procedure, and owns `start`.
    match unsafe {
        leonida_loader::spawn_thread(trampoline as *const () as usize, start as usize, stack)
    } {
        Ok(_) => {
            if !id_out.is_null() {
                // SAFETY: the title's out pointer.
                unsafe { *id_out = id };
            }
            handle
        }
        Err(e) => {
            tracing::error!("CreateThread: {e}");
            // SAFETY: the thread did not start, so the Box is still ours.
            drop(unsafe { Box::from_raw(start) });
            if let Some(o) = table().get_mut(handle) {
                *o = Object::Event {
                    manual: true,
                    set: true,
                };
            }
            set_last_error(8);
            0
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn ResumeThread(handle: usize) -> u32 {
    let Some(gate) = gate_of(handle) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return u32::MAX;
    };
    let mut suspended = gate.suspended.lock().unwrap_or_else(|e| e.into_inner());
    let previous = *suspended;
    if *suspended > 0 {
        *suspended -= 1;
        gate.resumed.notify_all();
    }
    previous
}

/// shortcut: only a thread that has not started yet can be suspended; a
/// running one keeps running (the call answers as if it was suspended).
#[allow(non_snake_case)]
pub extern "win64" fn SuspendThread(handle: usize) -> u32 {
    let Some(gate) = gate_of(handle) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return u32::MAX;
    };
    let mut suspended = gate.suspended.lock().unwrap_or_else(|e| e.into_inner());
    let previous = *suspended;
    *suspended += 1;
    previous
}

/// shortcut: the thread ends for everyone waiting on it, but stays parked
/// rather than unwinding its title frames (its DLL detach notices and FLS
/// callbacks do not run).
#[allow(non_snake_case)]
pub extern "win64" fn ExitThread(code: u32) -> ! {
    let handle = HANDLE.with(Cell::get);
    if handle != 0 {
        ended(handle, code);
    }
    loop {
        std::thread::park();
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn TerminateThread(_: usize, _: u32) -> i32 {
    // Another thread cannot be stopped from outside on this host.
    set_last_error(ERROR_ACCESS_DENIED);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn GetExitCodeThread(handle: usize, code: *mut u32) -> i32 {
    let value = if handle == CURRENT_THREAD {
        STILL_ACTIVE
    } else {
        match table().get_mut(handle) {
            Some(Object::Thread { ended, .. }) => ended.unwrap_or(STILL_ACTIVE),
            _ => {
                set_last_error(ERROR_INVALID_HANDLE);
                return 0;
            }
        }
    };
    // SAFETY: the title's out pointer.
    unsafe { *code = value };
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn GetThreadId(handle: usize) -> u32 {
    if handle == CURRENT_THREAD {
        return current_thread_id();
    }
    match table().get_mut(handle) {
        Some(Object::Thread { id, .. }) => *id,
        _ => 0,
    }
}

// shortcut: priorities, affinities and names are accepted and not applied;
// the host schedules title threads as it schedules its own.
#[allow(non_snake_case)]
pub extern "win64" fn SetThreadPriority(_: usize, _: i32) -> i32 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn GetThreadPriority(_: usize) -> i32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn SetThreadAffinityMask(_: usize, _: usize) -> usize {
    usize::MAX
}

#[allow(non_snake_case)]
pub extern "win64" fn SetThreadIdealProcessor(_: usize, _: u32) -> u32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn SetThreadDescription(_: usize, _: *const u16) -> i32 {
    0 // S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn Sleep(ms: u32) {
    if ms == 0 {
        std::thread::yield_now();
    } else if ms != u32::MAX {
        std::thread::sleep(std::time::Duration::from_millis(ms.into()));
    } else {
        loop {
            std::thread::park();
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn SleepEx(ms: u32, _alertable: i32) -> u32 {
    Sleep(ms);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn SwitchToThread() -> i32 {
    std::thread::yield_now();
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn ExitProcess(code: u32) -> ! {
    std::process::exit(code as i32)
}

// TLS: slots in the TEB, where title code may read them itself.

static TLS_TAKEN: Mutex<Vec<bool>> = Mutex::new(Vec::new());

/// The TEB word slot `index` of this thread is, made if needed.
fn tls_slot(index: u32) -> Option<*mut usize> {
    let teb = teb();
    if index < TLS_SLOTS {
        // SAFETY: inside this thread's TEB.
        return Some(unsafe { teb.add(TEB_TLS_SLOTS + 8 * index as usize).cast() });
    }
    let index = index
        .checked_sub(TLS_SLOTS)
        .filter(|&i| i < TLS_EXPANSION)?;
    // SAFETY: inside this thread's TEB.
    let expansion = unsafe { teb.add(TEB_TLS_EXPANSION).cast::<*mut usize>() };
    // SAFETY: as above; the slots, once made, live as long as the thread.
    unsafe {
        if (*expansion).is_null() {
            let slots = vec![0usize; TLS_EXPANSION as usize].leak();
            *expansion = slots.as_mut_ptr();
        }
        Some((*expansion).add(index as usize))
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn TlsAlloc() -> u32 {
    let mut taken = TLS_TAKEN.lock().unwrap_or_else(|e| e.into_inner());
    taken.resize((TLS_SLOTS + TLS_EXPANSION) as usize, false);
    match taken.iter().position(|&t| !t) {
        Some(i) => {
            taken[i] = true;
            i as u32
        }
        None => TLS_OUT_OF_INDEXES,
    }
}

/// shortcut: the slot is freed, but other threads keep their value in it.
#[allow(non_snake_case)]
pub extern "win64" fn TlsFree(index: u32) -> i32 {
    let mut taken = TLS_TAKEN.lock().unwrap_or_else(|e| e.into_inner());
    match taken.get_mut(index as usize) {
        Some(t) if *t => {
            *t = false;
            if let Some(slot) = tls_slot(index) {
                // SAFETY: this thread's slot.
                unsafe { *slot = 0 };
            }
            1
        }
        _ => {
            set_last_error(87);
            0
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn TlsGetValue(index: u32) -> usize {
    match tls_slot(index) {
        Some(slot) => {
            set_last_error(0);
            // SAFETY: this thread's slot.
            unsafe { *slot }
        }
        None => {
            set_last_error(87);
            0
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn TlsSetValue(index: u32, value: usize) -> i32 {
    match tls_slot(index) {
        Some(slot) => {
            // SAFETY: this thread's slot.
            unsafe { *slot = value };
            1
        }
        None => {
            set_last_error(87);
            0
        }
    }
}

// FLS: a slot per index and thread, here. shortcut: the callbacks given to
// FlsAlloc are not called at thread exit or FlsFree.

static FLS_NEXT: AtomicU32 = AtomicU32::new(1);

#[allow(non_snake_case)]
pub extern "win64" fn FlsAlloc(_callback: usize) -> u32 {
    let index = FLS_NEXT.fetch_add(1, SeqCst);
    if index < FLS_SLOTS {
        index
    } else {
        TLS_OUT_OF_INDEXES
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn FlsFree(index: u32) -> i32 {
    FlsSetValue(index, 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn FlsGetValue(index: u32) -> usize {
    FLS.with_borrow(|f| f.get(index as usize).copied().unwrap_or(0))
}

#[allow(non_snake_case)]
pub extern "win64" fn FlsSetValue(index: u32, value: usize) -> i32 {
    if index == 0 || index >= FLS_SLOTS {
        set_last_error(87);
        return 0;
    }
    FLS.with_borrow_mut(|f| {
        if f.len() <= index as usize {
            f.resize(index as usize + 1, 0);
        }
        f[index as usize] = value;
    });
    1
}

// The standard handles: pseudo handles that write to the host's own.

const STD_INPUT: usize = 0x10;
const STD_OUTPUT: usize = 0x14;
const STD_ERROR: usize = 0x18;

pub(super) fn close_std(handle: usize) -> bool {
    matches!(handle, STD_INPUT | STD_OUTPUT | STD_ERROR)
}

#[allow(non_snake_case)]
pub extern "win64" fn GetStdHandle(which: u32) -> usize {
    match which as i32 {
        -10 => STD_INPUT,
        -11 => STD_OUTPUT,
        -12 => STD_ERROR,
        _ => usize::MAX,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFileType(handle: usize) -> u32 {
    // FILE_TYPE_CHAR for the console handles, FILE_TYPE_UNKNOWN otherwise.
    if close_std(handle) { 2 } else { 0 }
}

/// shortcut: only the standard handles are written; files need the macOS
/// port of `files`.
#[allow(non_snake_case)]
pub extern "win64" fn WriteFile(
    handle: usize,
    buffer: *const u8,
    length: u32,
    written: *mut u32,
    _overlapped: *mut c_void,
) -> i32 {
    let fd = match handle {
        STD_OUTPUT => 1,
        STD_ERROR => 2,
        _ => {
            set_last_error(ERROR_INVALID_HANDLE);
            return 0;
        }
    };
    // SAFETY: the title passes `length` readable bytes.
    let n = unsafe { libc::write(fd, buffer.cast(), length as usize) };
    if !written.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *written = n.max(0) as u32 };
    }
    (n >= 0).into()
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
    let text =
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length as usize) });
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

#[cfg(test)]
mod tests {
    use super::super::sync::{INFINITE, WaitForSingleObject};
    use super::*;

    extern "win64" fn add_one(parameter: usize) -> u32 {
        parameter as u32 + 1
    }

    #[test]
    fn threads_start_suspended_resume_and_report_their_exit_code() {
        let mut id = 0;
        let h = CreateThread(
            std::ptr::null(),
            0,
            add_one as *const () as usize,
            41,
            CREATE_SUSPENDED,
            &mut id,
        );
        assert_ne!(h, 0);
        assert_eq!(id % 4, 0);
        assert_eq!(GetThreadId(h), id);
        let mut code = 0;
        GetExitCodeThread(h, &mut code);
        assert_eq!(code, STILL_ACTIVE, "suspended");
        assert_eq!(WaitForSingleObject(h, 20), 0x102, "still suspended");
        assert_eq!(ResumeThread(h), 1);
        assert_eq!(WaitForSingleObject(h, INFINITE), 0);
        GetExitCodeThread(h, &mut code);
        assert_eq!(code, 42);
    }

    #[test]
    fn tls_slots_are_per_thread_and_in_the_teb() {
        let i = TlsAlloc();
        assert!(i < TLS_SLOTS);
        assert_eq!(TlsSetValue(i, 99), 1);
        assert_eq!(TlsGetValue(i), 99);
        // SAFETY: the slot title code would read itself.
        let in_teb = unsafe { *teb().add(TEB_TLS_SLOTS + 8 * i as usize).cast::<usize>() };
        assert_eq!(in_teb, 99);
        assert_eq!(
            std::thread::spawn(move || TlsGetValue(i)).join().unwrap(),
            0
        );
        assert_eq!(TlsFree(i), 1);

        let mut far = Vec::new();
        while far.last().is_none_or(|&f| f < TLS_SLOTS) {
            far.push(TlsAlloc());
        }
        let f = *far.last().unwrap();
        assert_eq!(TlsSetValue(f, 7), 1);
        assert_eq!(TlsGetValue(f), 7, "an expansion slot");
        for i in far {
            TlsFree(i);
        }

        let fls = FlsAlloc(0);
        assert_eq!(FlsSetValue(fls, 5), 1);
        assert_eq!(FlsGetValue(fls), 5);
        assert_eq!(
            std::thread::spawn(move || FlsGetValue(fls)).join().unwrap(),
            0
        );
    }
}
