//! Windows structured exceptions on macOS.
//!
//! On Windows the kernel turns a fault into an exception and ntdll
//! dispatches it. Here a signal handler does the first part: for a fault in
//! a mapped title image it builds the `EXCEPTION_RECORD` and `CONTEXT`,
//! puts them on the faulting thread's stack and returns into [`dispatch`],
//! which runs like ordinary code. Dispatch asks the vectored handlers, then
//! walks the title's frames with their unwind info ([`crate::unwind`]) and
//! calls each language handler, as `RtlDispatchException` does. The ntdll
//! and kernel32 functions those handlers and the CRT call (`RtlUnwindEx`,
//! `RaiseException`, `RtlCaptureContext` and the rest) are [`export`]ed for
//! the resolver to bind.
//!
//! Our own frames have no Windows unwind info. Where a walk reaches one (a
//! language handler called by [`dispatch`], or by `RtlUnwindEx`), it goes on
//! from the context of the exception being dispatched on this thread, which
//! is where Windows' walk through ntdll's frames would lead.

use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::unwind::{
    self, CONTEXT_FULL, Context, RuntimeFunction, UNW_FLAG_EHANDLER, UNW_FLAG_UHANDLER,
};

const RAX: usize = 0;
const RCX: usize = 1;
const RDX: usize = 2;
const R8: usize = 8;
const R9: usize = 9;

const EXCEPTION_NONCONTINUABLE: u32 = 0x01;
const EXCEPTION_UNWINDING: u32 = 0x02;
const EXCEPTION_EXIT_UNWIND: u32 = 0x04;
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;

const STATUS_BREAKPOINT: u32 = 0x8000_0003;
const STATUS_SINGLE_STEP: u32 = 0x8000_0004;
const STATUS_UNWIND_CONSOLIDATE: u32 = 0x8000_0029;
const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
const STATUS_UNWIND: u32 = 0xC000_0027;
const STATUS_FLOAT_INVALID_OPERATION: u32 = 0xC000_0090;
const STATUS_INTEGER_DIVIDE_BY_ZERO: u32 = 0xC000_0094;
const STATUS_INTEGER_OVERFLOW: u32 = 0xC000_0095;

/// A vectored handler's "resume at the context as it now is".
const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
/// `EXCEPTION_DISPOSITION::ExceptionContinueExecution`.
const DISPOSITION_CONTINUE_EXECUTION: u32 = 0;

/// `EXCEPTION_RECORD` (0x98 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExceptionRecord {
    pub code: u32,
    pub flags: u32,
    pub record: u64,
    pub address: u64,
    pub parameters: u32,
    pub info: [u64; 15],
}

const _: () = assert!(size_of::<ExceptionRecord>() == 0x98);

impl ExceptionRecord {
    fn new(code: u32, address: u64) -> Self {
        ExceptionRecord {
            code,
            flags: 0,
            record: 0,
            address,
            parameters: 0,
            info: [0; 15],
        }
    }
}

#[repr(C)]
struct ExceptionPointers {
    record: *mut ExceptionRecord,
    context: *mut Context,
}

/// `DISPATCHER_CONTEXT`, which language handlers read.
#[repr(C)]
struct DispatcherContext {
    control_pc: u64,
    image_base: u64,
    function_entry: *const RuntimeFunction,
    establisher_frame: u64,
    target_ip: u64,
    context_record: *mut Context,
    language_handler: u64,
    handler_data: u64,
    history_table: u64,
    scope_index: u32,
    fill0: u32,
}

type LanguageHandler =
    extern "win64" fn(*mut ExceptionRecord, u64, *mut Context, *mut DispatcherContext) -> u32;
type VectoredHandler = extern "win64" fn(*mut ExceptionPointers) -> i32;

// The images whose code may fault into us, readable from a signal handler:
// base, size, `.pdata` address and entry count. Written under `REGISTER`.
const MAX_IMAGES: usize = 64;
static IMAGES: [[AtomicUsize; 4]; MAX_IMAGES] =
    [const { [const { AtomicUsize::new(0) }; 4] }; MAX_IMAGES];
static IMAGE_COUNT: AtomicUsize = AtomicUsize::new(0);
static REGISTER: Mutex<()> = Mutex::new(());

/// Makes faults in `base..base + size` Windows exceptions, unwound with the
/// `.pdata` at `pdata` (`count` entries). Installs the signal handlers the
/// first time.
pub(crate) fn register(base: usize, size: usize, pdata: usize, count: usize) {
    let _held = REGISTER.lock().unwrap_or_else(|e| e.into_inner());
    let i = IMAGE_COUNT.load(Ordering::Acquire);
    assert!(i < MAX_IMAGES, "more than {MAX_IMAGES} title images");
    for (slot, value) in IMAGES[i].iter().zip([base, size, pdata, count]) {
        slot.store(value, Ordering::Relaxed);
    }
    IMAGE_COUNT.store(i + 1, Ordering::Release);
    if i == 0 {
        install();
    }
}

/// The image holding `pc`: its base and `.pdata`.
fn image_of(pc: u64) -> Option<(u64, &'static [RuntimeFunction])> {
    let pc = pc as usize;
    IMAGES[..IMAGE_COUNT.load(Ordering::Acquire)]
        .iter()
        .find_map(|image| {
            let [base, size, pdata, count] = image.each_ref().map(|v| v.load(Ordering::Relaxed));
            (pc.wrapping_sub(base) < size).then(|| {
                // SAFETY: registered from the mapped image's exception directory,
                // which stays mapped for the process's life.
                let pdata = unsafe { std::slice::from_raw_parts(pdata as *const _, count) };
                (base as u64, pdata)
            })
        })
}

/// The base of the mapped image holding `pc` (`RtlPcToFileHeader`).
pub fn image_base(pc: usize) -> Option<usize> {
    image_of(pc as u64).map(|(base, _)| base as usize)
}

/// With `LEONIDA_TRACE_IMPORTS=1`: the imports the title called last.
fn print_last_imports() {
    let last = crate::thunks::trace::last_imports();
    for name in &last[last.len().saturating_sub(24)..] {
        eprintln!("  called       {name}");
    }
}

/// `module+rva` for a log line.
pub fn locate_address(address: usize) -> String {
    locate(address as u64)
}

/// `module+rva` for a crash report.
fn locate(address: u64) -> String {
    match image_of(address) {
        Some((base, _)) => format!("title+{:#x}", address - base),
        None => format!("{address:#x}"),
    }
}

thread_local! {
    /// The exceptions being dispatched on this thread, innermost last.
    static DISPATCHES: RefCell<Vec<*const Context>> = const { RefCell::new(Vec::new()) };
}

/// The context of the exception whose dispatch called the frames at `rsp`:
/// the innermost one above them.
fn dispatch_above(rsp: u64) -> Option<Context> {
    DISPATCHES.with_borrow(|d| {
        d.iter()
            .copied()
            // SAFETY: each entry is a context on this thread's stack, above
            // the frames still running; `forget_dispatches_below` drops them
            // before those frames are abandoned.
            .map(|c| unsafe { *c })
            .filter(|c| c.rsp() > rsp)
            .min_by_key(|c| c.rsp())
    })
}

/// Drops the dispatches whose frames end below `rsp`: resuming there
/// abandons them.
fn forget_dispatches_below(rsp: u64) {
    DISPATCHES.with_borrow_mut(|d| {
        // SAFETY: as in `dispatch_above`.
        d.retain(|&c| unsafe { (*c).rsp() } >= rsp)
    });
}

/// One step up the stack from `c`. `None` at the top of the title's frames.
/// Returns the function's unwind result, for its handler.
fn step(
    kind: u8,
    c: &mut Context,
) -> Option<Option<(u64, &'static RuntimeFunction, unwind::Unwound)>> {
    let Some((base, pdata)) = image_of(c.rip) else {
        *c = dispatch_above(c.rsp())?;
        return Some(None);
    };
    let rva = (c.rip - base) as u32;
    match unwind::lookup(pdata, rva) {
        // SAFETY: `c` is a frame of this image on this thread's stack.
        Some(f) => Some(Some((base, f, unsafe {
            unwind::virtual_unwind(kind, base, f, c)
        }))),
        None => {
            // SAFETY: a leaf function's return address is at rsp.
            unsafe { unwind::unwind_leaf(c) };
            Some(None)
        }
    }
}

/// Dispatches an exception raised with `ctx`, and resumes where a handler
/// says; never returns.
fn dispatch(record: &mut ExceptionRecord, ctx: &mut Context) -> ! {
    // The console's CPU has instructions Rosetta's lacks; on Windows the
    // crash reporter's vectored handler asks the same first.
    if record.code == STATUS_ILLEGAL_INSTRUCTION {
        // SAFETY: the faulting thread's context, laid out as Windows' CONTEXT.
        let as_windows = unsafe { &mut *(ctx as *mut Context).cast() };
        // SAFETY: the exception being dispatched.
        if unsafe { crate::console_insn::emulate(as_windows) } == EXCEPTION_CONTINUE_EXECUTION {
            restore(ctx);
        }
    }
    DISPATCHES.with_borrow_mut(|d| d.push(ctx as *const Context));
    let handlers = VECTORED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for (_, handler) in handlers {
        let mut pointers = ExceptionPointers {
            record,
            context: ctx,
        };
        if handler(&mut pointers) == EXCEPTION_CONTINUE_EXECUTION {
            forget_dispatches_below(ctx.rsp() + 1);
            restore(ctx);
        }
    }

    let mut c = *ctx;
    loop {
        let frame = c;
        let Some(stepped) = step(UNW_FLAG_EHANDLER, &mut c) else {
            break;
        };
        let Some((base, function, unwound)) = stepped else {
            continue;
        };
        if let Some((handler, data)) = unwound.handler {
            let mut frame = frame;
            let mut dc = DispatcherContext {
                control_pc: frame.rip,
                image_base: base,
                function_entry: function,
                establisher_frame: unwound.establisher_frame,
                target_ip: 0,
                context_record: &mut frame,
                language_handler: handler,
                handler_data: data,
                history_table: 0,
                scope_index: 0,
                fill0: 0,
            };
            // SAFETY: the frame's unwind info names this language handler.
            let handler: LanguageHandler = unsafe { std::mem::transmute(handler) };
            if handler(record, unwound.establisher_frame, ctx, &mut dc)
                == DISPOSITION_CONTINUE_EXECUTION
            {
                if record.flags & EXCEPTION_NONCONTINUABLE != 0 {
                    unhandled(record, "a handler continued a noncontinuable exception");
                }
                forget_dispatches_below(ctx.rsp() + 1);
                restore(ctx);
            }
            // shortcut: nested and collided dispositions are taken as "continue
            // search"; handle them when a title's handlers return them.
        }
        if c.rsp() <= frame.rsp() {
            break; // no progress: a broken stack
        }
    }
    // `LEONIDA_SKIP_BREAKPOINTS=1` (off): a breakpoint no handler takes (a
    // title's own check) is stepped over, as a debugger does when told to go
    // on, to see how far the title gets past it. Each one is said.
    if record.code == STATUS_BREAKPOINT && skip_breakpoints() {
        eprintln!("Leonida Legacy: a breakpoint of the title at {} stepped over", locate(ctx.rip));
        forget_dispatches_below(ctx.rsp() + 1);
        ctx.rip += 1;
        restore(ctx);
    }
    unhandled(record, "no handler took it");
}

fn skip_breakpoints() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LEONIDA_SKIP_BREAKPOINTS").is_some_and(|v| v == "1"))
}

fn unhandled(record: &ExceptionRecord, why: &str) -> ! {
    let (code, at) = (record.code, locate(record.address));
    let mut text = format!("Leonida Legacy: unhandled exception {code:#010x} at {at} ({why})");
    if record.code == STATUS_ACCESS_VIOLATION {
        text += &format!(", accessing {:#x}", record.info[1]);
    }
    eprintln!("{text}");
    print_last_imports();
    std::process::abort();
}

/// Resumes execution at `ctx`.
fn restore(ctx: &Context) -> ! {
    let mut ctx = *ctx;
    ctx.eflags &= 0x0CD5; // the status flags and DF; no trap flag
    // SAFETY: `ctx` is a context of this thread to resume at.
    unsafe { restore_context(&ctx) }
}

/// Loads every register from `ctx` and jumps to its rip. The return address
/// and flags are written just below the target rsp, which no live frame of
/// the target uses.
#[unsafe(naked)]
unsafe extern "C" fn restore_context(ctx: *const Context) -> ! {
    std::arch::naked_asm!(
        "movdqu xmm0, [rdi + 0x1A0]",
        "movdqu xmm1, [rdi + 0x1B0]",
        "movdqu xmm2, [rdi + 0x1C0]",
        "movdqu xmm3, [rdi + 0x1D0]",
        "movdqu xmm4, [rdi + 0x1E0]",
        "movdqu xmm5, [rdi + 0x1F0]",
        "movdqu xmm6, [rdi + 0x200]",
        "movdqu xmm7, [rdi + 0x210]",
        "movdqu xmm8, [rdi + 0x220]",
        "movdqu xmm9, [rdi + 0x230]",
        "movdqu xmm10, [rdi + 0x240]",
        "movdqu xmm11, [rdi + 0x250]",
        "movdqu xmm12, [rdi + 0x260]",
        "movdqu xmm13, [rdi + 0x270]",
        "movdqu xmm14, [rdi + 0x280]",
        "movdqu xmm15, [rdi + 0x290]",
        "ldmxcsr [rdi + 0x34]",
        "mov rax, [rdi + 0x98]",
        "sub rax, 16",
        "mov rcx, [rdi + 0xF8]",
        "mov [rax + 8], rcx",
        "mov ecx, [rdi + 0x44]",
        "mov [rax], rcx",
        "mov rcx, [rdi + 0x80]",
        "mov rdx, [rdi + 0x88]",
        "mov rbx, [rdi + 0x90]",
        "mov rbp, [rdi + 0xA0]",
        "mov rsi, [rdi + 0xA8]",
        "mov r8, [rdi + 0xB8]",
        "mov r9, [rdi + 0xC0]",
        "mov r10, [rdi + 0xC8]",
        "mov r11, [rdi + 0xD0]",
        "mov r12, [rdi + 0xD8]",
        "mov r13, [rdi + 0xE0]",
        "mov r14, [rdi + 0xE8]",
        "mov r15, [rdi + 0xF0]",
        "mov rsp, rax",
        "mov rax, [rdi + 0x78]",
        "mov rdi, [rdi + 0xB0]",
        "popfq",
        "ret",
    )
}

/// A title-callable (Win64) function whose body is `$imp`, a System V
/// `fn(&mut Context) -> u64` given the caller's context as it was at the
/// call: rip is the return address, rsp the caller's after it returns, and
/// the arguments are in rcx, rdx, r8, r9 and at rsp + 0x20.
macro_rules! with_caller_context {
    ($name:ident, $imp:path) => {
        #[unsafe(naked)]
        extern "win64" fn $name() {
            std::arch::naked_asm!(
                "sub rsp, 0x4D8",
                "mov [rsp + 0x78], rax",
                "mov [rsp + 0x80], rcx",
                "mov [rsp + 0x88], rdx",
                "mov [rsp + 0x90], rbx",
                "mov [rsp + 0xA0], rbp",
                "mov [rsp + 0xA8], rsi",
                "mov [rsp + 0xB0], rdi",
                "mov [rsp + 0xB8], r8",
                "mov [rsp + 0xC0], r9",
                "mov [rsp + 0xC8], r10",
                "mov [rsp + 0xD0], r11",
                "mov [rsp + 0xD8], r12",
                "mov [rsp + 0xE0], r13",
                "mov [rsp + 0xE8], r14",
                "mov [rsp + 0xF0], r15",
                "lea rax, [rsp + 0x4E0]",
                "mov [rsp + 0x98], rax",
                "mov rax, [rsp + 0x4D8]",
                "mov [rsp + 0xF8], rax",
                "pushfq",
                "pop rax",
                "mov [rsp + 0x44], eax",
                "stmxcsr [rsp + 0x34]",
                "stmxcsr [rsp + 0x118]",
                "movdqu [rsp + 0x1A0], xmm0",
                "movdqu [rsp + 0x1B0], xmm1",
                "movdqu [rsp + 0x1C0], xmm2",
                "movdqu [rsp + 0x1D0], xmm3",
                "movdqu [rsp + 0x1E0], xmm4",
                "movdqu [rsp + 0x1F0], xmm5",
                "movdqu [rsp + 0x200], xmm6",
                "movdqu [rsp + 0x210], xmm7",
                "movdqu [rsp + 0x220], xmm8",
                "movdqu [rsp + 0x230], xmm9",
                "movdqu [rsp + 0x240], xmm10",
                "movdqu [rsp + 0x250], xmm11",
                "movdqu [rsp + 0x260], xmm12",
                "movdqu [rsp + 0x270], xmm13",
                "movdqu [rsp + 0x280], xmm14",
                "movdqu [rsp + 0x290], xmm15",
                "mov dword ptr [rsp + 0x30], {full}",
                "mov rdi, rsp",
                "call {imp}",
                // Win64 callers keep rsi, rdi and xmm6-15; System V code doesn't.
                "mov rsi, [rsp + 0xA8]",
                "mov rdi, [rsp + 0xB0]",
                "movdqu xmm6, [rsp + 0x200]",
                "movdqu xmm7, [rsp + 0x210]",
                "movdqu xmm8, [rsp + 0x220]",
                "movdqu xmm9, [rsp + 0x230]",
                "movdqu xmm10, [rsp + 0x240]",
                "movdqu xmm11, [rsp + 0x250]",
                "movdqu xmm12, [rsp + 0x260]",
                "movdqu xmm13, [rsp + 0x270]",
                "movdqu xmm14, [rsp + 0x280]",
                "movdqu xmm15, [rsp + 0x290]",
                "add rsp, 0x4D8",
                "ret",
                full = const CONTEXT_FULL,
                imp = sym $imp,
            )
        }
    };
}

with_caller_context!(rtl_capture_context, capture_context);
with_caller_context!(rtl_unwind_ex, unwind_ex);
with_caller_context!(rtl_unwind, unwind_ex);
with_caller_context!(raise_exception, raise);
with_caller_context!(rtl_raise_exception, raise_record);
with_caller_context!(rtl_restore_context, restore_given);

/// `RtlCaptureContext(CONTEXT*)`.
extern "C" fn capture_context(caller: &mut Context) -> u64 {
    // SAFETY: the title passes a CONTEXT to fill.
    unsafe { (caller.gpr[RCX] as *mut Context).write_unaligned(*caller) };
    0
}

/// `RtlRestoreContext(CONTEXT*, EXCEPTION_RECORD*)`.
extern "C" fn restore_given(caller: &mut Context) -> u64 {
    // SAFETY: the title passes a CONTEXT to resume at.
    let ctx = unsafe { (caller.gpr[RCX] as *const Context).read_unaligned() };
    forget_dispatches_below(ctx.rsp() + 1);
    restore(&ctx)
}

/// `RaiseException(code, flags, count, arguments)`.
extern "C" fn raise(caller: &mut Context) -> u64 {
    let mut record = ExceptionRecord::new(caller.gpr[RCX] as u32, caller.rip);
    record.flags = caller.gpr[RDX] as u32 & EXCEPTION_NONCONTINUABLE;
    let (count, arguments) = (
        (caller.gpr[R8] as usize).min(15),
        caller.gpr[R9] as *const u64,
    );
    if !arguments.is_null() {
        record.parameters = count as u32;
        for (i, slot) in record.info[..count].iter_mut().enumerate() {
            // SAFETY: the title passes `count` arguments.
            *slot = unsafe { arguments.add(i).read_unaligned() };
        }
    }
    let mut ctx = *caller;
    dispatch(&mut record, &mut ctx)
}

/// `RtlRaiseException(EXCEPTION_RECORD*)`.
extern "C" fn raise_record(caller: &mut Context) -> u64 {
    // SAFETY: the title passes a record.
    let mut record = unsafe { (caller.gpr[RCX] as *const ExceptionRecord).read_unaligned() };
    record.address = caller.rip;
    let mut ctx = *caller;
    dispatch(&mut record, &mut ctx)
}

/// `RtlUnwindEx(target_frame, target_ip, record, return_value, context,
/// history)` and `RtlUnwind` (the first four): runs the unwind handlers of
/// every frame from the caller up to `target_frame`, then resumes in that
/// frame at `target_ip` with `return_value` in rax.
extern "C" fn unwind_ex(caller: &mut Context) -> u64 {
    let (target_frame, target_ip) = (caller.gpr[RCX], caller.gpr[RDX]);
    let mut local = ExceptionRecord::new(STATUS_UNWIND, caller.rip);
    let record = match caller.gpr[R8] as *mut ExceptionRecord {
        // SAFETY: the title passes its exception record.
        record if !record.is_null() => unsafe { &mut *record },
        _ => &mut local,
    };
    record.flags |= EXCEPTION_UNWINDING;
    if target_frame == 0 {
        record.flags |= EXCEPTION_EXIT_UNWIND;
    }
    if record.code == STATUS_UNWIND_CONSOLIDATE {
        // shortcut: consolidated unwinds (C++ catch blocks of __CxxFrameHandler4)
        // are not run yet; implement them before titles with C++ exceptions.
        unhandled(record, "STATUS_UNWIND_CONSOLIDATE is not handled yet");
    }

    let mut c = *caller;
    loop {
        let frame = c;
        let Some(stepped) = step(UNW_FLAG_UHANDLER, &mut c) else {
            unhandled(record, "the unwind target frame is not on the stack");
        };
        let Some((base, function, unwound)) = stepped else {
            continue;
        };
        let at_target = unwound.establisher_frame == target_frame;
        if target_frame != 0 && unwound.establisher_frame > target_frame {
            unhandled(record, "unwound past the target frame");
        }
        if at_target {
            record.flags |= EXCEPTION_TARGET_UNWIND;
        }
        let mut frame = frame;
        if let Some((handler, data)) = unwound.handler {
            let mut dc = DispatcherContext {
                control_pc: frame.rip,
                image_base: base,
                function_entry: function,
                establisher_frame: unwound.establisher_frame,
                target_ip,
                context_record: &mut frame,
                language_handler: handler,
                handler_data: data,
                history_table: 0,
                scope_index: 0,
                fill0: 0,
            };
            // SAFETY: the frame's unwind info names this language handler.
            let handler: LanguageHandler = unsafe { std::mem::transmute(handler) };
            let context: *mut Context = dc.context_record;
            handler(record, unwound.establisher_frame, context, &mut dc);
        }
        if at_target {
            frame.rip = target_ip;
            frame.gpr[RAX] = caller.gpr[R9];
            forget_dispatches_below(frame.rsp());
            restore(&frame);
        }
    }
}

/// `RtlLookupFunctionEntry(pc, image_base*, history)`.
extern "win64" fn rtl_lookup_function_entry(
    pc: u64,
    image_base: *mut u64,
    _history: *mut u8,
) -> *const RuntimeFunction {
    let Some((base, pdata)) = image_of(pc) else {
        return std::ptr::null();
    };
    if !image_base.is_null() {
        // SAFETY: the title passes a u64 to fill.
        unsafe { image_base.write_unaligned(base) };
    }
    unwind::lookup(pdata, (pc - base) as u32).map_or(std::ptr::null(), |f| f as *const _)
}

/// `RtlVirtualUnwind(type, base, pc, function, context, handler_data*,
/// establisher_frame*, context_pointers)`.
#[allow(clippy::too_many_arguments)]
extern "win64" fn rtl_virtual_unwind(
    kind: u32,
    base: u64,
    pc: u64,
    function: *const RuntimeFunction,
    ctx: *mut Context,
    handler_data: *mut u64,
    establisher_frame: *mut u64,
    _context_pointers: *mut u8,
) -> u64 {
    // SAFETY: the title passes its CONTEXT and a function entry of `base`.
    let (ctx, function) = unsafe { (&mut *ctx, &*function) };
    ctx.rip = pc;
    // SAFETY: as above; the title's stack is readable.
    let unwound = unsafe { unwind::virtual_unwind(kind as u8, base, function, ctx) };
    // SAFETY: the title passes u64s to fill.
    unsafe {
        if !establisher_frame.is_null() {
            establisher_frame.write_unaligned(unwound.establisher_frame);
        }
        match unwound.handler {
            Some((handler, data)) => {
                if !handler_data.is_null() {
                    handler_data.write_unaligned(data);
                }
                handler
            }
            None => 0,
        }
    }
}

type Vectored = Vec<(usize, VectoredHandler)>;
static VECTORED: Mutex<Vectored> = Mutex::new(Vec::new());
static NEXT_HANDLE: AtomicUsize = AtomicUsize::new(1);

/// `AddVectoredExceptionHandler(first, handler)`: returns a handle.
extern "win64" fn add_vectored_exception_handler(first: u32, handler: VectoredHandler) -> usize {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let mut handlers = VECTORED.lock().unwrap_or_else(|e| e.into_inner());
    let at = if first != 0 { 0 } else { handlers.len() };
    handlers.insert(at, (handle, handler));
    handle
}

/// `RemoveVectoredExceptionHandler(handle)`.
extern "win64" fn remove_vectored_exception_handler(handle: usize) -> u32 {
    let mut handlers = VECTORED.lock().unwrap_or_else(|e| e.into_inner());
    let before = handlers.len();
    handlers.retain(|&(h, _)| h != handle);
    (handlers.len() != before) as u32
}

/// Our implementation of an exception-handling import, for the resolver:
/// `dll` is ntdll, kernel32, kernelbase or one of their API sets.
pub fn export(dll: &str, name: &str) -> Option<usize> {
    let dll = dll.to_ascii_lowercase();
    let ours = ["ntdll.dll", "kernel32.dll", "kernelbase.dll"].contains(&dll.as_str())
        || dll.starts_with("api-ms-win-core-");
    if !ours {
        return None;
    }
    let address = match name {
        "RtlCaptureContext" => rtl_capture_context as *const () as usize,
        "RtlRestoreContext" => rtl_restore_context as *const () as usize,
        "RtlUnwindEx" => rtl_unwind_ex as *const () as usize,
        "RtlUnwind" => rtl_unwind as *const () as usize,
        "RaiseException" => raise_exception as *const () as usize,
        "RtlRaiseException" => rtl_raise_exception as *const () as usize,
        "RtlLookupFunctionEntry" => rtl_lookup_function_entry as *const () as usize,
        "RtlVirtualUnwind" => rtl_virtual_unwind as *const () as usize,
        "AddVectoredExceptionHandler" => add_vectored_exception_handler as *const () as usize,
        "RemoveVectoredExceptionHandler" => remove_vectored_exception_handler as *const () as usize,
        _ => return None,
    };
    Some(address)
}

// `si_code` of SIGFPE in macOS's <sys/signal.h>; the libc crate lacks them.
const FPE_INTDIV: i32 = 7;
const FPE_INTOVF: i32 = 8;
const SIGNALS: [i32; 5] = [
    libc::SIGSEGV,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
    libc::SIGTRAP,
];
// What was installed before us, for faults outside title code.
static PREVIOUS: Mutex<Vec<(i32, libc::sigaction)>> = Mutex::new(Vec::new());

fn install() {
    let mut previous = PREVIOUS.lock().unwrap_or_else(|e| e.into_inner());
    for signal in SIGNALS {
        // SAFETY: a plain sigaction call with a handler of the SA_SIGINFO shape.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO;
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, &action, &mut old);
            previous.push((signal, old));
        }
    }
}

/// Turns a fault in title code into a Windows exception: builds the record
/// and context on the faulting thread's stack, below what it uses, and
/// returns into [`dispatch_from_signal`] there.
extern "C" fn on_signal(signal: i32, info: *mut libc::siginfo_t, uc: *mut libc::c_void) {
    // SAFETY: the kernel passes a valid siginfo and ucontext to an
    // SA_SIGINFO handler.
    let (info, mc) = unsafe { (&*info, &mut *(*(uc as *mut libc::ucontext_t)).uc_mcontext) };
    let ss = &mut mc.__ss;
    if image_of(ss.__rip).is_none() {
        // Not ours: put the previous handler back, and the fault happens
        // again under it when this returns.
        if crate::thunks::trace::on() {
            // SAFETY: plain data filled in by the call.
            let mut where_: libc::Dl_info = unsafe { std::mem::zeroed() };
            // SAFETY: an address and an out structure.
            unsafe { libc::dladdr(ss.__rip as *const _, &mut where_) };
            let name = if where_.dli_sname.is_null() {
                String::new()
            } else {
                // SAFETY: a symbol name of the host's, NUL-terminated.
                unsafe { std::ffi::CStr::from_ptr(where_.dli_sname) }.to_string_lossy().into_owned()
            };
            eprintln!(
                "Leonida Legacy: signal {signal} outside the title at {:#x} ({name}), accessing {:#x}",
                ss.__rip, info.si_addr as u64
            );
            print_last_imports();
        }
        if let Ok(previous) = PREVIOUS.try_lock()
            && let Some((_, old)) = previous.iter().find(|(s, _)| *s == signal)
        {
            // SAFETY: restoring the action that was installed before ours.
            unsafe { libc::sigaction(signal, old, std::ptr::null_mut()) };
        }
        return;
    }

    let mut ctx = Context::zeroed();
    ctx.context_flags = CONTEXT_FULL;
    ctx.gpr = [
        ss.__rax, ss.__rcx, ss.__rdx, ss.__rbx, ss.__rsp, ss.__rbp, ss.__rsi, ss.__rdi, ss.__r8,
        ss.__r9, ss.__r10, ss.__r11, ss.__r12, ss.__r13, ss.__r14, ss.__r15,
    ];
    ctx.rip = ss.__rip;
    ctx.eflags = ss.__rflags as u32;
    ctx.mx_csr = mc.__fs.__fpu_mxcsr;
    let xmm = &mc.__fs.__fpu_xmm0 as *const _ as *const [u8; 16];
    for n in 0..16 {
        // SAFETY: the 16 XMM fields of the float state follow each other.
        ctx.set_xmm(n, unsafe { xmm.add(n).read_unaligned() });
    }

    // `int 0x29` is `__fastfail(code)`: on Windows the process ends at once,
    // no handler is asked, and the reason is in rcx. The host raises a
    // segment fault for it.
    // SAFETY: the faulting instruction lies in a mapped image (checked above).
    if signal == libc::SIGSEGV && unsafe { (ss.__rip as *const [u8; 2]).read_unaligned() } == [0xCD, 0x29] {
        let text = format!(
            "Leonida Legacy: the title failed fast (__fastfail, reason {}) at {}",
            ss.__rcx,
            locate(ss.__rip)
        );
        eprintln!("{text}");
        // SAFETY: async-signal-safe exit; nothing more of the title runs.
        print_last_imports();
        unsafe { libc::_exit(0xC000_0409_u32 as i32) };
    }

    let mut record = ExceptionRecord::new(0, ctx.rip);
    record.code = match signal {
        libc::SIGSEGV | libc::SIGBUS => {
            // Bit 1 of the page-fault error code is set for a write; an
            // execute fault faults at the instruction pointer itself.
            let address = info.si_addr as u64;
            let kind = if address == ctx.rip {
                8
            } else {
                (mc.__es.__err >> 1) as u64 & 1
            };
            record.parameters = 2;
            record.info[..2].copy_from_slice(&[kind, address]);
            STATUS_ACCESS_VIOLATION
        }
        libc::SIGILL => STATUS_ILLEGAL_INSTRUCTION,
        libc::SIGFPE => match info.si_code {
            FPE_INTDIV => STATUS_INTEGER_DIVIDE_BY_ZERO,
            FPE_INTOVF => STATUS_INTEGER_OVERFLOW,
            _ => STATUS_FLOAT_INVALID_OPERATION,
        },
        _ => {
            // SAFETY: the byte before rip is in the title's code.
            if unsafe { ((ctx.rip - 1) as *const u8).read() } == 0xCC {
                // Windows reports a breakpoint at the int3 itself.
                ctx.rip -= 1;
                record.address = ctx.rip;
                STATUS_BREAKPOINT
            } else {
                STATUS_SINGLE_STEP
            }
        }
    };

    // Title code keeps no red zone, but leave room anyway.
    let sp =
        (ctx.rsp() - 0x100 - (size_of::<Context>() + size_of::<ExceptionRecord>()) as u64) & !0xF;
    let ctx_at = sp as *mut Context;
    let record_at = (sp + size_of::<Context>() as u64) as *mut ExceptionRecord;
    // SAFETY: below the faulting thread's stack pointer, so no live frame;
    // 16-aligned as `Context` needs.
    unsafe {
        ctx_at.write(ctx);
        record_at.write(record);
        ((sp - 8) as *mut u64).write(0); // a return address nobody uses
    }
    ss.__rsp = sp - 8;
    ss.__rdi = record_at as u64;
    ss.__rsi = ctx_at as u64;
    ss.__rip = dispatch_from_signal as *const () as u64;
}

extern "C" fn dispatch_from_signal(record: *mut ExceptionRecord, ctx: *mut Context) -> ! {
    // SAFETY: `on_signal` wrote both on this stack, above this frame.
    dispatch(unsafe { &mut *record }, unsafe { &mut *ctx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exception_imports_are_ours_under_any_of_their_dlls() {
        assert!(export("NTDLL.dll", "RtlUnwindEx").is_some());
        assert!(export("api-ms-win-core-rtlsupport-l1-1-0.dll", "RtlCaptureContext").is_some());
        assert!(export("kernel32.dll", "RaiseException").is_some());
        assert!(export("user32.dll", "RaiseException").is_none());
        assert!(export("ntdll.dll", "NtClose").is_none());
    }

    #[test]
    fn vectored_handlers_go_first_or_last_and_can_be_removed() {
        extern "win64" fn a(_: *mut ExceptionPointers) -> i32 {
            0
        }
        extern "win64" fn b(_: *mut ExceptionPointers) -> i32 {
            0
        }
        let first = add_vectored_exception_handler(0, a);
        let second = add_vectored_exception_handler(1, b);
        let order: Vec<usize> = VECTORED.lock().unwrap().iter().map(|&(h, _)| h).collect();
        let (i, j) = (
            order.iter().position(|&h| h == first),
            order.iter().position(|&h| h == second),
        );
        assert!(j < i, "added first: {order:?}");
        assert_eq!(remove_vectored_exception_handler(first), 1);
        assert_eq!(remove_vectored_exception_handler(first), 0);
        assert_eq!(remove_vectored_exception_handler(second), 1);
    }
}
