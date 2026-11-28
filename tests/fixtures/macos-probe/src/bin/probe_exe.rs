//! A stand-in title executable: its entry point returns 40 plus its static
//! TLS variable, read as an exe's code reads it, from slot 0 of the TLS
//! array without looking at `_tls_index`.
#![no_std]
#![no_main]

#[unsafe(link_section = ".tls")]
#[used]
static mut TLS_START: u8 = 0;
#[unsafe(link_section = ".tls$B")]
#[used]
static mut TLS_TWO: u32 = 2;
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

#[unsafe(no_mangle)]
pub extern "system" fn probe_exe_main(_peb: *mut u8) -> u32 {
    unsafe {
        let array: *const *const u8;
        core::arch::asm!("mov {}, gs:[0x58]", out(reg) array, options(nostack, readonly));
        let offset = (&raw const TLS_TWO as usize) - (&raw const TLS_START as usize);
        40 + *((*array).add(offset) as *const u32)
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
