//! Instructions the console's CPU (Zen 2) has and an Intel PC's lacks, emulated: the title's
//! code raises STATUS_ILLEGAL_INSTRUCTION on them (the title zeroes memory with a `clzero` loop at
//! its first frames), and the vectored handler steps over them here.
//!
//! Ported from XCL (github.com/ivall/xcl, GPL-2.0-only): `insn.rs` (decoder, as
//! `console_insn_decode.rs`, unchanged) and `eh.rs` `emulate_console_only`.

#[path = "console_insn_decode.rs"]
mod decode;

use decode::ConsoleOnly;
use std::cell::Cell;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering::Relaxed};
use host::CONTEXT;

const CONTINUE_EXECUTION: i32 = -1;

/// Claims the first warning for a kind of instruction, regardless of its operands.
/// Seen kinds take only a load: the exception handler need not lock on every wait hint.
fn first_observation(seen: &AtomicU16, instruction: ConsoleOnly) -> bool {
    let bit = match instruction {
        ConsoleOnly::Clwb => 1 << 0,
        ConsoleOnly::Clzero => 1 << 1,
        ConsoleOnly::ClzeroLoop { .. } => 1 << 2,
        ConsoleOnly::ClzeroStep { .. } => 1 << 3,
        ConsoleOnly::Rdpru => 1 << 4,
        ConsoleOnly::Monitorx => 1 << 5,
        ConsoleOnly::Mwaitx => 1 << 6,
        ConsoleOnly::Rdpid(_) => 1 << 7,
        ConsoleOnly::Extrq { .. } => 1 << 8,
        ConsoleOnly::Insertq { .. } => 1 << 9,
        ConsoleOnly::Ud2 => return false, // the title's own trap is not emulated or logged
    };
    // Only warning ownership is shared, not data published by another thread. The RMW
    // chooses exactly one caller when several threads first see the same kind together.
    seen.load(Relaxed) & bit == 0 && seen.fetch_or(bit, Relaxed) & bit == 0
}

/// Whether what Windows said about memory is remembered per thread (the regions it found
/// writable, the code bytes it found readable) and not asked again at every emulated
/// instruction. A query walks the pages of the region it answers for: asked from outside, it
/// took 2-9 ms in the title's heaps of 0.6-2.2 GB and 89 ms in its 6.2 GB page pool (run 30),
/// and the title zeroes blocks of them with `clzero` loops all through a frame: six of its
/// threads spent 25-78 % of their time inside that query (runs 29 and 30), about 27 s of
/// processor time in Windows per 9 s frame. On by default; `LEONIDA_CLZERO_ASK=1` asks every
/// time, as before.
fn remembers() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LEONIDA_CLZERO_ASK").is_none_or(|v| v != "1"))
}

/// Answers a remembered region gives before Windows is asked about it again: one freed or
/// protected meanwhile is forgotten then.
const ANSWERS: u32 = 4096;

thread_local! {
    /// The regions Windows last said are committed and writable (start, end), newest first,
    /// and the answers they gave since they were emptied.
    static WRITABLE: Cell<([(usize, usize); 4], u32)> = const { Cell::new(([(0, 0); 4], 0)) };
    /// Faulting addresses and the code bytes found readable there, newest first.
    static CODE: Cell<[(usize, usize); 8]> = const { Cell::new([(0, 0); 8]) };
}

/// Instructions emulated, the times Windows was asked about memory for them, and the bytes
/// `clzero` loops zeroed, since the start.
static EMULATED: AtomicU64 = AtomicU64::new(0);
static ASKED: AtomicU64 = AtomicU64::new(0);
static ZEROED: AtomicU64 = AtomicU64::new(0);

/// Exceptions taken by kind since the last line: a lone `clzero`, the 12-byte loop, the
/// 15-byte loop taken whole, `monitorx`, `mwaitx`, everything else; and the 64-byte lines the
/// 15-byte loops zeroed (each was an exception of its own before they were taken whole).
static KINDS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
const K_CLZERO: usize = 0;
const K_LOOP12: usize = 1;
const K_LOOP15: usize = 2;
const K_MONITORX: usize = 3;
const K_MWAITX: usize = 4;
const K_OTHER: usize = 5;
const K_LOOP15_LINES: usize = 6;

/// Waits the title made with `monitorx` / `mwaitx` (it waits for a fence that way, reading
/// the fence's value in memory: its +0x4fb0ad0) that have ended since the last line: how
/// many, their microseconds, and both again for those of 1 ms and more, 5 ms and more,
/// 20 ms and more.
static WAITS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
/// Waits said in the log one by one.
static WAITS_SAID: AtomicU64 = AtomicU64::new(0);
/// When the last line was written, in milliseconds of [`clock_us`].
static LINE_AT: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The address and the value this thread last armed a monitor for (`monitorx`: rax, and
    /// the title's wanted value in rsi).
    static MONITOR: Cell<(usize, u64)> = const { Cell::new((0, 0)) };
    /// This thread's wait under way: when it began and when its last `mwaitx` was, in
    /// microseconds, what it waits for, and its `mwaitx` count.
    static WAIT: Cell<(u64, u64, usize, u64, u32)> = const { Cell::new((0, 0, 0, 0, 0)) };
}

/// Exact integer microseconds; the common 10 MHz counter needs only a narrow division.
/// Other counter frequencies retain the wide multiply so a long uptime cannot overflow.
fn counter_us(ticks: u64, per_second: u64) -> u64 {
    if per_second == 10_000_000 {
        ticks / 10
    } else {
        (ticks as u128 * 1_000_000 / per_second as u128) as u64
    }
}

/// Microseconds on the performance counter.
fn clock_us() -> u64 {
    static PER_SECOND: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let per_second = *PER_SECOND.get_or_init(|| host::frequency().max(1));
    counter_us(host::counter(), per_second as u64)
}

/// Whether events are said one by one now: four seconds of every two minutes of the wall
/// clock, the same seconds in which the fence meter of the graphics side says its own
/// (`leonida_gpu::fence::pace`), so that the two can be laid side by side.
fn detail_now() -> bool {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).is_ok_and(|d| d.as_secs() % 120 < 4)
}

/// Whether the 15-byte `clzero` loop is taken whole (`LEONIDA_CLZERO_LOOP15=0`: line by
/// line, as before).
fn loop15() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LEONIDA_CLZERO_LOOP15").is_none_or(|v| v != "0"))
}

/// A wait of this thread has ended: counted, and said if it is one to say.
fn wait_ended(begin: u64, last: u64, address: usize, wanted: u64, spins: u32) {
    let us = last.saturating_sub(begin);
    for (i, least) in [0u64, 1_000, 5_000, 20_000].into_iter().enumerate() {
        if us >= least {
            WAITS[2 * i].fetch_add(1, Relaxed);
            WAITS[2 * i + 1].fetch_add(us, Relaxed);
        }
    }
    if us >= 500 && (detail_now() || WAITS_SAID.fetch_add(1, Relaxed) < 40) {
        let thread = host::thread_id();
        tracing::info!(
            "mwait: thread {thread} waited {:.1} ms for the value at {address:#x} to reach {wanted} ({spins} mwaitx)",
            us as f64 / 1e3
        );
    }
}

/// A `mwaitx` of this thread: part of the wait under way, or the first of a new one.
fn wait_step(now: u64) {
    let (address, wanted) = MONITOR.with(Cell::get);
    let (begin, last, was_at, was_for, spins) = WAIT.with(Cell::get);
    if begin != 0 && was_at == address && was_for == wanted && now.saturating_sub(last) < 2_000 {
        WAIT.with(|w| w.set((begin, now, address, wanted, spins + 1)));
        return;
    }
    if begin != 0 {
        wait_ended(begin, last, was_at, was_for, spins);
    }
    WAIT.with(|w| w.set((now, now, address, wanted, 1)));
}

/// A line every five seconds: exceptions a second by kind, and the waits that ended.
fn line(now_us: u64) {
    let now = now_us / 1000;
    let last = LINE_AT.load(Relaxed);
    if now < last + 5_000 || LINE_AT.compare_exchange(last, now, Relaxed, Relaxed).is_err() {
        return;
    }
    if last == 0 {
        return;
    }
    let seconds = (now - last) as f64 / 1e3;
    let k: Vec<f64> = KINDS.iter().map(|k| k.swap(0, Relaxed) as f64 / seconds).collect();
    let w: Vec<f64> = WAITS.iter().map(|w| w.swap(0, Relaxed) as f64).collect();
    tracing::info!(
        "console CPU instructions, exceptions a second: clzero alone {:.0}, its 12-byte loop {:.0}, its 15-byte loop taken whole {:.0} ({:.0} lines a second, each an exception \
         before; LEONIDA_CLZERO_LOOP15=0: again), monitorx {:.0}, mwaitx {:.0}, others {:.0} | mwaitx waits ended a second: {:.0}, {:.1} ms of thread time a second; of 1 ms and \
         more {:.1} ({:.1} ms), of 5 ms and more {:.1} ({:.1} ms), of 20 ms and more {:.1} ({:.1} ms)",
        k[K_CLZERO],
        k[K_LOOP12],
        k[K_LOOP15],
        k[K_LOOP15_LINES],
        k[K_MONITORX],
        k[K_MWAITX],
        k[K_OTHER],
        w[0] / seconds,
        w[1] / seconds / 1e3,
        w[2] / seconds,
        w[3] / seconds / 1e3,
        w[4] / seconds,
        w[5] / seconds / 1e3,
        w[6] / seconds,
        w[7] / seconds / 1e3,
    );
}

fn readable(va: usize, bytes: usize) -> bool {
    ASKED.fetch_add(1, Relaxed);
    host::region(va).is_some_and(|r| r.readable && r.to >= va + bytes)
}

fn writable(va: usize, bytes: usize) -> bool {
    writable_to(va, va + bytes) == Some(va + bytes)
}

/// Where the committed, writable memory that `va` lies in ends, at most `end`; None if `va`
/// is not in such memory. A region found is remembered ([`remembers`]).
/// shortcut: a remembered region that was freed or protected since is written all the same
/// for up to [`ANSWERS`] answers: the write then faults inside the handler, which the guard
/// on view memory opens as it does any write, and which is a crash otherwise (as the
/// unchecked single-line emulation behind this one already is).
fn writable_to(va: usize, end: usize) -> Option<usize> {
    let remember = remembers();
    if remember {
        let (regions, answers) = WRITABLE.with(Cell::get);
        if let Some(&(_, to)) = regions.iter().find(|&&(from, to)| from <= va && va < to) {
            let next = if answers >= ANSWERS { ([(0, 0); 4], 0) } else { (regions, answers + 1) };
            WRITABLE.with(|w| w.set(next));
            return Some(to.min(end));
        }
    }
    ASKED.fetch_add(1, Relaxed);
    let region = host::region(va).filter(|r| r.writable)?;
    let (from, to) = (region.from, region.to);
    if remember {
        WRITABLE.with(|w| {
            let (mut regions, answers) = w.get();
            regions.rotate_right(1);
            regions[0] = (from, to);
            w.set((regions, answers));
        });
    }
    Some(to.min(end))
}

/// The register `r` of `ctx` (CONTEXT order: rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8..r15).
fn register(ctx: &mut CONTEXT, r: u8) -> &mut u64 {
    match r & 15 {
        0 => &mut ctx.Rax,
        1 => &mut ctx.Rcx,
        2 => &mut ctx.Rdx,
        3 => &mut ctx.Rbx,
        4 => &mut ctx.Rsp,
        5 => &mut ctx.Rbp,
        6 => &mut ctx.Rsi,
        7 => &mut ctx.Rdi,
        8 => &mut ctx.R8,
        9 => &mut ctx.R9,
        10 => &mut ctx.R10,
        11 => &mut ctx.R11,
        12 => &mut ctx.R12,
        13 => &mut ctx.R13,
        14 => &mut ctx.R14,
        _ => &mut ctx.R15,
    }
}

/// Where the lines of the loop `mov rax, ptr; add ptr, 0x40; clzero; cmp ptr, end; jb` end,
/// from the state the loop has at its `clzero`: the line at `rax` is zeroed now, and one
/// more at `ptr` for every pass the `cmp` sends back. None if this is not the plain case
/// the loop makes (the line at `rax` on a 64-byte boundary, `ptr` the next line, under
/// 2 GiB in all): then every line is taken alone, as the instruction itself does it.
fn loop15_end(rax: usize, ptr: usize, end: usize) -> Option<usize> {
    if rax & 63 != 0 || ptr != rax.wrapping_add(64) || ptr < rax {
        return None;
    }
    let more = if ptr < end { (end - ptr).div_ceil(64) } else { 0 };
    let last = ptr.checked_add(more.checked_mul(64)?)?;
    (last - rax < 2 << 30).then_some(last)
}

/// The `clzero` at `rip` in the loop `mov rax, ptr; add ptr, 0x40; clzero; cmp ptr, end; jb`,
/// with all the lines the loop would still zero: `Some(true)` when they are zeroed and the
/// registers stand as the loop leaves them at its last `clzero` (the caller steps over the
/// instruction; the title's own `cmp` and `jb` then leave the loop), `Some(false)` when the
/// first line is not writable (the real instruction faults there too), and when only a
/// first part was writable the registers stand at the line after it, where the instruction
/// runs again. None if it is not that loop: the lone instruction is emulated.
///
/// # Safety
/// `rip` is the faulting instruction of `ctx`, whose bytes were read.
unsafe fn clzero_loop15(ctx: &mut CONTEXT, rip: usize, ptr_reg: u8, end_reg: u8) -> Option<bool> {
    // The seven bytes in front of it: in the same page as the instruction, which ran.
    if rip & 0xFFF < 7 && !readable(rip - 7, 7) {
        return None;
    }
    // SAFETY: readable, as checked or as the instruction's own page.
    let before = unsafe { std::slice::from_raw_parts((rip - 7) as *const u8, 7) };
    if !decode::loop15_head(before, ptr_reg) || end_reg == 0 || end_reg == ptr_reg {
        return None;
    }
    let rax = ctx.Rax as usize;
    let ptr = *register(ctx, ptr_reg) as usize;
    let end = *register(ctx, end_reg) as usize;
    let last = loop15_end(rax, ptr, end)?;
    // Region by region, as the 12-byte loop: a block may lie across several views.
    let mut at = rax;
    while at < last {
        let Some(to) = writable_to(at, last) else { break };
        // A region ends on a page boundary, so `to` is on a line boundary as `at` is.
        // SAFETY: committed and writable from `at` to `to`.
        unsafe { std::ptr::write_bytes(at as *mut u8, 0, to - at) };
        at = to;
    }
    if at == rax {
        return Some(false);
    }
    ZEROED.fetch_add((at - rax) as u64, Relaxed);
    KINDS[K_LOOP15_LINES].fetch_add(((at - rax) / 64) as u64, Relaxed);
    if at < last {
        // What follows is not writable: the loop stands at that line's `clzero` now, where
        // the real one faults too. The instruction runs again (the caller must not step).
        ctx.Rax = at as u64;
        *register(ctx, ptr_reg) = (at + 64) as u64;
        ctx.Rip = (rip as u64).wrapping_sub(3);
        return Some(true);
    }
    ctx.Rax = (last - 64) as u64;
    *register(ctx, ptr_reg) = last as u64;
    Some(true)
}

/// An illegal instruction at `ctx.Rip`: a console-only one is emulated and stepped over
/// (`CONTINUE_EXECUTION`); anything else returns 0 (the crash report and the title's handlers
/// see it).
///
/// # Safety
/// `ctx` is the faulting thread's context, from the exception being dispatched.
pub(crate) unsafe fn emulate(ctx: &mut CONTEXT) -> i32 {
    let rip = ctx.Rip as usize;
    // The same few instructions fault again and again: their bytes are asked about once.
    let known = if remembers() { CODE.with(Cell::get) } else { [(0, 0); 8] };
    let n = match known.iter().find(|&&(at, n)| at == rip && n > 0) {
        Some(&(_, n)) => n,
        None => {
            if !readable(rip, 1) {
                return 0;
            }
            let n = (1..=16).rev().find(|&n| readable(rip, n)).unwrap_or(0);
            if remembers() {
                let mut known = known;
                known.rotate_right(1);
                known[0] = (rip, n);
                CODE.with(|c| c.set(known));
            }
            n
        }
    };
    // SAFETY: `n` bytes at `rip` are readable.
    let code = unsafe { std::slice::from_raw_parts(rip as *const u8, n) };
    let Some((what, len)) = decode::decode(code) else { return 0 };
    static SEEN: AtomicU16 = AtomicU16::new(0);
    if first_observation(&SEEN, what) {
        tracing::warn!(instruction = ?what, at = format_args!("{rip:#x}"), "a console CPU instruction this CPU lacks: emulated (logged once)");
    }
    let mut wait_at = None;
    match what {
        ConsoleOnly::Ud2 => return 0,
        ConsoleOnly::Clwb => {
            // nothing to write back on the PC: patched to NOPs, no exception next time
            // SAFETY: the instruction's own bytes, which were read.
            unsafe { host::patch(rip, len, 0x90) };
        }
        ConsoleOnly::Monitorx => {
            KINDS[K_MONITORX].fetch_add(1, Relaxed);
            MONITOR.with(|m| m.set((ctx.Rax as usize, ctx.Rsi)));
        }
        ConsoleOnly::Mwaitx => {
            KINDS[K_MWAITX].fetch_add(1, Relaxed);
            let now = clock_us();
            wait_step(now);
            // The rate line can use this same sample; it needs no second counter read.
            wait_at = Some(now);
            std::hint::spin_loop()
        }
        ConsoleOnly::Clzero | ConsoleOnly::ClzeroStep { .. } => {
            // The loop `mov rax, ptr; add ptr, 0x40; clzero; cmp ptr, end; jb` (the title's
            // +0x4e8ab4c and three more like it zero 896 to 7,040 bytes each, a line an
            // exception) is taken whole from the line it stands at.
            if let ConsoleOnly::ClzeroStep { ptr_reg, end_reg } = what
                && loop15()
                && let Some(done) = unsafe { clzero_loop15(ctx, rip, ptr_reg, end_reg) }
            {
                if !done {
                    return 0;
                }
                ctx.Rip += len as u64;
                EMULATED.fetch_add(1, Relaxed);
                KINDS[K_LOOP15].fetch_add(1, Relaxed);
                line(clock_us());
                return CONTINUE_EXECUTION;
            }
            KINDS[K_CLZERO].fetch_add(1, Relaxed);
            // with an address-size prefix (0x67) the line is at EAX, zero-extended
            let a32 = code[..len - 3].contains(&0x67);
            let line = (if a32 { ctx.Rax & 0xFFFF_FFFF } else { ctx.Rax } as usize) & !63;
            if !writable(line, 64) {
                return 0; // the real instruction faults there too
            }
            // SAFETY: the 64-byte line is committed and writable.
            unsafe { std::ptr::write_bytes(line as *mut u8, 0, 64) };
        }
        ConsoleOnly::ClzeroLoop { end_reg } => {
            KINDS[K_LOOP12].fetch_add(1, Relaxed);
            // the whole `clzero; add rax,40h; cmp rax,end; jb` loop at once
            let regs: [u64; 16] = [
                ctx.Rax, ctx.Rcx, ctx.Rdx, ctx.Rbx, ctx.Rsp, ctx.Rbp, ctx.Rsi, ctx.Rdi, ctx.R8, ctx.R9, ctx.R10, ctx.R11, ctx.R12, ctx.R13, ctx.R14, ctx.R15,
            ];
            let end = regs[end_reg as usize & 15] as usize;
            let start = ctx.Rax as usize;
            if end > start && end - start < 2 << 30 {
                // Region by region: a block may lie across several (the title's pages are
                // mapped one view each), and one answer for all of it was a refusal, after
                // which every 64-byte line took an exception of its own.
                let mut at = start;
                while at < end {
                    let Some(to) = writable_to(at, end) else { break };
                    // SAFETY: committed and writable from `at` to `to`.
                    unsafe { std::ptr::write_bytes(at as *mut u8, 0, to - at) };
                    at = to;
                }
                if at == start {
                    return 0;
                }
                ZEROED.fetch_add((at - start) as u64, Relaxed);
                if at < end {
                    // What follows is not writable: the loop goes on there (the same
                    // instruction again), where the real one faults too.
                    ctx.Rax = at as u64;
                    return CONTINUE_EXECUTION;
                }
            }
            ctx.Rax = end as u64;
        }
        ConsoleOnly::Rdpru => {
            let v = if ctx.Rcx as u32 <= 1 { host::counter() } else { 0 };
            ctx.Rax = v & 0xFFFF_FFFF;
            ctx.Rdx = v >> 32;
        }
        ConsoleOnly::Rdpid(r) => {
            let cpu = host::processor() as u64;
            let regs: [&mut u64; 16] = [
                &mut ctx.Rax, &mut ctx.Rcx, &mut ctx.Rdx, &mut ctx.Rbx, &mut ctx.Rsp, &mut ctx.Rbp, &mut ctx.Rsi, &mut ctx.Rdi, &mut ctx.R8, &mut ctx.R9, &mut ctx.R10, &mut ctx.R11, &mut ctx.R12, &mut ctx.R13,
                &mut ctx.R14, &mut ctx.R15,
            ];
            *regs[r as usize & 15] = cpu;
        }
        // the low quadword changes; the high one is zeroed, as a Zen 5 does
        ConsoleOnly::Extrq { dst, src, len, idx } => {
            // SAFETY: the context's XMM registers (CONTEXT_FLOATING_POINT is captured for exceptions).
            let x = unsafe { &mut ctx.Anonymous.FltSave.XmmRegisters };
            let (len, idx) = match src {
                Some(s) => {
                    let c = x[s as usize & 15].Low;
                    (c as u8, (c >> 8) as u8)
                }
                None => (len, idx),
            };
            let d = &mut x[dst as usize & 15];
            d.Low = decode::extrq(d.Low, len, idx);
            d.High = 0;
        }
        ConsoleOnly::Insertq { dst, src, reg, len, idx } => {
            // SAFETY: as above.
            let x = unsafe { &mut ctx.Anonymous.FltSave.XmmRegisters };
            let s = x[src as usize & 15];
            let (len, idx) = if reg { (s.High as u8, (s.High >> 8) as u8) } else { (len, idx) };
            let d = &mut x[dst as usize & 15];
            d.Low = decode::insertq(d.Low, s.Low, len, idx);
            d.High = 0;
        }
    }
    if !matches!(
        what,
        ConsoleOnly::Monitorx | ConsoleOnly::Mwaitx | ConsoleOnly::Clzero | ConsoleOnly::ClzeroStep { .. } | ConsoleOnly::ClzeroLoop { .. }
    ) {
        KINDS[K_OTHER].fetch_add(1, Relaxed);
    }
    line(wait_at.unwrap_or_else(clock_us));
    ctx.Rip += len as u64;
    // How often this runs at all (each one is an exception: a trip into Windows and back).
    let emulated = EMULATED.fetch_add(1, Relaxed) + 1;
    if emulated >= 1 << 16 && emulated.is_power_of_two() {
        tracing::info!(
            emulated,
            asked = ASKED.load(Relaxed),
            zeroed_mb = ZEROED.load(Relaxed) >> 20,
            "console CPU instructions emulated so far, the times Windows was asked about memory for them, and what clzero loops zeroed"
        );
    }
    CONTINUE_EXECUTION
}

/// A memory region as the host describes it.
#[derive(Debug, Clone, Copy)]
struct Region {
    from: usize,
    to: usize,
    readable: bool,
    writable: bool,
}

/// What the emulation asks of the host: Windows' own calls there, their
/// macOS counterparts under Rosetta.
#[cfg(windows)]
mod host {
    pub(super) use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_GUARD, PAGE_NOACCESS,
        PAGE_READWRITE, VirtualProtect, VirtualQuery,
    };

    /// The performance counter, and its ticks per second.
    pub(super) fn counter() -> u64 {
        let mut now = 0i64;
        // SAFETY: a valid out pointer.
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut now) };
        now as u64
    }

    pub(super) fn frequency() -> i64 {
        let mut f = 0i64;
        // SAFETY: a valid out pointer.
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut f) };
        f
    }

    pub(super) fn thread_id() -> u32 {
        // SAFETY: no preconditions.
        unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
    }

    pub(super) fn processor() -> u32 {
        // SAFETY: no preconditions.
        unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessorNumber() }
    }

    /// The committed region `va` lies in.
    pub(super) fn region(va: usize) -> Option<super::Region> {
        // SAFETY: plain data filled in by the query.
        let mut m: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer of the right size.
        if unsafe { VirtualQuery(va as *const _, &mut m, size_of_val(&m)) } == 0 {
            return None;
        }
        if m.State != MEM_COMMIT {
            return None;
        }
        let from = m.BaseAddress as usize;
        Some(super::Region {
            from,
            to: from + m.RegionSize,
            readable: m.Protect & (PAGE_NOACCESS | PAGE_GUARD) == 0,
            writable: m.Protect & (PAGE_READWRITE | PAGE_EXECUTE_READWRITE) != 0,
        })
    }

    /// Overwrites `len` code bytes at `at` with `byte`.
    ///
    /// # Safety
    /// `at..at+len` is one instruction of title code.
    pub(super) unsafe fn patch(at: usize, len: usize, byte: u8) {
        let mut old = 0u32;
        // SAFETY: the instruction's own bytes, made writable for the patch and restored.
        unsafe {
            if VirtualProtect(at as *const _, len, PAGE_EXECUTE_READWRITE, &mut old) != 0 {
                std::slice::from_raw_parts_mut(at as *mut u8, len).fill(byte);
                VirtualProtect(at as *const _, len, old, &mut old);
            }
        }
    }
}

#[cfg(not(windows))]
mod host {
    // Windows' own names, so the emulation reads the same on both hosts.
    #![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

    /// Windows x64's `CONTEXT`, with its field names, laid out as
    /// `crate::unwind::Context` (the layout title code reads).
    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    pub(crate) struct CONTEXT {
        pub P1Home: [u64; 6],
        pub ContextFlags: u32,
        pub MxCsr: u32,
        pub Seg: [u16; 6],
        pub EFlags: u32,
        pub Dr: [u64; 6],
        pub Rax: u64,
        pub Rcx: u64,
        pub Rdx: u64,
        pub Rbx: u64,
        pub Rsp: u64,
        pub Rbp: u64,
        pub Rsi: u64,
        pub Rdi: u64,
        pub R8: u64,
        pub R9: u64,
        pub R10: u64,
        pub R11: u64,
        pub R12: u64,
        pub R13: u64,
        pub R14: u64,
        pub R15: u64,
        pub Rip: u64,
        pub Anonymous: CONTEXT_0,
        pub VectorRegister: [[u64; 2]; 26],
        pub Rest: [u64; 6],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) union CONTEXT_0 {
        pub FltSave: XMM_SAVE_AREA32,
    }

    /// The FXSAVE layout: x87 state, then the 16 XMM registers at 0xA0.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct XMM_SAVE_AREA32 {
        pub Header: [u8; 32],
        pub FloatRegisters: [M128A; 8],
        pub XmmRegisters: [M128A; 16],
        pub Reserved4: [u8; 96],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct M128A {
        pub Low: u64,
        pub High: i64,
    }

    const _: () = assert!(size_of::<CONTEXT>() == size_of::<crate::unwind::Context>());
    const _: () = assert!(std::mem::offset_of!(CONTEXT, Rip) == 0xF8);
    const _: () = assert!(std::mem::offset_of!(CONTEXT, Anonymous) == 0x100);

    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut [u32; 2]) -> i32;
        fn pthread_threadid_np(thread: usize, id: *mut u64) -> i32;
        fn mach_vm_region(
            task: u32,
            address: *mut u64,
            size: *mut u64,
            flavor: i32,
            info: *mut i32,
            count: *mut u32,
            object: *mut u32,
        ) -> i32;
        static mach_task_self_: u32;
    }

    /// `VM_REGION_BASIC_INFO_64`, and its size in 32-bit words.
    const BASIC_INFO_64: i32 = 9;
    const BASIC_INFO_64_WORDS: u32 = 9;
    const VM_PROT_READ: i32 = 1;
    const VM_PROT_WRITE: i32 = 2;

    /// Mach absolute time, which ticks at `frequency()` per second.
    pub(super) fn counter() -> u64 {
        // SAFETY: no preconditions.
        unsafe { mach_absolute_time() }
    }

    pub(super) fn frequency() -> i64 {
        let mut base = [0u32; 2];
        // SAFETY: a valid out pointer to {numer, denom}.
        unsafe { mach_timebase_info(&mut base) };
        let [numer, denom] = base.map(|v| v.max(1) as i64);
        1_000_000_000 * denom / numer
    }

    pub(super) fn thread_id() -> u32 {
        let mut id = 0u64;
        // SAFETY: 0 names the calling thread; a valid out pointer.
        unsafe { pthread_threadid_np(0, &mut id) };
        id as u32
    }

    /// shortcut: macOS does not say which processor a thread runs on; every
    /// `rdpid` reads 0. Titles use it to spread work per processor.
    pub(super) fn processor() -> u32 {
        0
    }

    /// The mapped region `va` lies in (a reservation without access is not
    /// one, as Windows' reserved memory is not committed).
    pub(super) fn region(va: usize) -> Option<super::Region> {
        let (mut address, mut size) = (va as u64, 0u64);
        let mut info = [0i32; BASIC_INFO_64_WORDS as usize];
        let (mut count, mut object) = (BASIC_INFO_64_WORDS, 0u32);
        // SAFETY: out pointers of the sizes the flavor needs.
        let kr = unsafe {
            mach_vm_region(
                mach_task_self_,
                &mut address,
                &mut size,
                BASIC_INFO_64,
                info.as_mut_ptr(),
                &mut count,
                &mut object,
            )
        };
        // The call answers with the next region when `va` is in none.
        if kr != 0 || address > va as u64 || info[0] == 0 {
            return None;
        }
        Some(super::Region {
            from: address as usize,
            to: (address + size) as usize,
            readable: info[0] & VM_PROT_READ != 0,
            writable: info[0] & VM_PROT_WRITE != 0,
        })
    }

    /// Overwrites `len` code bytes at `at` with `byte`; Rosetta translates
    /// the page again after the protection changes back.
    ///
    /// # Safety
    /// `at..at+len` is one instruction of title code.
    pub(super) unsafe fn patch(at: usize, len: usize, byte: u8) {
        let page = at & !0xFFF;
        let span = (at + len).next_multiple_of(0x1000) - page;
        let rwx = libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC;
        // SAFETY: the instruction's own pages, writable for the patch, then
        // read and execute again as the loader left them.
        unsafe {
            if libc::mprotect(page as *mut _, span, rwx) == 0 {
                std::slice::from_raw_parts_mut(at as *mut u8, len).fill(byte);
                libc::mprotect(page as *mut _, span, libc::PROT_READ | libc::PROT_EXEC);
            }
        }
    }
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn counter_microseconds_match_wide_arithmetic() {
        // Tick boundaries, long uptimes, and counters that cannot use the 10 MHz path.
        let frequencies = [
            1,
            3,
            32_768,
            1_000_000,
            3_579_545,
            10_000_000,
            24_000_000,
            3_000_000_000,
            i64::MAX as u64,
        ];
        let check = |ticks: u64, frequency: u64| {
            let expected = (ticks as u128 * 1_000_000 / frequency as u128) as u64;
            assert_eq!(
                counter_us(ticks, frequency),
                expected,
                "{ticks} / {frequency}"
            );
        };
        for frequency in frequencies {
            for ticks in [
                0,
                1,
                9,
                10,
                11,
                frequency - 1,
                frequency,
                frequency + 1,
                u64::MAX / 1_000_000,
                u64::MAX / 1_000_000 + 1,
                i64::MAX as u64,
            ] {
                check(ticks, frequency);
            }
            let mut ticks = 7u64;
            for _ in 0..10_000 {
                ticks = ticks
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                check(ticks & i64::MAX as u64, frequency);
            }
        }
    }

    #[test]
    fn wait_tracking_uses_sampled_time_and_preserves_boundaries() {
        let old_monitor = MONITOR.with(|m| m.replace((1, 7)));
        let old_wait = WAIT.with(|w| w.replace((0, 0, 0, 0, 0)));
        wait_step(100);
        assert_eq!(WAIT.with(Cell::get), (100, 100, 1, 7, 1));
        wait_step(2_099); // A gap strictly below 2 ms continues the same wait.
        assert_eq!(WAIT.with(Cell::get), (100, 2_099, 1, 7, 2));
        wait_step(4_099); // Exactly 2 ms starts another wait.
        assert_eq!(WAIT.with(Cell::get), (4_099, 4_099, 1, 7, 1));
        MONITOR.with(|m| m.set((2, 7)));
        wait_step(4_100);
        assert_eq!(WAIT.with(Cell::get), (4_100, 4_100, 2, 7, 1));
        MONITOR.with(|m| m.set((2, 8)));
        wait_step(4_101);
        assert_eq!(WAIT.with(Cell::get), (4_101, 4_101, 2, 8, 1));
        wait_step(4_100); // Keep the existing saturating treatment of a backwards sample.
        assert_eq!(WAIT.with(Cell::get), (4_101, 4_100, 2, 8, 2));
        MONITOR.with(|m| m.set(old_monitor));
        WAIT.with(|w| w.set(old_wait));
    }

    #[test]
    fn monitor_wait_emulation_preserves_saved_context() {
        #[repr(align(16))]
        struct Saved(std::mem::MaybeUninit<CONTEXT>);

        let old_monitor = MONITOR.with(|m| m.replace((0, 0)));
        let old_wait = WAIT.with(|w| w.replace((0, 0, 0, 0, 0)));
        let old_code = CODE.with(|c| c.replace([(0, 0); 8]));
        let watched = AtomicU64::new(0x1234_5678_9abc_def0);
        for opcode in [0xfa, 0xfb] {
            let mut code = [0xccu8; 16];
            code[..3].copy_from_slice(&[0x0f, 0x01, opcode]);
            let mut saved = Saved(std::mem::MaybeUninit::uninit());
            let context = saved.0.as_mut_ptr();
            let size = size_of::<CONTEXT>();
            // SAFETY: CONTEXT contains integers and their arrays/unions, so every bit pattern
            // is valid. Initialize all storage, including padding, and never move a typed
            // CONTEXT value. Each temporary mutable reference ends before a byte snapshot.
            // The decoder reads our live code; MONITORX/MWAITX do not write watched memory.
            unsafe {
                std::ptr::write_bytes(context.cast::<u8>(), 0xa5, size);
                (*context).Rip = code.as_ptr() as u64;
                (*context).Rax = std::ptr::from_ref(&watched) as u64;
                let mut expected = std::slice::from_raw_parts(context.cast::<u8>(), size).to_vec();
                let rip = std::mem::offset_of!(CONTEXT, Rip);
                expected[rip..rip + 8].copy_from_slice(&((*context).Rip + 3).to_ne_bytes());
                assert_eq!(emulate(&mut *context), CONTINUE_EXECUTION);
                assert_eq!(
                    std::slice::from_raw_parts(context.cast::<u8>(), size),
                    expected,
                    "opcode {opcode:#x}: only the saved instruction pointer may change"
                );
            }
            assert_eq!(watched.load(Relaxed), 0x1234_5678_9abc_def0);
        }
        MONITOR.with(|m| m.set(old_monitor));
        WAIT.with(|w| w.set(old_wait));
        CODE.with(|c| c.set(old_code));
    }

    const WARNING_KINDS: [ConsoleOnly; 10] = [
        ConsoleOnly::Clwb,
        ConsoleOnly::Clzero,
        ConsoleOnly::ClzeroLoop { end_reg: 1 },
        ConsoleOnly::ClzeroStep { ptr_reg: 2, end_reg: 3 },
        ConsoleOnly::Rdpru,
        ConsoleOnly::Monitorx,
        ConsoleOnly::Mwaitx,
        ConsoleOnly::Rdpid(0),
        ConsoleOnly::Extrq { dst: 0, src: None, len: 8, idx: 16 },
        ConsoleOnly::Insertq { dst: 0, src: 1, reg: false, len: 8, idx: 16 },
    ];

    #[test]
    fn instruction_warnings_are_once_per_kind_not_per_operand() {
        let seen = AtomicU16::new(0);
        assert!(!first_observation(&seen, ConsoleOnly::Ud2));
        for instruction in WARNING_KINDS {
            assert!(first_observation(&seen, instruction), "{instruction:?}");
            assert!(!first_observation(&seen, instruction), "{instruction:?}");
        }
        for instruction in [
            ConsoleOnly::ClzeroLoop { end_reg: 15 },
            ConsoleOnly::ClzeroStep { ptr_reg: 10, end_reg: 11 },
            ConsoleOnly::Rdpid(15),
            ConsoleOnly::Extrq { dst: 15, src: Some(14), len: 0, idx: 0 },
            ConsoleOnly::Insertq { dst: 15, src: 14, reg: true, len: 0, idx: 0 },
            ConsoleOnly::Ud2,
        ] {
            assert!(!first_observation(&seen, instruction), "{instruction:?}");
        }
    }

    #[test]
    fn concurrent_instruction_observers_claim_each_warning_once() {
        let seen = AtomicU16::new(0);
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| {
                    let mut claimed = [0; WARNING_KINDS.len()];
                    for (count, instruction) in claimed.iter_mut().zip(WARNING_KINDS) {
                        start.wait();
                        for _ in 0..128 {
                            *count += u32::from(first_observation(&seen, instruction));
                        }
                    }
                    claimed
                }))
                .collect();
            let mut total = [0; WARNING_KINDS.len()];
            for worker in workers {
                for (total, claimed) in total.iter_mut().zip(worker.join().unwrap()) {
                    *total += claimed;
                }
            }
            assert_eq!(total, [1; WARNING_KINDS.len()]);
        });
    }

    /// Synthetic wait hints through the real vectored exception handler, with no game data.
    /// Run this test alone on a CPU without MONITORX, in release mode, on both revisions:
    /// `cargo test -p leonida-loader --release benchmark_monitor_wait_exceptions`,
    /// followed by `-- --ignored --nocapture --test-threads=1`.
    #[test]
    #[ignore = "manual CPU performance measurement; run alone on a CPU without MONITORX"]
    fn benchmark_monitor_wait_exceptions() {
        // The extended feature bit reports MONITORX/MWAITX.
        let native_wait = std::arch::x86_64::__cpuid(0x8000_0000).eax >= 0x8000_0001
            && std::arch::x86_64::__cpuid(0x8000_0001).ecx & (1 << 29) != 0;
        if native_wait {
            eprintln!("skipped: this CPU supports MONITORX/MWAITX without emulation");
            return;
        }
        crate::report_crashes();
        let mut frequency = 0i64;
        // SAFETY: a valid out pointer for the current performance-counter frequency.
        let available = unsafe {
            windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut frequency)
        };
        assert!(available != 0 && frequency > 0, "QPC frequency must be available");
        eprintln!("wait exceptions: QPC frequency={frequency} Hz");
        const WARMUP: u64 = 20_000;
        const PAIRS: u64 = 500_000;
        for threads in [1, 2, 4] {
            let start = std::sync::Barrier::new(threads);
            let before = EMULATED.load(Relaxed);
            let elapsed = std::thread::scope(|scope| {
                let workers: Vec<_> = (0..threads).map(|_| scope.spawn(|| {
                    let watched = AtomicU64::new(0);
                    let pair = || {
                        // SAFETY: the monitor address is ours and stays alive; the registered
                        // handler emulates these unsupported wait hints without changing it.
                        unsafe {
                            std::arch::asm!(
                                ".byte 0x0f, 0x01, 0xfa",
                                ".byte 0x0f, 0x01, 0xfb",
                                in("rax") std::ptr::from_ref(&watched),
                                in("ecx") 0,
                                in("edx") 0,
                                in("rsi") 1u64,
                                options(nostack),
                            );
                        }
                    };
                    for _ in 0..WARMUP {
                        pair();
                    }
                    start.wait();
                    let began = std::time::Instant::now();
                    for _ in 0..PAIRS {
                        pair();
                    }
                    let elapsed = began.elapsed();
                    assert_eq!(watched.load(Relaxed), 0);
                    elapsed
                })).collect();
                workers.into_iter().map(|worker| worker.join().unwrap()).max().unwrap()
            });
            assert_eq!(EMULATED.load(Relaxed) - before, 2 * (WARMUP + PAIRS) * threads as u64);
            let exceptions = 2 * PAIRS * threads as u64;
            eprintln!(
                "wait exceptions: threads={threads} count={exceptions} \
                 elapsed={elapsed:?} ns/exception={:.1}",
                elapsed.as_nanos() as f64 / exceptions as f64,
            );
        }
    }

    /// What the title's loop leaves, run line by line as the processor would: the lines it
    /// zeroed and the registers (rax, ptr) at the end, from the state at its first `clzero`.
    fn by_hand(mut rax: usize, mut ptr: usize, end: usize) -> (Vec<usize>, usize, usize) {
        let mut lines = vec![rax & !63];
        while ptr < end {
            rax = ptr;
            ptr += 0x40;
            lines.push(rax & !63);
        }
        (lines, rax, ptr)
    }

    #[test]
    fn the_15_byte_loop_ends_where_the_processor_would_end_it() {
        for (rax, len) in [(0x1000usize, 7040usize), (0x1000, 3264), (0x1000, 3776), (0x1000, 896), (0x4000, 64), (0x4000, 65), (0x4000, 100), (0x4000, 1), (0x4000, 0), (0x8000, 128)] {
            let (ptr, end) = (rax + 64, rax + len);
            let (lines, last_rax, last_ptr) = by_hand(rax, ptr, end);
            let last = loop15_end(rax, ptr, end).expect("the plain case");
            assert_eq!(last, last_ptr, "rax {rax:#x} len {len}");
            assert_eq!(last - 64, last_rax, "rax {rax:#x} len {len}");
            assert_eq!((last - rax) / 64, lines.len(), "rax {rax:#x} len {len}");
            assert_eq!(*lines.last().unwrap(), last - 64);
        }
        // an end before the pointer: the line at rax alone
        assert_eq!(loop15_end(0x1000, 0x1040, 0x800), Some(0x1040));
        // not the plain case: a line not on a boundary, a pointer that is not the next line
        assert_eq!(loop15_end(0x1008, 0x1048, 0x2000), None);
        assert_eq!(loop15_end(0x1000, 0x1080, 0x2000), None);
        assert_eq!(loop15_end(0x1000, 0x1040, 0x1000 + (3 << 30)), None, "over 2 GiB");
    }

    /// The loop as the title has it at +0x4e8ab4c, for the given registers.
    fn code(ptr_reg: u8, end_reg: u8) -> Vec<u8> {
        let (ph, pl, eh, el) = (ptr_reg >> 3, ptr_reg & 7, end_reg >> 3, end_reg & 7);
        vec![
            0x48 | ph << 2, 0x89, 0xC0 | pl << 3, // mov rax, ptr
            0x48 | ph, 0x83, 0xC0 | pl, 0x40, // add ptr, 0x40
            0x0F, 0x01, 0xFC, // clzero
            0x48 | eh << 2 | ph, 0x39, 0xC0 | el << 3 | pl, // cmp ptr, end
            0x72, 0xF1, // jb -15
            0x90,
        ]
    }

    #[test]
    fn the_15_byte_loop_is_zeroed_whole_and_leaves_the_loop_s_registers() {
        for (ptr_reg, end_reg, len) in [(10u8, 11u8, 7040usize), (9, 10, 3264), (8, 9, 3776), (2, 8, 896), (10, 11, 64), (10, 11, 100)] {
            let code = code(ptr_reg, end_reg);
            // a block of 0xAA with room on both sides, its start on a line boundary
            let mut block = vec![0xAAu8; 16384];
            let base = (block.as_mut_ptr() as usize + 4096 + 63) & !63;
            // SAFETY: plain data.
            let mut ctx: CONTEXT = unsafe { std::mem::zeroed() };
            ctx.Rip = code.as_ptr() as u64 + 7;
            ctx.Rax = base as u64;
            *register(&mut ctx, ptr_reg) = base as u64 + 64;
            *register(&mut ctx, end_reg) = (base + len) as u64;
            // SAFETY: the context points at the code above.
            assert_eq!(unsafe { emulate(&mut ctx) }, CONTINUE_EXECUTION);
            let (lines, last_rax, last_ptr) = by_hand(base, base + 64, base + len);
            assert_eq!(ctx.Rip, code.as_ptr() as u64 + 10, "at the loop's own cmp");
            assert_eq!(ctx.Rax as usize, last_rax);
            assert_eq!(*register(&mut ctx, ptr_reg) as usize, last_ptr);
            assert_eq!(*register(&mut ctx, end_reg) as usize, base + len, "the end is left alone");
            let zeroed = lines.len() * 64;
            let at = base - block.as_ptr() as usize;
            assert!(block[at..at + zeroed].iter().all(|&b| b == 0), "{zeroed} bytes zeroed");
            assert!(block[..at].iter().all(|&b| b == 0xAA), "nothing before the block");
            assert!(block[at + zeroed..].iter().all(|&b| b == 0xAA), "nothing after its last line");
        }
    }

    #[test]
    fn a_clzero_that_only_looks_like_the_loop_is_one_line() {
        // the same bytes behind it, another head in front (add ptr, 0x80): one line, one step
        let mut code = code(10, 11);
        code[6] = 0x80;
        let mut block = vec![0xAAu8; 8192];
        let base = (block.as_mut_ptr() as usize + 2048 + 63) & !63;
        // SAFETY: plain data.
        let mut ctx: CONTEXT = unsafe { std::mem::zeroed() };
        ctx.Rip = code.as_ptr() as u64 + 7;
        ctx.Rax = base as u64;
        ctx.R10 = base as u64 + 64;
        ctx.R11 = base as u64 + 640;
        // SAFETY: the context points at the code above.
        assert_eq!(unsafe { emulate(&mut ctx) }, CONTINUE_EXECUTION);
        assert_eq!(ctx.Rip, code.as_ptr() as u64 + 10);
        assert_eq!((ctx.Rax, ctx.R10), (base as u64, base as u64 + 64), "registers untouched");
        let at = base - block.as_ptr() as usize;
        assert!(block[at..at + 64].iter().all(|&b| b == 0));
        assert!(block[at + 64..].iter().all(|&b| b == 0xAA), "one line only");
    }
}
