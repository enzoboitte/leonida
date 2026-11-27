//! A stand-in for a title, written by us, for the macOS loader test. Each
//! export checks one thing the loader has to get right.
#![no_std]

use core::hint::black_box;

#[link(name = "xgameruntime", kind = "raw-dylib")]
unsafe extern "system" {
    fn XGameRuntimeInitialize() -> i32;
}

#[link(name = "leonida-probe-missing", kind = "raw-dylib")]
unsafe extern "system" {
    fn LeonidaProbeNotImplemented(x: u64) -> u64;
}

/// Console-only API, answered by the test's resolver.
#[unsafe(no_mangle)]
pub extern "system" fn probe_xgameruntime_init() -> i32 {
    unsafe { XGameRuntimeInitialize() }
}

/// Nobody implements it: reaches an unimplemented-import stub.
#[unsafe(no_mangle)]
pub extern "system" fn probe_missing() -> u64 {
    unsafe { LeonidaProbeNotImplemented(7) }
}

/// Pointers inside the image: right only once the loader relocated them.
static WORDS: [&str; 3] = ["one", "three", "seven"];

#[unsafe(no_mangle)]
pub extern "system" fn probe_relocated() -> usize {
    black_box(&WORDS).iter().map(|w| w.len()).sum()
}

/// The TEB as Win64 code finds it: `gs:[0x30]` is its own address.
#[unsafe(no_mangle)]
pub extern "system" fn probe_teb() -> u64 {
    let teb: u64;
    unsafe { core::arch::asm!("mov {}, gs:[0x30]", out(reg) teb, options(nostack, readonly)) };
    teb
}

// Static TLS, laid out the way the MSVC CRT does it: the linker sorts
// `.tls$*` by name, and `_tls_used` is the directory it points the image at.
#[unsafe(link_section = ".tls")]
#[used]
static mut TLS_START: u8 = 0;
#[unsafe(link_section = ".tls$B")]
#[used]
static mut TLS_COUNTER: u32 = 5;
#[unsafe(link_section = ".tls$ZZZ")]
#[used]
static mut TLS_END: u8 = 0;

#[unsafe(no_mangle)]
static mut _tls_index: u32 = 0x55;

#[repr(C)]
pub struct TlsDirectory {
    start: *const u8,
    end: *const u8,
    index: *const u32,
    callbacks: *const usize,
    zero_fill: u32,
    characteristics: u32,
}
// SAFETY: read-only after linking; only the loader reads it.
unsafe impl Sync for TlsDirectory {}

static NO_CALLBACKS: [usize; 1] = [0];

#[unsafe(no_mangle)]
#[used]
pub static _tls_used: TlsDirectory = TlsDirectory {
    start: &raw const TLS_START,
    end: &raw const TLS_END,
    index: &raw const _tls_index,
    callbacks: NO_CALLBACKS.as_ptr(),
    zero_fill: 0,
    characteristics: 0,
};

/// This thread's copy of `TLS_COUNTER`, plus one, reached as compiled MSVC
/// code reaches it: `gs:[0x58]`, the slot `_tls_index` names, then the
/// variable's offset in the block.
#[unsafe(no_mangle)]
pub extern "system" fn probe_tls_bump() -> u32 {
    unsafe {
        let array: *const *mut u8;
        core::arch::asm!("mov {}, gs:[0x58]", out(reg) array, options(nostack, readonly));
        let index = core::ptr::read_volatile(&raw const _tls_index) as usize;
        let offset = (&raw const TLS_COUNTER as usize) - (&raw const TLS_START as usize);
        let counter = (*array.add(index)).add(offset) as *mut u32;
        *counter += 1;
        *counter
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

// Structured exceptions. `probe_guarded` calls `probe_inner`, which faults
// on `[rcx]` (or raises an exception when rcx is 0). The outer frame's
// language handler catches it as `__except` would: it unwinds to
// `probe_landing`, after the inner frame's unwind handler has run. rbx is
// 0x1111 in the outer frame and 0x2222 in the inner one, so the landing's
// `rbx + 2` is right only if unwinding restored it.

#[repr(C)]
pub struct ExceptionRecord {
    code: u32,
    flags: u32,
    record: u64,
    address: u64,
    parameters: u32,
    info: [u64; 15],
}

const EXCEPTION_UNWIND: u32 = 0x66;

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn RaiseException(code: u32, flags: u32, count: u32, arguments: *const usize);
}

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn RtlUnwindEx(
        frame: u64,
        target: u64,
        record: *mut ExceptionRecord,
        value: u64,
        context: *mut u8,
        history: *mut u8,
    );
}

static mut LAST_CODE: u32 = 0;
static mut UNWOUND: u32 = 0;

unsafe extern "C" {
    static probe_landing: u8;
}

unsafe extern "win64" {
    fn probe_guarded(address: u64) -> u64;
}

extern "system" fn probe_except_handler(
    record: *mut ExceptionRecord,
    frame: u64,
    context: *mut u8,
    _dispatch: *mut u8,
) -> u32 {
    unsafe {
        if (*record).flags & EXCEPTION_UNWIND == 0 {
            LAST_CODE = (*record).code;
            let landing = &raw const probe_landing as u64;
            RtlUnwindEx(frame, landing, record, 0, context, core::ptr::null_mut());
        }
    }
    1 // ExceptionContinueSearch
}

extern "system" fn probe_unwind_handler(
    record: *mut ExceptionRecord,
    _frame: u64,
    _context: *mut u8,
    _dispatch: *mut u8,
) -> u32 {
    unsafe {
        if (*record).flags & EXCEPTION_UNWIND != 0 {
            UNWOUND += 1;
        }
    }
    1
}

extern "system" fn probe_raise_helper() {
    unsafe { RaiseException(0xE000_1234, 0, 0, core::ptr::null()) };
}

core::arch::global_asm!(
    ".def probe_guarded; .scl 2; .type 32; .endef",
    ".seh_proc probe_guarded",
    "probe_guarded:",
    "push rbx",
    ".seh_pushreg rbx",
    "sub rsp, 0x20",
    ".seh_stackalloc 0x20",
    ".seh_endprologue",
    ".seh_handler {except}, @except",
    "mov rbx, 0x1111",
    "call .Lprobe_inner",
    "mov rax, 1",
    "jmp .Lprobe_done",
    ".globl probe_landing",
    "probe_landing:",
    "lea rax, [rbx + 2]",
    ".Lprobe_done:",
    "add rsp, 0x20",
    "pop rbx",
    "ret",
    ".seh_endproc",
    "",
    ".def .Lprobe_inner; .scl 3; .type 32; .endef",
    ".seh_proc .Lprobe_inner",
    ".Lprobe_inner:",
    "push rbx",
    ".seh_pushreg rbx",
    "sub rsp, 0x20",
    ".seh_stackalloc 0x20",
    ".seh_endprologue",
    ".seh_handler {unwind}, @unwind",
    "mov rbx, 0x2222",
    "test rcx, rcx",
    "jz .Lprobe_raise",
    "mov rax, [rcx]",
    "jmp .Lprobe_inner_done",
    ".Lprobe_raise:",
    "call {raise}",
    // As MSVC does: a return address on an epilog would read as "in the
    // epilog", and the frame's handler would be skipped.
    "nop",
    ".Lprobe_inner_done:",
    "add rsp, 0x20",
    "pop rbx",
    "ret",
    ".seh_endproc",
    "",
    // A leaf that reads [rcx]: 48 8B 01, three bytes, for a vectored handler to step over.
    ".def probe_read; .scl 2; .type 32; .endef",
    "probe_read:",
    "mov rax, [rcx]",
    "ret",
    except = sym probe_except_handler,
    unwind = sym probe_unwind_handler,
    raise = sym probe_raise_helper,
);

unsafe extern "win64" {
    fn probe_read(address: u64) -> u64;
}

/// 0x1113 when the fault (or the exception, for 0) was caught and unwound.
#[unsafe(no_mangle)]
pub extern "system" fn probe_seh(address: u64) -> u64 {
    unsafe { probe_guarded(address) }
}

/// The last exception code `probe_seh` caught, and how many inner frames
/// were unwound.
#[unsafe(no_mangle)]
pub extern "system" fn probe_seh_seen() -> u64 {
    unsafe { ((UNWOUND as u64) << 32) | LAST_CODE as u64 }
}

#[unsafe(no_mangle)]
pub extern "system" fn probe_fault_read(address: u64) -> u64 {
    unsafe { probe_read(address) }
}
