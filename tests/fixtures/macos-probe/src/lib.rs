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
