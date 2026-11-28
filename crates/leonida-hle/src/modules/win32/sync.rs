//! Handles and waits (events, mutexes, semaphores, threads), and the locks
//! that live in title memory (critical sections, SRW locks, condition
//! variables, `WaitOnAddress`, one-time initialisation).
//!
//! shortcut: every handle object sits behind one lock, and every change
//! wakes every waiter, which then looks again. Correct, but a title with
//! many busy waiters pays for it: a waiter per object is the next step.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering::SeqCst};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::{current_thread_id, set_last_error};

pub(super) const INFINITE: u32 = u32::MAX;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x102;
const WAIT_FAILED: u32 = u32::MAX;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_NOT_OWNER: u32 = 288;
const ERROR_TOO_MANY_POSTS: u32 = 298;
const ERROR_TIMEOUT: u32 = 1460;
const MAXIMUM_WAIT_OBJECTS: u32 = 64;
/// `GetCurrentProcess()` and `GetCurrentThread()`.
pub(super) const CURRENT_PROCESS: usize = usize::MAX;
pub(super) const CURRENT_THREAD: usize = usize::MAX - 1;

pub(super) enum Object {
    Event {
        manual: bool,
        set: bool,
    },
    Mutex {
        owner: u32,
        count: u32,
    },
    Semaphore {
        count: i32,
        max: i32,
    },
    /// A thread: its id, and its exit code once it ended.
    Thread {
        id: u32,
        ended: Option<u32>,
    },
    /// An open file: its host descriptor, whether it was opened for
    /// overlapped I/O, and the host path to delete when it is closed.
    File {
        fd: i32,
        overlapped: bool,
        delete: Option<std::path::PathBuf>,
    },
    /// A file search: what it has still to give.
    Find {
        found: std::collections::VecDeque<super::files::FindData>,
    },
}

pub(super) struct Table {
    next: usize,
    objects: BTreeMap<usize, Object>,
}

static TABLE: Mutex<Table> = Mutex::new(Table {
    next: 0x100,
    objects: BTreeMap::new(),
});
static CHANGED: Condvar = Condvar::new();

pub(super) fn table() -> MutexGuard<'static, Table> {
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

impl Table {
    pub(super) fn add(&mut self, object: Object) -> usize {
        let handle = self.next;
        self.next += 4;
        self.objects.insert(handle, object);
        handle
    }

    pub(super) fn get_mut(&mut self, handle: usize) -> Option<&mut Object> {
        self.objects.get_mut(&handle)
    }

    /// Whether `handle` is signalled for this thread; takes it if `take`.
    fn ready(&mut self, handle: usize, me: u32, take: bool) -> Option<bool> {
        if handle == CURRENT_PROCESS {
            return Some(false);
        }
        let ready = match self.objects.get_mut(&handle)? {
            Object::Event { manual, set } => {
                let ready = *set;
                if ready && take && !*manual {
                    *set = false;
                }
                ready
            }
            Object::Mutex { owner, count } => {
                let ready = *owner == 0 || *owner == me;
                if ready && take {
                    *owner = me;
                    *count += 1;
                }
                ready
            }
            Object::Semaphore { count, .. } => {
                let ready = *count > 0;
                if ready && take {
                    *count -= 1;
                }
                ready
            }
            Object::Thread { ended, .. } => ended.is_some(),
            Object::File { .. } => true,
            Object::Find { .. } => false,
        };
        Some(ready)
    }
}

/// Wakes everything waiting on a handle; call after changing an object.
pub(super) fn changed() {
    CHANGED.notify_all();
}

fn deadline(ms: u32) -> Option<Instant> {
    (ms != INFINITE).then(|| Instant::now() + Duration::from_millis(ms.into()))
}

/// Waits for one of `handles` (or all of them): WAIT_OBJECT_0 + its index,
/// WAIT_TIMEOUT, or WAIT_FAILED for a handle we do not know.
pub(super) fn wait(handles: &[usize], all: bool, ms: u32) -> u32 {
    let me = current_thread_id();
    let until = deadline(ms);
    let mut t = table();
    loop {
        let mut ready = Vec::with_capacity(handles.len());
        for &h in handles {
            match t.ready(h, me, false) {
                Some(r) => ready.push(r),
                None => {
                    set_last_error(ERROR_INVALID_HANDLE);
                    return WAIT_FAILED;
                }
            }
        }
        if all && ready.iter().all(|&r| r) {
            for &h in handles {
                t.ready(h, me, true);
            }
            return WAIT_OBJECT_0;
        }
        if !all && let Some(i) = ready.iter().position(|&r| r) {
            t.ready(handles[i], me, true);
            return WAIT_OBJECT_0 + i as u32;
        }
        t = match until {
            None => CHANGED.wait(t).unwrap_or_else(|e| e.into_inner()),
            Some(until) => {
                let now = Instant::now();
                if now >= until {
                    return WAIT_TIMEOUT;
                }
                CHANGED
                    .wait_timeout(t, until - now)
                    .unwrap_or_else(|e| e.into_inner())
                    .0
            }
        };
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn WaitForSingleObject(handle: usize, ms: u32) -> u32 {
    wait(&[handle], false, ms)
}

#[allow(non_snake_case)]
pub extern "win64" fn WaitForSingleObjectEx(handle: usize, ms: u32, _alertable: i32) -> u32 {
    wait(&[handle], false, ms)
}

#[allow(non_snake_case)]
pub extern "win64" fn WaitForMultipleObjects(
    count: u32,
    handles: *const usize,
    all: i32,
    ms: u32,
) -> u32 {
    if count == 0 || count > MAXIMUM_WAIT_OBJECTS || handles.is_null() {
        set_last_error(87);
        return WAIT_FAILED;
    }
    // SAFETY: the title passes `count` handles.
    let handles = unsafe { std::slice::from_raw_parts(handles, count as usize) };
    wait(handles, all != 0, ms)
}

#[allow(non_snake_case)]
pub extern "win64" fn WaitForMultipleObjectsEx(
    count: u32,
    handles: *const usize,
    all: i32,
    ms: u32,
    _alertable: i32,
) -> u32 {
    WaitForMultipleObjects(count, handles, all, ms)
}

#[allow(non_snake_case)]
pub extern "win64" fn CloseHandle(handle: usize) -> i32 {
    if handle == CURRENT_PROCESS || handle == CURRENT_THREAD {
        return 1;
    }
    let removed = table().objects.remove(&handle);
    match removed {
        Some(Object::File { fd, delete, .. }) => super::files::closed(fd, delete),
        Some(_) => {}
        None if super::files::close_std(handle) => {}
        None => {
            set_last_error(ERROR_INVALID_HANDLE);
            return 0;
        }
    }
    1
}

fn event(manual: bool, set: bool) -> usize {
    table().add(Object::Event { manual, set })
}

// shortcut: object names are ignored: a named object is a new one, not
// the one another `Create*` of that name made.
#[allow(non_snake_case)]
pub extern "win64" fn CreateEventW(
    _: *const c_void,
    manual: i32,
    set: i32,
    _: *const u16,
) -> usize {
    event(manual != 0, set != 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateEventA(_: *const c_void, manual: i32, set: i32, _: *const u8) -> usize {
    event(manual != 0, set != 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateEventExW(_: *const c_void, _: *const u16, flags: u32, _: u32) -> usize {
    event(flags & 1 != 0, flags & 2 != 0)
}

fn set_event(handle: usize, to: bool) -> i32 {
    match table().get_mut(handle) {
        Some(Object::Event { set, .. }) => *set = to,
        _ => {
            set_last_error(ERROR_INVALID_HANDLE);
            return 0;
        }
    }
    changed();
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn SetEvent(handle: usize) -> i32 {
    set_event(handle, true)
}

#[allow(non_snake_case)]
pub extern "win64" fn ResetEvent(handle: usize) -> i32 {
    set_event(handle, false)
}

fn mutex(owned: bool) -> usize {
    let owner = if owned { current_thread_id() } else { 0 };
    table().add(Object::Mutex {
        owner,
        count: owned.into(),
    })
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateMutexW(_: *const c_void, owned: i32, _: *const u16) -> usize {
    mutex(owned != 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateMutexA(_: *const c_void, owned: i32, _: *const u8) -> usize {
    mutex(owned != 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateMutexExW(_: *const c_void, _: *const u16, flags: u32, _: u32) -> usize {
    mutex(flags & 1 != 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn ReleaseMutex(handle: usize) -> i32 {
    let me = current_thread_id();
    match table().get_mut(handle) {
        Some(Object::Mutex { owner, count }) if *owner == me => {
            *count -= 1;
            if *count == 0 {
                *owner = 0;
            }
        }
        Some(Object::Mutex { .. }) => {
            set_last_error(ERROR_NOT_OWNER);
            return 0;
        }
        _ => {
            set_last_error(ERROR_INVALID_HANDLE);
            return 0;
        }
    }
    changed();
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateSemaphoreW(
    _: *const c_void,
    count: i32,
    max: i32,
    _: *const u16,
) -> usize {
    table().add(Object::Semaphore { count, max })
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateSemaphoreExW(
    _: *const c_void,
    count: i32,
    max: i32,
    _: *const u16,
    _: u32,
    _: u32,
) -> usize {
    table().add(Object::Semaphore { count, max })
}

#[allow(non_snake_case)]
pub extern "win64" fn ReleaseSemaphore(handle: usize, release: i32, previous: *mut i32) -> i32 {
    match table().get_mut(handle) {
        Some(Object::Semaphore { count, max }) => {
            if release <= 0 || *count + release > *max {
                set_last_error(ERROR_TOO_MANY_POSTS);
                return 0;
            }
            if !previous.is_null() {
                // SAFETY: the title's out pointer.
                unsafe { *previous = *count };
            }
            *count += release;
        }
        _ => {
            set_last_error(ERROR_INVALID_HANDLE);
            return 0;
        }
    }
    changed();
    1
}

// Locks in title memory park their threads here, by address.

static PARK: Mutex<()> = Mutex::new(());
static UNPARKED: Condvar = Condvar::new();

/// Waits while `blocked()` holds, until woken at any address or `ms` pass;
/// whether it stopped holding.
fn park(blocked: impl Fn() -> bool, ms: u32) -> bool {
    let until = deadline(ms);
    let mut guard = PARK.lock().unwrap_or_else(|e| e.into_inner());
    while blocked() {
        guard = match until {
            None => UNPARKED.wait(guard).unwrap_or_else(|e| e.into_inner()),
            Some(until) => {
                let now = Instant::now();
                if now >= until {
                    return false;
                }
                let left = (until - now).min(Duration::from_millis(50));
                UNPARKED
                    .wait_timeout(guard, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0
            }
        };
    }
    true
}

/// Wakes the threads parked on any address; they look again.
fn unpark() {
    let _guard = PARK.lock().unwrap_or_else(|e| e.into_inner());
    UNPARKED.notify_all();
}

/// `RTL_CRITICAL_SECTION` (x64): the fields title code may read.
#[repr(C)]
pub struct CriticalSection {
    debug_info: usize,
    lock_count: AtomicI32,
    recursion: AtomicI32,
    owner: AtomicUsize,
    semaphore: usize,
    spin: usize,
}

#[allow(non_snake_case)]
pub extern "win64" fn InitializeCriticalSection(cs: *mut CriticalSection) {
    InitializeCriticalSectionAndSpinCount(cs, 0);
}

#[allow(non_snake_case)]
pub extern "win64" fn InitializeCriticalSectionAndSpinCount(
    cs: *mut CriticalSection,
    spin: u32,
) -> i32 {
    // SAFETY: the title's critical section.
    unsafe {
        cs.write(CriticalSection {
            debug_info: usize::MAX,
            lock_count: AtomicI32::new(-1),
            recursion: AtomicI32::new(0),
            owner: AtomicUsize::new(0),
            semaphore: 0,
            spin: spin as usize,
        })
    };
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn InitializeCriticalSectionEx(
    cs: *mut CriticalSection,
    spin: u32,
    _: u32,
) -> i32 {
    InitializeCriticalSectionAndSpinCount(cs, spin)
}

#[allow(non_snake_case)]
pub extern "win64" fn DeleteCriticalSection(_: *mut CriticalSection) {}

fn try_enter(cs: &CriticalSection, me: usize) -> bool {
    if cs.owner.load(SeqCst) == me {
        cs.recursion.fetch_add(1, SeqCst);
        return true;
    }
    if cs.owner.compare_exchange(0, me, SeqCst, SeqCst).is_ok() {
        cs.recursion.store(1, SeqCst);
        cs.lock_count.fetch_add(1, SeqCst);
        return true;
    }
    false
}

#[allow(non_snake_case)]
pub extern "win64" fn EnterCriticalSection(cs: *mut CriticalSection) {
    // SAFETY: an initialised critical section of the title.
    let cs = unsafe { &*cs };
    let me = current_thread_id() as usize;
    for _ in 0..cs.spin.min(4000) + 64 {
        if try_enter(cs, me) {
            return;
        }
        std::hint::spin_loop();
    }
    while !try_enter(cs, me) {
        park(|| cs.owner.load(SeqCst) != 0, 10);
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn TryEnterCriticalSection(cs: *mut CriticalSection) -> i32 {
    // SAFETY: an initialised critical section of the title.
    try_enter(unsafe { &*cs }, current_thread_id() as usize).into()
}

#[allow(non_snake_case)]
pub extern "win64" fn LeaveCriticalSection(cs: *mut CriticalSection) {
    // SAFETY: an initialised critical section this thread holds.
    let cs = unsafe { &*cs };
    if cs.recursion.fetch_sub(1, SeqCst) == 1 {
        cs.lock_count.fetch_sub(1, SeqCst);
        cs.owner.store(0, SeqCst);
        unpark();
    }
}

/// An SRW lock is one pointer: bit 0 a writer, the rest twice the readers.
fn srw(lock: *mut usize) -> &'static AtomicUsize {
    // SAFETY: the title's pointer-sized, pointer-aligned SRW lock.
    unsafe { AtomicUsize::from_ptr(lock) }
}

#[allow(non_snake_case)]
pub extern "win64" fn InitializeSRWLock(lock: *mut usize) {
    srw(lock).store(0, SeqCst);
}

#[allow(non_snake_case)]
pub extern "win64" fn TryAcquireSRWLockExclusive(lock: *mut usize) -> u8 {
    srw(lock)
        .compare_exchange(0, 1, SeqCst, SeqCst)
        .is_ok()
        .into()
}

#[allow(non_snake_case)]
pub extern "win64" fn AcquireSRWLockExclusive(lock: *mut usize) {
    let l = srw(lock);
    while l.compare_exchange(0, 1, SeqCst, SeqCst).is_err() {
        park(|| l.load(SeqCst) != 0, 10);
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn ReleaseSRWLockExclusive(lock: *mut usize) {
    srw(lock).store(0, SeqCst);
    unpark();
}

#[allow(non_snake_case)]
pub extern "win64" fn TryAcquireSRWLockShared(lock: *mut usize) -> u8 {
    let l = srw(lock);
    let s = l.load(SeqCst);
    (s & 1 == 0 && l.compare_exchange(s, s + 2, SeqCst, SeqCst).is_ok()).into()
}

#[allow(non_snake_case)]
pub extern "win64" fn AcquireSRWLockShared(lock: *mut usize) {
    // shortcut: readers can starve a writer; nothing favours a waiting one.
    let l = srw(lock);
    loop {
        let s = l.load(SeqCst);
        if s & 1 == 0 && l.compare_exchange(s, s + 2, SeqCst, SeqCst).is_ok() {
            return;
        }
        park(|| l.load(SeqCst) & 1 != 0, 10);
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn ReleaseSRWLockShared(lock: *mut usize) {
    if srw(lock).fetch_sub(2, SeqCst) == 2 {
        unpark();
    }
}

/// A condition variable is one pointer, used here as a wake count.
#[allow(non_snake_case)]
pub extern "win64" fn InitializeConditionVariable(cv: *mut usize) {
    srw(cv).store(0, SeqCst);
}

#[allow(non_snake_case)]
pub extern "win64" fn WakeConditionVariable(cv: *mut usize) {
    // Waking every waiter is allowed: a wait may always end spuriously.
    WakeAllConditionVariable(cv);
}

#[allow(non_snake_case)]
pub extern "win64" fn WakeAllConditionVariable(cv: *mut usize) {
    srw(cv).fetch_add(1, SeqCst);
    unpark();
}

fn sleep_on(cv: *mut usize, ms: u32, release: impl FnOnce(), acquire: impl FnOnce()) -> i32 {
    let c = srw(cv);
    let seen = c.load(SeqCst);
    release();
    let woken = park(|| c.load(SeqCst) == seen, ms);
    acquire();
    if !woken {
        set_last_error(ERROR_TIMEOUT);
    }
    woken.into()
}

#[allow(non_snake_case)]
pub extern "win64" fn SleepConditionVariableSRW(
    cv: *mut usize,
    lock: *mut usize,
    ms: u32,
    flags: u32,
) -> i32 {
    // CONDITION_VARIABLE_LOCKMODE_SHARED
    if flags & 1 != 0 {
        sleep_on(
            cv,
            ms,
            || ReleaseSRWLockShared(lock),
            || AcquireSRWLockShared(lock),
        )
    } else {
        sleep_on(
            cv,
            ms,
            || ReleaseSRWLockExclusive(lock),
            || AcquireSRWLockExclusive(lock),
        )
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn SleepConditionVariableCS(
    cv: *mut usize,
    cs: *mut CriticalSection,
    ms: u32,
) -> i32 {
    // SAFETY: a critical section this thread holds.
    let depth = unsafe { (*cs).recursion.load(SeqCst) };
    let leave = || {
        // SAFETY: as above; it is left entirely, and taken back as deep.
        unsafe { (*cs).recursion.store(1, SeqCst) };
        LeaveCriticalSection(cs);
    };
    let enter = || {
        EnterCriticalSection(cs);
        // SAFETY: held again by this thread.
        unsafe { (*cs).recursion.store(depth, SeqCst) };
    };
    sleep_on(cv, ms, leave, enter)
}

#[allow(non_snake_case)]
pub extern "win64" fn WaitOnAddress(
    address: *const c_void,
    compare: *const c_void,
    size: usize,
    ms: u32,
) -> i32 {
    if !matches!(size, 1 | 2 | 4 | 8) {
        set_last_error(87);
        return 0;
    }
    // SAFETY: the title passes `size` readable bytes at each; read volatile,
    // since another thread changes `address`.
    let same = || {
        (0..size).all(|i| unsafe {
            std::ptr::read_volatile((address as *const u8).add(i)) == *(compare as *const u8).add(i)
        })
    };
    let woken = park(same, ms);
    if !woken {
        set_last_error(ERROR_TIMEOUT);
    }
    woken.into()
}

#[allow(non_snake_case)]
pub extern "win64" fn WakeByAddressSingle(_: *const c_void) {
    unpark();
}

#[allow(non_snake_case)]
pub extern "win64" fn WakeByAddressAll(_: *const c_void) {
    unpark();
}

/// `INIT_ONCE`: 0 not begun, 1 under way, 2 done (its context in the upper
/// bits, as Windows keeps it).
type InitFn = unsafe extern "win64" fn(*mut usize, *mut c_void, *mut *mut c_void) -> i32;

#[allow(non_snake_case)]
pub extern "win64" fn InitOnceExecuteOnce(
    once: *mut usize,
    init: InitFn,
    parameter: *mut c_void,
    context: *mut *mut c_void,
) -> i32 {
    let o = srw(once);
    loop {
        match o.compare_exchange(0, 1, SeqCst, SeqCst) {
            Ok(_) => {
                let mut made: *mut c_void = std::ptr::null_mut();
                // SAFETY: the title's initialisation function, as it declares it.
                let ok = unsafe { init(once, parameter, &mut made) } != 0;
                o.store(if ok { (made as usize & !3) | 2 } else { 0 }, SeqCst);
                unpark();
                if ok && !context.is_null() {
                    // SAFETY: the title's out pointer.
                    unsafe { *context = made };
                }
                return ok.into();
            }
            Err(s) if s & 3 == 2 => {
                if !context.is_null() {
                    // SAFETY: the title's out pointer.
                    unsafe { *context = (s & !3) as *mut c_void };
                }
                return 1;
            }
            Err(_) => {
                park(|| o.load(SeqCst) & 3 == 1, 10);
            }
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn InitOnceInitialize(once: *mut usize) {
    srw(once).store(0, SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn events_wake_waiters_and_auto_reset() {
        let e = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
        assert_eq!(WaitForSingleObject(e, 0), WAIT_TIMEOUT);
        let waiter = std::thread::spawn(move || WaitForSingleObject(e, INFINITE));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(SetEvent(e), 1);
        assert_eq!(waiter.join().unwrap(), WAIT_OBJECT_0);
        assert_eq!(WaitForSingleObject(e, 0), WAIT_TIMEOUT, "auto-reset");
        assert_eq!(CloseHandle(e), 1);
        assert_eq!(WaitForSingleObject(e, 0), WAIT_FAILED);
    }

    #[test]
    fn wait_for_any_and_all() {
        let manual = CreateEventW(std::ptr::null(), 1, 1, std::ptr::null());
        let off = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
        let both = [off, manual];
        assert_eq!(
            WaitForMultipleObjects(2, both.as_ptr(), 0, 0),
            WAIT_OBJECT_0 + 1
        );
        assert_eq!(
            WaitForMultipleObjects(2, both.as_ptr(), 1, 10),
            WAIT_TIMEOUT
        );
        SetEvent(off);
        assert_eq!(
            WaitForMultipleObjects(2, both.as_ptr(), 1, 0),
            WAIT_OBJECT_0
        );
    }

    #[test]
    fn semaphores_and_mutexes_count() {
        let s = CreateSemaphoreW(std::ptr::null(), 1, 2, std::ptr::null());
        assert_eq!(WaitForSingleObject(s, 0), WAIT_OBJECT_0);
        assert_eq!(WaitForSingleObject(s, 0), WAIT_TIMEOUT);
        let mut previous = -1;
        assert_eq!(ReleaseSemaphore(s, 2, &mut previous), 1);
        assert_eq!(previous, 0);
        assert_eq!(
            ReleaseSemaphore(s, 1, std::ptr::null_mut()),
            0,
            "past the maximum"
        );

        let m = CreateMutexW(std::ptr::null(), 1, std::ptr::null());
        assert_eq!(
            WaitForSingleObject(m, 0),
            WAIT_OBJECT_0,
            "recursive for its owner"
        );
        let other = std::thread::spawn(move || WaitForSingleObject(m, 0))
            .join()
            .unwrap();
        assert_eq!(other, WAIT_TIMEOUT);
        assert_eq!(ReleaseMutex(m), 1);
        assert_eq!(ReleaseMutex(m), 1);
        assert_eq!(ReleaseMutex(m), 0, "no longer the owner");
    }

    #[test]
    fn critical_sections_and_srw_locks_exclude() {
        struct Shared {
            cs: std::cell::UnsafeCell<CriticalSection>,
            srw: std::cell::UnsafeCell<usize>,
            n: std::cell::UnsafeCell<u64>,
        }
        // SAFETY: every access below holds one of the locks.
        unsafe impl Sync for Shared {}
        let s = Arc::new(Shared {
            // SAFETY: initialised right below, as titles do.
            cs: std::cell::UnsafeCell::new(unsafe { std::mem::zeroed() }),
            srw: std::cell::UnsafeCell::new(0),
            n: std::cell::UnsafeCell::new(0),
        });
        InitializeCriticalSection(s.cs.get());
        let workers: Vec<_> = (0..4)
            .map(|i| {
                let s = s.clone();
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        if i % 2 == 0 {
                            EnterCriticalSection(s.cs.get());
                            EnterCriticalSection(s.cs.get());
                            // SAFETY: under the critical section.
                            unsafe { *s.n.get() += 1 };
                            LeaveCriticalSection(s.cs.get());
                            LeaveCriticalSection(s.cs.get());
                        } else {
                            AcquireSRWLockExclusive(s.srw.get());
                            EnterCriticalSection(s.cs.get());
                            // SAFETY: under the critical section.
                            unsafe { *s.n.get() += 1 };
                            LeaveCriticalSection(s.cs.get());
                            ReleaseSRWLockExclusive(s.srw.get());
                        }
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        // SAFETY: every worker ended.
        assert_eq!(unsafe { *s.n.get() }, 8000);
    }

    #[test]
    fn condition_variables_wake_and_time_out() {
        struct Shared(std::cell::UnsafeCell<[usize; 2]>, AtomicU32);
        // SAFETY: the lock word and the condition variable are used atomically.
        unsafe impl Sync for Shared {}
        let s = Arc::new(Shared(
            std::cell::UnsafeCell::new([0, 0]),
            AtomicU32::new(0),
        ));
        // SAFETY: two pointer-sized words: [lock, condition variable].
        let (lock, cv) = unsafe { ((*s.0.get()).as_mut_ptr(), (*s.0.get()).as_mut_ptr().add(1)) };
        let (lock, cv) = (lock as usize, cv as usize);
        AcquireSRWLockExclusive(lock as *mut usize);
        assert_eq!(
            SleepConditionVariableSRW(cv as *mut usize, lock as *mut usize, 10, 0),
            0
        );
        let s2 = s.clone();
        let waker = std::thread::spawn(move || {
            AcquireSRWLockExclusive(lock as *mut usize);
            s2.1.store(1, SeqCst);
            ReleaseSRWLockExclusive(lock as *mut usize);
            WakeConditionVariable(cv as *mut usize);
        });
        while s.1.load(SeqCst) == 0 {
            SleepConditionVariableSRW(cv as *mut usize, lock as *mut usize, INFINITE, 0);
        }
        ReleaseSRWLockExclusive(lock as *mut usize);
        waker.join().unwrap();
    }

    #[test]
    fn wait_on_address_and_init_once() {
        static WORD: AtomicU32 = AtomicU32::new(0);
        let zero = 0u32;
        let at = WORD.as_ptr() as usize;
        let t = std::thread::spawn(move || {
            WaitOnAddress(
                at as *const c_void,
                (&zero as *const u32).cast(),
                4,
                INFINITE,
            )
        });
        std::thread::sleep(Duration::from_millis(20));
        WORD.store(5, SeqCst);
        WakeByAddressSingle(at as *const c_void);
        assert_eq!(t.join().unwrap(), 1);

        static RUNS: AtomicU32 = AtomicU32::new(0);
        unsafe extern "win64" fn init(_: *mut usize, p: *mut c_void, out: *mut *mut c_void) -> i32 {
            RUNS.fetch_add(1, SeqCst);
            // SAFETY: the out pointer InitOnceExecuteOnce passes.
            unsafe { *out = p };
            1
        }
        let mut once = 0usize;
        let mut context = std::ptr::null_mut();
        for _ in 0..3 {
            assert_eq!(
                InitOnceExecuteOnce(&mut once, init, 0x40 as *mut c_void, &mut context),
                1
            );
        }
        assert_eq!((RUNS.load(SeqCst), context as usize), (1, 0x40));
    }
}
