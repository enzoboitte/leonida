//! What the game's executable imports beyond the rest of this module:
//! dates and times, memory and processor counts, locale names, random
//! bytes, the console window (Win32 calls the title makes for it), and the
//! entry points of areas not ported yet (media, DirectStorage, the audio
//! processor), which fail as a console without them would.

use std::ffi::c_void;
use std::time::{SystemTime, UNIX_EPOCH};

use super::set_last_error;
use crate::{HleExport, HleModule, hle_export};

const S_OK: i32 = 0;
const E_NOTIMPL: i32 = 0x8000_4001_u32 as i32;
const FILETIME_TO_UNIX: u64 = 116_444_736_000_000_000;

/// `SYSTEMTIME`: year, month, day of week, day, hour, minute, second, ms.
type SystemTimeParts = [u16; 8];

fn parts(filetime: u64) -> SystemTimeParts {
    let ms_total = filetime.saturating_sub(FILETIME_TO_UNIX) / 10_000;
    let (days, ms) = ((ms_total / 86_400_000) as i64, ms_total % 86_400_000);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let weekday = (days + 4).rem_euclid(7); // 1970-01-01 was a Thursday
    [
        year as u16,
        month as u16,
        weekday as u16,
        day as u16,
        (ms / 3_600_000) as u16,
        (ms / 60_000 % 60) as u16,
        (ms / 1000 % 60) as u16,
        (ms % 1000) as u16,
    ]
}

fn filetime_of(p: &SystemTimeParts) -> Option<u64> {
    let [year, month, _, day, hour, minute, second, ms] = p.map(i64::from);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1601 {
        return None;
    }
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let ms_total = days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1000 + ms;
    Some((FILETIME_TO_UNIX as i64 + ms_total * 10_000) as u64)
}

fn now() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    FILETIME_TO_UNIX + (since.as_nanos() / 100) as u64
}

#[allow(non_snake_case)]
pub extern "win64" fn GetSystemTime(out: *mut SystemTimeParts) {
    // SAFETY: the title's SYSTEMTIME.
    unsafe { out.write(parts(now())) };
}

/// shortcut: local time is UTC (the console's time zone is not asked).
#[allow(non_snake_case)]
pub extern "win64" fn GetLocalTime(out: *mut SystemTimeParts) {
    GetSystemTime(out);
}

#[allow(non_snake_case)]
pub extern "win64" fn FileTimeToSystemTime(ft: *const u64, out: *mut SystemTimeParts) -> i32 {
    // SAFETY: the title's FILETIME and SYSTEMTIME.
    unsafe { out.write(parts(ft.read_unaligned())) };
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn SystemTimeToFileTime(st: *const SystemTimeParts, out: *mut u64) -> i32 {
    // SAFETY: the title's SYSTEMTIME.
    match filetime_of(unsafe { &*st }) {
        Some(ft) => {
            // SAFETY: the title's FILETIME.
            unsafe { out.write_unaligned(ft) };
            1
        }
        None => {
            set_last_error(87);
            0
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn FileTimeToLocalFileTime(ft: *const u64, out: *mut u64) -> i32 {
    // SAFETY: the title's FILETIMEs; local is UTC here.
    unsafe { out.write_unaligned(ft.read_unaligned()) };
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn LocalFileTimeToFileTime(ft: *const u64, out: *mut u64) -> i32 {
    FileTimeToLocalFileTime(ft, out)
}

#[allow(non_snake_case)]
pub extern "win64" fn GetFileTime(
    handle: usize,
    created: *mut u64,
    accessed: *mut u64,
    written: *mut u64,
) -> i32 {
    let mut info = std::mem::MaybeUninit::<[u32; 13]>::uninit();
    if super::files::GetFileInformationByHandle(handle, info.as_mut_ptr().cast()) == 0 {
        return 0;
    }
    // SAFETY: filled in above: attributes, then three FILETIMEs.
    let w = unsafe { info.assume_init() };
    for (out, at) in [(created, 1), (accessed, 3), (written, 5)] {
        if !out.is_null() {
            // SAFETY: the title's FILETIME.
            unsafe { out.write_unaligned(u64::from(w[at]) | u64::from(w[at + 1]) << 32) };
        }
    }
    1
}

/// Writes `text` as a Windows string getter does (`size` 0: the size needed).
fn give(text: &str, out: *mut u16, size: i32) -> i32 {
    let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
    if size == 0 {
        return units.len() as i32;
    }
    if (size as usize) < units.len() || out.is_null() {
        set_last_error(122);
        return 0;
    }
    // SAFETY: the title's buffer of `size` units.
    unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), out, units.len()) };
    units.len() as i32
}

fn date_of(st: *const SystemTimeParts) -> SystemTimeParts {
    if st.is_null() {
        parts(now())
    } else {
        // SAFETY: the title's SYSTEMTIME.
        unsafe { *st }
    }
}

/// shortcut: the short en-US forms, whatever format the title asks for.
#[allow(non_snake_case)]
pub extern "win64" fn GetDateFormatEx(
    _: *const u16,
    _: u32,
    st: *const SystemTimeParts,
    _: *const u16,
    out: *mut u16,
    size: i32,
    _: *const u16,
) -> i32 {
    let [y, m, _, d, ..] = date_of(st);
    give(&format!("{m}/{d}/{y}"), out, size)
}

#[allow(non_snake_case)]
pub extern "win64" fn GetTimeFormatEx(
    _: *const u16,
    _: u32,
    st: *const SystemTimeParts,
    _: *const u16,
    out: *mut u16,
    size: i32,
) -> i32 {
    let [.., h, mi, s, _] = date_of(st);
    give(&format!("{h:02}:{mi:02}:{s:02}"), out, size)
}

#[allow(non_snake_case)]
pub extern "win64" fn GetUserDefaultLocaleName(out: *mut u16, size: i32) -> i32 {
    give("en-US", out, size)
}

#[allow(non_snake_case)]
pub extern "win64" fn GetUserDefaultGeoName(out: *mut u16, size: i32) -> i32 {
    give("US", out, size)
}

#[allow(non_snake_case)]
pub extern "win64" fn LocaleNameToLCID(_: *const u16, _: u32) -> u32 {
    0x0409
}

#[allow(non_snake_case)]
pub extern "win64" fn lstrcmpW(a: *const u16, b: *const u16) -> i32 {
    // SAFETY: NUL-terminated strings.
    let len = |s: *const u16| (0..).take_while(|&i| unsafe { *s.add(i) } != 0).count();
    // SAFETY: as counted.
    let (a, b) = unsafe {
        (
            std::slice::from_raw_parts(a, len(a)),
            std::slice::from_raw_parts(b, len(b)),
        )
    };
    a.cmp(b) as i32
}

/// `MEMORYSTATUSEX`: the host's memory as the console's.
#[allow(non_snake_case)]
pub extern "win64" fn GlobalMemoryStatusEx(out: *mut u64) -> i32 {
    let mut total = 0u64;
    let mut len = size_of::<u64>();
    // SAFETY: a NUL-terminated name and a u64 out value.
    unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&raw mut total).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    // SAFETY: the title's MEMORYSTATUSEX: dwLength, dwMemoryLoad, then six u64 and one more.
    unsafe {
        let s = out.cast::<u32>();
        *s.add(1) = 50;
        let v = out.add(1);
        // shortcut: half of it reads as available.
        for (i, value) in [total, total / 2, total, total / 2, 1 << 47, 1 << 47, 0]
            .into_iter()
            .enumerate()
        {
            *v.add(i) = value;
        }
    }
    1
}

/// Cores as `SYSTEM_LOGICAL_PROCESSOR_INFORMATION` (32 bytes each).
#[allow(non_snake_case)]
pub extern "win64" fn GetLogicalProcessorInformation(out: *mut u8, length: *mut u32) -> i32 {
    let n = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(64);
    let need = (n * 32) as u32;
    // SAFETY: the title's in-out length.
    let have = unsafe { std::mem::replace(&mut *length, need) };
    if out.is_null() || have < need {
        set_last_error(122);
        return 0;
    }
    for i in 0..n {
        let mut e = [0u8; 32];
        e[0..8].copy_from_slice(&(1u64 << i).to_le_bytes()); // mask; RelationProcessorCore is 0
        // SAFETY: the title's buffer of `need` bytes.
        unsafe { std::ptr::copy_nonoverlapping(e.as_ptr(), out.add(i * 32), 32) };
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn VirtualQueryEx(
    _: usize,
    address: *const c_void,
    out: *mut c_void,
    length: usize,
) -> usize {
    super::memory::VirtualQuery(address, out.cast(), length)
}

#[allow(non_snake_case)]
pub extern "win64" fn GetProcessId(_: usize) -> u32 {
    std::process::id()
}

/// The calling thread's own id gives its pseudo handle; other threads
/// cannot be opened here.
#[allow(non_snake_case)]
pub extern "win64" fn OpenThread(_: u32, _: i32, id: u32) -> usize {
    if id == super::thread::GetCurrentThreadId() {
        return super::sync::CURRENT_THREAD;
    }
    set_last_error(5);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn GetCurrentThreadStackLimits(low: *mut usize, high: *mut usize) {
    // SAFETY: the calling thread's own pthread.
    let (top, size) = unsafe {
        let me = libc::pthread_self();
        (
            libc::pthread_get_stackaddr_np(me) as usize,
            libc::pthread_get_stacksize_np(me),
        )
    };
    // SAFETY: the title's out pointers.
    unsafe {
        *low = top - size;
        *high = top;
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn SetProcessPriorityBoost(_: usize, _: i32) -> i32 {
    1
}

/// `GROUP_AFFINITY`: every processor, group 0.
#[allow(non_snake_case)]
pub extern "win64" fn GetThreadGroupAffinity(_: usize, out: *mut u64) -> i32 {
    let n = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(64);
    // SAFETY: the title's GROUP_AFFINITY (mask, group and reserved words).
    unsafe {
        *out = if n == 64 { u64::MAX } else { (1 << n) - 1 };
        *out.add(1) = 0;
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn SetThreadGroupAffinity(
    handle: usize,
    _: *const u64,
    previous: *mut u64,
) -> i32 {
    if !previous.is_null() {
        GetThreadGroupAffinity(handle, previous);
    }
    1
}

/// No named pipes on this console.
#[allow(non_snake_case)]
pub extern "win64" fn WaitNamedPipeW(_: *const u16, _: u32) -> i32 {
    set_last_error(2);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn SetNamedPipeHandleState(
    _: usize,
    _: *const u32,
    _: *const u32,
    _: *const u32,
) -> i32 {
    set_last_error(6);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn RegisterAppConstrainedChangeNotification(
    _: usize,
    _: usize,
    registration: *mut usize,
) -> u32 {
    if !registration.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *registration = 1 };
    }
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn UnregisterAppConstrainedChangeNotification(_: usize) {}

/// Error reports go nowhere: no report is ever sent (offline by design).
#[allow(non_snake_case)]
pub extern "win64" fn WerRegisterFile(_: *const u16, _: u32, _: u32) -> i32 {
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn CoInitializeEx(_: *const c_void, _: u32) -> i32 {
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn CoUninitialize() {}

#[allow(non_snake_case)]
pub extern "win64" fn StringFromGUID2(guid: *const [u8; 16], out: *mut u16, size: i32) -> i32 {
    // SAFETY: the title's GUID.
    let g = unsafe { *guid };
    let d1 = u32::from_le_bytes([g[0], g[1], g[2], g[3]]);
    let d2 = u16::from_le_bytes([g[4], g[5]]);
    let d3 = u16::from_le_bytes([g[6], g[7]]);
    let tail: String = g[10..].iter().map(|b| format!("{b:02X}")).collect();
    let text = format!(
        "{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{tail}}}",
        g[8], g[9]
    );
    if size < 39 {
        return 0;
    }
    give(&text, out, size)
}

// bcrypt: random bytes from the host; providers are tokens.

#[allow(non_snake_case)]
pub extern "win64" fn BCryptOpenAlgorithmProvider(
    out: *mut usize,
    _: *const u16,
    _: *const u16,
    _: u32,
) -> i32 {
    // SAFETY: the title's out pointer.
    unsafe { *out = 1 };
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn BCryptCloseAlgorithmProvider(_: usize, _: u32) -> i32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn BCryptGenRandom(_: usize, buffer: *mut u8, length: u32, _: u32) -> i32 {
    // SAFETY: the title's buffer of `length` bytes.
    unsafe { libc::arc4random_buf(buffer.cast(), length as usize) };
    0
}

// The console's core window: the title makes a Win32 window and pumps its
// messages; here our own window (the runner's) is the one shown, and the
// title's stays an id with no messages.
// shortcut: no message reaches the title (no input through it, no close).

const WINDOW: usize = 0x0001_0010;

#[allow(non_snake_case)]
pub extern "win64" fn RegisterClassExA(_: *const c_void) -> u16 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn CreateWindowExA(
    _: u32,
    _: *const u8,
    _: *const u8,
    _: u32,
    _: i32,
    _: i32,
    _: i32,
    _: i32,
    _: usize,
    _: usize,
    _: usize,
    _: *const c_void,
) -> usize {
    WINDOW
}

#[allow(non_snake_case)]
pub extern "win64" fn ShowWindow(_: usize, _: i32) -> i32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn DefWindowProcA(_: usize, _: u32, _: usize, _: isize) -> isize {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn PeekMessageA(_: *mut c_void, _: usize, _: u32, _: u32, _: u32) -> i32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn TranslateMessage(_: *const c_void) -> i32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn DispatchMessageA(_: *const c_void) -> isize {
    0
}

// Areas not ported to macOS yet answer as a console without them.
// shortcut: Media Foundation (movies), DirectStorage and the audio
// processor (xapu) are Windows-only modules so far.

#[allow(non_snake_case)]
pub extern "win64" fn MFStartup(_: u32, _: u32) -> i32 {
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn MFShutdown() -> i32 {
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn MFGetSystemTime() -> i64 {
    super::system::GetTickCount64() as i64 * 10_000
}

/// Writes nothing: these functions take their out pointer in different places.
#[allow(non_snake_case)]
pub extern "win64" fn not_ported() -> i32 {
    E_NOTIMPL
}

pub fn modules() -> Vec<HleModule> {
    let not_ported = not_ported as *const () as usize;
    let stub = |name| HleExport {
        name,
        address: not_ported,
    };
    vec![
        HleModule {
            name: "media",
            dlls: &["mfplat.dll", "mfreadwrite.dll"],
            exports: vec![
                hle_export!(MFStartup),
                hle_export!(MFShutdown),
                hle_export!(MFGetSystemTime),
                stub("MFCreateAttributes"),
                stub("MFCreateMediaType"),
                stub("MFCreateSourceResolver"),
                stub("MFCreateSourceReaderFromMediaSource"),
            ],
        },
        HleModule {
            name: "audio processor",
            dlls: &["xapu.dll"],
            exports: [
                "XApuConnect",
                "XApuDisconnect",
                "XApuEnqueueCommand",
                "XApuDequeueResult",
                "XApuGetResultCount",
            ]
            .map(stub)
            .to_vec(),
        },
        HleModule {
            name: "window and crypto",
            dlls: &["ext-ms-win-rtcore-ntuser-*", "user32.dll", "bcrypt.dll"],
            exports: vec![
                hle_export!(RegisterClassExA),
                hle_export!(CreateWindowExA),
                hle_export!(ShowWindow),
                hle_export!(DefWindowProcA),
                hle_export!(PeekMessageA),
                hle_export!(TranslateMessage),
                hle_export!(DispatchMessageA),
                hle_export!(BCryptOpenAlgorithmProvider),
                hle_export!(BCryptCloseAlgorithmProvider),
                hle_export!(BCryptGenRandom),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip_through_filetimes() {
        // 2026-11-28 15:04:05.678 UTC, a Saturday.
        let st: SystemTimeParts = [2026, 11, 6, 28, 15, 4, 5, 678];
        let mut ft = 0u64;
        assert_eq!(SystemTimeToFileTime(&st, &mut ft), 1);
        let mut back = [0u16; 8];
        FileTimeToSystemTime(&ft, &mut back);
        assert_eq!(back, st);
        let mut epoch = [0u16; 8];
        FileTimeToSystemTime(&FILETIME_TO_UNIX, &mut epoch);
        assert_eq!(epoch[..4], [1970, 1, 4, 1], "a Thursday");
    }

    #[test]
    fn guids_print_as_windows_writes_them() {
        let g = [
            0x78, 0x56, 0x34, 0x12, 0xBC, 0x9A, 0xF0, 0xDE, 1, 2, 3, 4, 5, 6, 7, 8,
        ];
        let mut out = [0u16; 39];
        assert_eq!(StringFromGUID2(&g, out.as_mut_ptr(), 39), 39);
        assert_eq!(
            String::from_utf16_lossy(&out[..38]),
            "{12345678-9ABC-DEF0-0102-030405060708}"
        );
    }
}
