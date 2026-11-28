//! Clocks, the system's description, the command line and environment,
//! code pages, and interlocked singly linked lists.

use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::memory::{GRANULARITY, PAGE};
use super::set_last_error;

/// The performance counter ticks at 10 MHz, as on most Windows PCs.
const COUNTER_HZ: u64 = 10_000_000;
/// 100 ns intervals from 1601 (FILETIME) to 1970.
const FILETIME_TO_UNIX: u64 = 116_444_736_000_000_000;
const CP_ACP: u32 = 0;
const CP_OEMCP: u32 = 1;
const CP_UTF8: u32 = 65001;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_ENVVAR_NOT_FOUND: u32 = 203;

fn started() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

#[allow(non_snake_case)]
pub extern "win64" fn QueryPerformanceCounter(out: *mut i64) -> i32 {
    let ticks = started().elapsed().as_nanos() / (1_000_000_000 / COUNTER_HZ as u128);
    // SAFETY: the title's out pointer.
    unsafe { *out = ticks as i64 };
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn QueryPerformanceFrequency(out: *mut i64) -> i32 {
    // SAFETY: the title's out pointer.
    unsafe { *out = COUNTER_HZ as i64 };
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn GetTickCount64() -> u64 {
    started().elapsed().as_millis() as u64
}

#[allow(non_snake_case)]
pub extern "win64" fn GetTickCount() -> u32 {
    GetTickCount64() as u32
}

fn filetime_now() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    FILETIME_TO_UNIX + (since.as_nanos() / 100) as u64
}

#[allow(non_snake_case)]
pub extern "win64" fn GetSystemTimeAsFileTime(out: *mut u64) {
    // SAFETY: the title's FILETIME (two u32, as one u64 little-endian).
    unsafe { out.write_unaligned(filetime_now()) };
}

#[allow(non_snake_case)]
pub extern "win64" fn GetSystemTimePreciseAsFileTime(out: *mut u64) {
    GetSystemTimeAsFileTime(out);
}

/// `SYSTEM_INFO` (x64).
#[repr(C)]
pub struct SystemInfo {
    architecture: u16,
    reserved: u16,
    page_size: u32,
    minimum_address: usize,
    maximum_address: usize,
    processor_mask: usize,
    processors: u32,
    processor_type: u32,
    granularity: u32,
    level: u16,
    revision: u16,
}

#[allow(non_snake_case)]
pub extern "win64" fn GetSystemInfo(out: *mut SystemInfo) {
    let processors = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(64);
    let info = SystemInfo {
        architecture: 9, // PROCESSOR_ARCHITECTURE_AMD64
        reserved: 0,
        page_size: PAGE as u32,
        minimum_address: GRANULARITY,
        maximum_address: 0x7FFF_FFFE_FFFF,
        processor_mask: if processors == 64 {
            usize::MAX
        } else {
            (1 << processors) - 1
        },
        processors: processors as u32,
        processor_type: 8664,
        granularity: GRANULARITY as u32,
        level: 0x17, // family 17h: Zen 2, as the console's
        revision: 0x7100,
    };
    // SAFETY: the title's SYSTEM_INFO.
    unsafe { out.write(info) };
}

#[allow(non_snake_case)]
pub extern "win64" fn GetNativeSystemInfo(out: *mut SystemInfo) {
    GetSystemInfo(out);
}

#[allow(non_snake_case)]
pub extern "win64" fn IsProcessorFeaturePresent(feature: u32) -> i32 {
    use std::arch::is_x86_feature_detected as has;
    let present = match feature {
        // PF_COMPARE_EXCHANGE_DOUBLE, PF_MMX, PF_XMMI, PF_RDTSC, PF_XMMI64,
        // PF_NX, PF_COMPARE_EXCHANGE128, PF_FASTFAIL: every x86-64 has them.
        2 | 3 | 6 | 8 | 10 | 12 | 14 | 23 => true,
        13 => has!("sse3"),
        36 => has!("ssse3"),
        37 => has!("sse4.1"),
        38 => has!("sse4.2"),
        39 => has!("avx"),
        40 => has!("avx2"),
        41 => has!("avx512f"),
        _ => false,
    };
    present.into()
}

#[allow(non_snake_case)]
pub extern "win64" fn IsDebuggerPresent() -> i32 {
    0
}

/// The command line the title sees: set by the runner, else ours.
static COMMAND_LINE: OnceLock<(Vec<u16>, Vec<u8>)> = OnceLock::new();

fn command_line() -> &'static (Vec<u16>, Vec<u8>) {
    COMMAND_LINE.get_or_init(|| {
        let line = std::env::args()
            .map(|a| {
                if a.contains(' ') {
                    format!("\"{a}\"")
                } else {
                    a
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let wide = line.encode_utf16().chain([0]).collect();
        let narrow = line.bytes().chain([0]).collect();
        (wide, narrow)
    })
}

/// Sets the title's command line before it starts (its executable and
/// arguments, as Windows writes them). Later calls change nothing.
pub fn set_command_line(line: &str) {
    let _ = COMMAND_LINE.set((
        line.encode_utf16().chain([0]).collect(),
        line.bytes().chain([0]).collect(),
    ));
}

#[allow(non_snake_case)]
pub extern "win64" fn GetCommandLineW() -> *const u16 {
    command_line().0.as_ptr()
}

#[allow(non_snake_case)]
pub extern "win64" fn GetCommandLineA() -> *const u8 {
    command_line().1.as_ptr()
}

/// # Safety
/// `s` is NUL-terminated UTF-16.
unsafe fn wide(s: *const u16) -> String {
    let mut n = 0;
    // SAFETY: up to its terminator.
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` units before the terminator.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(s, n) })
}

/// # Safety
/// `s` is NUL-terminated.
unsafe fn narrow(s: *const u8) -> String {
    // SAFETY: as above.
    unsafe { std::ffi::CStr::from_ptr(s.cast()) }
        .to_string_lossy()
        .into_owned()
}

/// Copies `value` and its terminator into `out` (`size` units) as Windows'
/// string getters do: its length without the terminator when it fits, the
/// size it needs with the terminator when it does not.
fn give<T: Copy + Default>(value: &[T], out: *mut T, size: u32) -> u32 {
    if (value.len() as u32) < size && !out.is_null() {
        // SAFETY: the title's buffer of `size` units.
        unsafe {
            std::ptr::copy_nonoverlapping(value.as_ptr(), out, value.len());
            *out.add(value.len()) = T::default();
        }
        value.len() as u32
    } else {
        value.len() as u32 + 1
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn GetEnvironmentVariableW(name: *const u16, out: *mut u16, size: u32) -> u32 {
    // SAFETY: a NUL-terminated name.
    match std::env::var(unsafe { wide(name) }) {
        Ok(v) => give(&v.encode_utf16().collect::<Vec<_>>(), out, size),
        Err(_) => {
            set_last_error(ERROR_ENVVAR_NOT_FOUND);
            0
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn GetEnvironmentVariableA(name: *const u8, out: *mut u8, size: u32) -> u32 {
    // SAFETY: a NUL-terminated name.
    match std::env::var(unsafe { narrow(name) }) {
        Ok(v) => give(v.as_bytes(), out, size),
        Err(_) => {
            set_last_error(ERROR_ENVVAR_NOT_FOUND);
            0
        }
    }
}

/// shortcut: changes the host process's environment, which the title
/// shares; titles set a few variables of their own at most.
#[allow(non_snake_case)]
pub extern "win64" fn SetEnvironmentVariableW(name: *const u16, value: *const u16) -> i32 {
    static ENV: Mutex<()> = Mutex::new(());
    let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: NUL-terminated strings; the lock above keeps our own writers
    // apart (the variable functions of the host are not thread-safe).
    unsafe {
        let name = wide(name);
        if value.is_null() {
            std::env::remove_var(name);
        } else {
            std::env::set_var(name, wide(value));
        }
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn GetEnvironmentStringsW() -> *mut u16 {
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in std::env::vars() {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    Box::leak(block.into_boxed_slice()).as_mut_ptr()
}

/// shortcut: the blocks are leaked; titles read the environment once.
#[allow(non_snake_case)]
pub extern "win64" fn FreeEnvironmentStringsW(_: *mut u16) -> i32 {
    1
}

/// `STARTUPINFOW`: its size, and no settings.
#[allow(non_snake_case)]
pub extern "win64" fn GetStartupInfoW(out: *mut u8) {
    // SAFETY: the title's STARTUPINFOW, 104 bytes on x64.
    unsafe {
        out.write_bytes(0, 104);
        out.cast::<u32>().write(104);
    }
}

// Code pages. shortcut: the ANSI code page is UTF-8 (as Windows can be set
// up), and other single-byte code pages read as Latin-1.

#[allow(non_snake_case)]
pub extern "win64" fn GetACP() -> u32 {
    CP_UTF8
}

#[allow(non_snake_case)]
pub extern "win64" fn GetOEMCP() -> u32 {
    437
}

#[allow(non_snake_case)]
pub extern "win64" fn IsValidCodePage(page: u32) -> i32 {
    matches!(page, CP_UTF8 | 437 | 850 | 1252 | 28591 | 20127).into()
}

fn utf8(page: u32) -> bool {
    matches!(page, CP_ACP | CP_OEMCP | CP_UTF8 | 3) // 3: CP_THREAD_ACP
}

/// `CPINFO`: the longest character, the default one, the lead bytes.
#[allow(non_snake_case)]
pub extern "win64" fn GetCPInfo(page: u32, out: *mut u8) -> i32 {
    // SAFETY: the title's CPINFO, 20 bytes.
    unsafe {
        out.write_bytes(0, 20);
        out.cast::<u32>().write(if utf8(page) { 4 } else { 1 });
        *out.add(4) = b'?';
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn MultiByteToWideChar(
    page: u32,
    _flags: u32,
    source: *const u8,
    length: i32,
    out: *mut u16,
    size: i32,
) -> i32 {
    // -1: up to and with the terminator.
    let bytes = if length < 0 {
        // SAFETY: NUL-terminated, as -1 says.
        unsafe { std::ffi::CStr::from_ptr(source.cast()) }.to_bytes_with_nul()
    } else {
        // SAFETY: the title passes `length` bytes.
        unsafe { std::slice::from_raw_parts(source, length as usize) }
    };
    let units: Vec<u16> = if utf8(page) {
        String::from_utf8_lossy(bytes).encode_utf16().collect()
    } else {
        bytes.iter().map(|&b| b as u16).collect()
    };
    convert(&units, out, size)
}

#[allow(non_snake_case)]
pub extern "win64" fn WideCharToMultiByte(
    page: u32,
    _flags: u32,
    source: *const u16,
    length: i32,
    out: *mut u8,
    size: i32,
    _default: *const u8,
    used_default: *mut i32,
) -> i32 {
    let units = if length < 0 {
        let mut n = 0;
        // SAFETY: NUL-terminated, as -1 says.
        while unsafe { *source.add(n) } != 0 {
            n += 1;
        }
        // SAFETY: up to and with the terminator.
        unsafe { std::slice::from_raw_parts(source, n + 1) }
    } else {
        // SAFETY: the title passes `length` units.
        unsafe { std::slice::from_raw_parts(source, length as usize) }
    };
    let mut lost = false;
    let bytes: Vec<u8> = if utf8(page) {
        String::from_utf16_lossy(units).into_bytes()
    } else {
        units
            .iter()
            .map(|&u| {
                lost |= u > 0xFF;
                if u > 0xFF { b'?' } else { u as u8 }
            })
            .collect()
    };
    if !used_default.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *used_default = lost.into() };
    }
    convert(&bytes, out, size)
}

/// Writes `converted` to `out` (`size` units), or says how many it needs
/// when `size` is 0: Windows' conversion results.
fn convert<T: Copy>(converted: &[T], out: *mut T, size: i32) -> i32 {
    if size == 0 {
        return converted.len() as i32;
    }
    if converted.len() > size as usize {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    // SAFETY: the title's buffer of `size` units.
    unsafe { std::ptr::copy_nonoverlapping(converted.as_ptr(), out, converted.len()) };
    converted.len() as i32
}

// Interlocked singly linked lists, laid out as x64's SLIST_HEADER: depth and
// sequence in the first word, the first entry in the second.
// shortcut: one lock for every list instead of a 16-byte compare-exchange.

static SLISTS: Mutex<()> = Mutex::new(());

/// The header word after a push (+1) or pop (-1): depth in the low 16 bits,
/// a sequence above them that every change advances.
fn step(word: u64, depth: i16) -> u64 {
    (((word >> 16) + 1) << 16) | u64::from((word as u16).wrapping_add_signed(depth))
}

#[repr(C, align(16))]
pub struct SListHeader {
    depth_sequence: u64,
    first: usize,
}

#[allow(non_snake_case)]
pub extern "win64" fn InitializeSListHead(head: *mut SListHeader) {
    // SAFETY: the title's 16-byte aligned header.
    unsafe {
        head.write(SListHeader {
            depth_sequence: 0,
            first: 0,
        })
    };
}

#[allow(non_snake_case)]
pub extern "win64" fn InterlockedPushEntrySList(
    head: *mut SListHeader,
    entry: *mut usize,
) -> *mut usize {
    let _guard = SLISTS.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: the title's header, and an entry whose first word is its link.
    unsafe {
        let h = &mut *head;
        let previous = h.first as *mut usize;
        *entry = h.first;
        h.first = entry as usize;
        h.depth_sequence = step(h.depth_sequence, 1);
        previous
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn InterlockedPopEntrySList(head: *mut SListHeader) -> *mut usize {
    let _guard = SLISTS.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: as above.
    unsafe {
        let h = &mut *head;
        let entry = h.first as *mut usize;
        if !entry.is_null() {
            h.first = *entry;
            h.depth_sequence = step(h.depth_sequence, -1);
        }
        entry
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn InterlockedFlushSList(head: *mut SListHeader) -> *mut usize {
    let _guard = SLISTS.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: as above.
    unsafe {
        let h = &mut *head;
        let entries = h.first as *mut usize;
        h.first = 0;
        h.depth_sequence &= !0xFFFF;
        entries
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn QueryDepthSList(head: *const SListHeader) -> u16 {
    // SAFETY: the title's header.
    unsafe { (*head).depth_sequence as u16 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_advance() {
        let (mut a, mut b, mut f) = (0, 0, 0);
        QueryPerformanceFrequency(&mut f);
        QueryPerformanceCounter(&mut a);
        std::thread::sleep(std::time::Duration::from_millis(5));
        QueryPerformanceCounter(&mut b);
        assert!(b - a >= f / 1000 * 5, "{a} {b}");
        let mut ft = 0u64;
        GetSystemTimeAsFileTime(&mut ft);
        assert!(ft > FILETIME_TO_UNIX + 17_000_000_000_000_000, "after 2023");
    }

    #[test]
    fn conversions_follow_windows_conventions() {
        let text = "Léo\0";
        let need = MultiByteToWideChar(CP_UTF8, 0, text.as_ptr(), -1, std::ptr::null_mut(), 0);
        assert_eq!(need, 4, "with the terminator");
        let mut wide = [0u16; 4];
        assert_eq!(
            MultiByteToWideChar(CP_UTF8, 0, text.as_ptr(), -1, wide.as_mut_ptr(), 4),
            4
        );
        assert_eq!(String::from_utf16_lossy(&wide), "Léo\0");
        let mut small = [0u16; 2];
        assert_eq!(
            MultiByteToWideChar(CP_UTF8, 0, text.as_ptr(), -1, small.as_mut_ptr(), 2),
            0
        );

        let mut back = [0u8; 8];
        let n = WideCharToMultiByte(
            CP_UTF8,
            0,
            wide.as_ptr(),
            -1,
            back.as_mut_ptr(),
            8,
            std::ptr::null(),
            std::ptr::null_mut(),
        );
        assert_eq!(&back[..n as usize], "Léo\0".as_bytes());
        let mut lost = 0;
        let euro = [0x20ACu16];
        WideCharToMultiByte(
            1252,
            0,
            euro.as_ptr(),
            1,
            back.as_mut_ptr(),
            8,
            std::ptr::null(),
            &mut lost,
        );
        assert_eq!((back[0], lost), (b'?', 1));
    }

    #[test]
    fn environment_and_command_line() {
        let name: Vec<u16> = "PATH\0".encode_utf16().collect();
        let need = GetEnvironmentVariableW(name.as_ptr(), std::ptr::null_mut(), 0);
        assert!(need > 1);
        let mut buf = vec![0u16; need as usize];
        assert_eq!(
            GetEnvironmentVariableW(name.as_ptr(), buf.as_mut_ptr(), need),
            need - 1
        );
        let missing: Vec<u16> = "LEONIDA_NO_SUCH_VARIABLE\0".encode_utf16().collect();
        assert_eq!(
            GetEnvironmentVariableW(missing.as_ptr(), buf.as_mut_ptr(), need),
            0
        );
        // SAFETY: a NUL-terminated string of ours.
        assert!(!unsafe { wide(GetCommandLineW()) }.is_empty());
    }

    #[test]
    fn slists_push_pop_and_count() {
        let mut head = SListHeader {
            depth_sequence: 0,
            first: 0,
        };
        InitializeSListHead(&mut head);
        let mut entries = [[0usize; 2]; 3];
        for e in &mut entries {
            InterlockedPushEntrySList(&mut head, e.as_mut_ptr());
        }
        assert_eq!(QueryDepthSList(&head), 3);
        assert_eq!(InterlockedPopEntrySList(&mut head), entries[2].as_mut_ptr());
        assert_eq!(QueryDepthSList(&head), 2);
        let rest = InterlockedFlushSList(&mut head);
        assert_eq!(rest, entries[1].as_mut_ptr());
        assert_eq!(QueryDepthSList(&head), 0);
        assert!(InterlockedPopEntrySList(&mut head).is_null());
    }

    #[test]
    fn system_info_says_x64_and_64k_granularity() {
        let mut info = std::mem::MaybeUninit::<SystemInfo>::uninit();
        GetSystemInfo(info.as_mut_ptr());
        // SAFETY: filled in above.
        let info = unsafe { info.assume_init() };
        assert_eq!(
            (info.architecture, info.page_size, info.granularity),
            (9, 4096, 65536)
        );
        assert!(info.processors >= 1);
        assert_eq!(IsProcessorFeaturePresent(10), 1);
        let _ = std::ptr::null::<c_void>();
    }
}
