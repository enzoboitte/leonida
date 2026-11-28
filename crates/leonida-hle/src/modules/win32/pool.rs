//! Windows' thread pool: work items, timers and waits whose callbacks run
//! on threads of ours (started by the loader, so title DLLs hear of them).
//!
//! shortcut: every callback gets a thread of its own instead of a pool of
//! a few; callback environments (priority, cleanup groups) are ignored.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What every callback is given as its instance; no title code reads into it.
const INSTANCE: usize = 0x1;
const FILETIME_TO_UNIX: u64 = 116_444_736_000_000_000;

/// Runs `job` on a title thread.
fn spawn(job: Box<dyn FnOnce() + Send>) {
    extern "win64" fn run(job: usize) -> u32 {
        // SAFETY: the Box `spawn` leaked for this thread alone.
        let job = unsafe { Box::from_raw(job as *mut Box<dyn FnOnce() + Send>) };
        job();
        0
    }
    let job = Box::into_raw(Box::new(job)) as usize;
    // SAFETY: `run` is a thread procedure that owns `job`.
    if let Err(e) = unsafe { leonida_loader::spawn_thread(run as *const () as usize, job, 0) } {
        tracing::error!("thread pool: {e}");
        // SAFETY: the thread did not start: the Box is still ours.
        drop(unsafe { Box::from_raw(job as *mut Box<dyn FnOnce() + Send>) });
    }
}

/// Callbacks of one object that are running or queued.
#[derive(Default)]
struct Running {
    count: Mutex<usize>,
    done: Condvar,
}

impl Running {
    fn begin(&self) {
        *self.count.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    }

    fn end(&self) {
        let mut n = self.count.lock().unwrap_or_else(|e| e.into_inner());
        *n -= 1;
        self.done.notify_all();
    }

    fn wait(&self) {
        let mut n = self.count.lock().unwrap_or_else(|e| e.into_inner());
        while *n > 0 {
            n = self.done.wait(n).unwrap_or_else(|e| e.into_inner());
        }
    }
}

type Callback3 = unsafe extern "win64" fn(usize, usize, usize);
type Callback4 = unsafe extern "win64" fn(usize, usize, usize, u32);

/// A work item, a timer or a wait: its callback, context and generation (a
/// new setting or a cancel makes earlier runs moot).
struct Item {
    callback: usize,
    context: usize,
    generation: AtomicU64,
    running: Running,
}

fn item(callback: usize, context: usize) -> usize {
    Arc::into_raw(Arc::new(Item {
        callback,
        context,
        generation: AtomicU64::new(0),
        running: Running::default(),
    })) as usize
}

/// # Safety
/// `handle` came from [`item`] and is not closed.
unsafe fn get(handle: usize) -> Arc<Item> {
    // SAFETY: the caller's guarantee; one more reference for the caller.
    unsafe {
        Arc::increment_strong_count(handle as *const Item);
        Arc::from_raw(handle as *const Item)
    }
}

/// # Safety
/// As [`get`]; the handle is not used again.
unsafe fn close(handle: usize) {
    // SAFETY: the reference `item` made.
    drop(unsafe { Arc::from_raw(handle as *const Item) });
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateThreadpoolWork(
    callback: usize,
    context: usize,
    _env: *const c_void,
) -> usize {
    item(callback, context)
}

#[allow(non_snake_case)]
pub extern "win64" fn SubmitThreadpoolWork(work: usize) {
    // SAFETY: a work item of ours.
    let it = unsafe { get(work) };
    it.running.begin();
    spawn(Box::new(move || {
        let callback: Callback3 = unsafe { std::mem::transmute(it.callback) };
        // SAFETY: the title's work callback, as it gave it.
        unsafe { callback(INSTANCE, it.context, work) };
        it.running.end();
    }));
}

/// shortcut: queued callbacks are not cancelled; they all run first.
#[allow(non_snake_case)]
pub extern "win64" fn WaitForThreadpoolWorkCallbacks(work: usize, _cancel: i32) {
    // SAFETY: a work item of ours.
    unsafe { get(work) }.running.wait();
}

#[allow(non_snake_case)]
pub extern "win64" fn CloseThreadpoolWork(work: usize) {
    // SAFETY: a work item of ours, given back.
    unsafe { close(work) };
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateThreadpoolTimer(
    callback: usize,
    context: usize,
    _env: *const c_void,
) -> usize {
    item(callback, context)
}

/// How long until a FILETIME due time: negative is relative (100 ns), else
/// absolute.
fn until(due: i64) -> Duration {
    if due < 0 {
        return Duration::from_nanos(due.unsigned_abs() * 100);
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let now = FILETIME_TO_UNIX + (now.as_nanos() / 100) as u64;
    Duration::from_nanos((due as u64).saturating_sub(now) * 100)
}

#[allow(non_snake_case)]
pub extern "win64" fn SetThreadpoolTimer(timer: usize, due: *const i64, period: u32, _window: u32) {
    // SAFETY: a timer of ours.
    let it = unsafe { get(timer) };
    let generation = it.generation.fetch_add(1, SeqCst) + 1;
    if due.is_null() {
        return; // cancelled
    }
    // SAFETY: the title's FILETIME.
    let first = until(unsafe { due.read_unaligned() });
    it.running.begin();
    spawn(Box::new(move || {
        let mut at = Instant::now() + first;
        loop {
            std::thread::sleep(at.saturating_duration_since(Instant::now()));
            if it.generation.load(SeqCst) != generation {
                break;
            }
            let callback: Callback3 = unsafe { std::mem::transmute(it.callback) };
            // SAFETY: the title's timer callback, as it gave it.
            unsafe { callback(INSTANCE, it.context, timer) };
            if period == 0 {
                break;
            }
            at += Duration::from_millis(period.into());
        }
        it.running.end();
    }));
}

#[allow(non_snake_case)]
pub extern "win64" fn WaitForThreadpoolTimerCallbacks(timer: usize, cancel: i32) {
    // SAFETY: a timer of ours.
    let it = unsafe { get(timer) };
    if cancel != 0 {
        it.generation.fetch_add(1, SeqCst);
    }
    it.running.wait();
}

#[allow(non_snake_case)]
pub extern "win64" fn CloseThreadpoolTimer(timer: usize) {
    // SAFETY: a timer of ours: no new run after this.
    unsafe {
        get(timer).generation.fetch_add(1, SeqCst);
        close(timer);
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateThreadpoolWait(
    callback: usize,
    context: usize,
    _env: *const c_void,
) -> usize {
    item(callback, context)
}

#[allow(non_snake_case)]
pub extern "win64" fn SetThreadpoolWait(wait: usize, handle: usize, timeout: *const i64) {
    // SAFETY: a wait of ours.
    let it = unsafe { get(wait) };
    let generation = it.generation.fetch_add(1, SeqCst) + 1;
    if handle == 0 {
        return; // cancelled
    }
    let ms = if timeout.is_null() {
        super::sync::INFINITE
    } else {
        // SAFETY: the title's FILETIME.
        until(unsafe { timeout.read_unaligned() })
            .as_millis()
            .min(u32::MAX as u128 - 1) as u32
    };
    it.running.begin();
    spawn(Box::new(move || {
        let result = super::sync::wait(&[handle], false, ms);
        if it.generation.load(SeqCst) == generation {
            let callback: Callback4 = unsafe { std::mem::transmute(it.callback) };
            // SAFETY: the title's wait callback, as it gave it.
            unsafe { callback(INSTANCE, it.context, wait, result) };
        }
        it.running.end();
    }));
}

#[allow(non_snake_case)]
pub extern "win64" fn WaitForThreadpoolWaitCallbacks(wait: usize, cancel: i32) {
    WaitForThreadpoolTimerCallbacks(wait, cancel);
}

#[allow(non_snake_case)]
pub extern "win64" fn CloseThreadpoolWait(wait: usize) {
    CloseThreadpoolTimer(wait);
}

#[allow(non_snake_case)]
pub extern "win64" fn QueueUserWorkItem(callback: usize, context: usize, _flags: u32) -> i32 {
    spawn(Box::new(move || {
        let callback: unsafe extern "win64" fn(usize) -> u32 =
            unsafe { std::mem::transmute(callback) };
        // SAFETY: the title's thread procedure, as it gave it.
        unsafe { callback(context) };
    }));
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn CallbackMayRunLong(_instance: usize) -> i32 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn DisassociateCurrentThreadFromCallback(_instance: usize) {}

#[allow(non_snake_case)]
pub extern "win64" fn FreeLibraryWhenCallbackReturns(_instance: usize, _module: usize) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    unsafe extern "win64" fn count(_: usize, context: usize, _: usize) {
        // SAFETY: the test's AtomicUsize.
        unsafe { (*(context as *const AtomicUsize)).fetch_add(1, SeqCst) };
    }

    #[test]
    fn work_runs_as_often_as_submitted_and_can_be_waited_for() {
        let n = Box::leak(Box::new(AtomicUsize::new(0)));
        let work = CreateThreadpoolWork(
            count as *const () as usize,
            n as *const _ as usize,
            std::ptr::null(),
        );
        for _ in 0..5 {
            SubmitThreadpoolWork(work);
        }
        WaitForThreadpoolWorkCallbacks(work, 0);
        assert_eq!(n.load(SeqCst), 5);
        CloseThreadpoolWork(work);
    }

    #[test]
    fn timers_fire_repeat_and_cancel() {
        let n = Box::leak(Box::new(AtomicUsize::new(0)));
        let timer = CreateThreadpoolTimer(
            count as *const () as usize,
            n as *const _ as usize,
            std::ptr::null(),
        );
        let due = -10_000i64; // 1 ms from now
        SetThreadpoolTimer(timer, &due, 5, 0);
        std::thread::sleep(Duration::from_millis(40));
        WaitForThreadpoolTimerCallbacks(timer, 1);
        let fired = n.load(SeqCst);
        assert!(fired >= 2, "fired {fired} times");
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(n.load(SeqCst), fired, "cancelled");
        CloseThreadpoolTimer(timer);
    }

    #[test]
    fn waits_call_back_when_their_handle_is_set() {
        unsafe extern "win64" fn got(_: usize, context: usize, _: usize, result: u32) {
            // SAFETY: the test's AtomicUsize.
            unsafe { (*(context as *const AtomicUsize)).store(result as usize + 1, SeqCst) };
        }
        let n = Box::leak(Box::new(AtomicUsize::new(0)));
        let wait = CreateThreadpoolWait(
            got as *const () as usize,
            n as *const _ as usize,
            std::ptr::null(),
        );
        let event = super::super::sync::CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
        SetThreadpoolWait(wait, event, std::ptr::null());
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(n.load(SeqCst), 0, "not yet");
        super::super::sync::SetEvent(event);
        WaitForThreadpoolWaitCallbacks(wait, 0);
        assert_eq!(n.load(SeqCst), 1, "WAIT_OBJECT_0");
        CloseThreadpoolWait(wait);
    }
}
