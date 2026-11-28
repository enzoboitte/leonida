//! Smaller parts of kernel32 and advapi32 a title's DLLs import: pointer
//! encoding, the image a code address lies in, local heaps, messages,
//! strings and locale, fail-fast, event tracing and app state notices.

use std::ffi::c_void;

use super::set_last_error;

const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_FLAGS: u32 = 1004;

/// Windows mixes pointers with a per-process secret; any reversible
/// encoding keeps the contract (decode gives back what was encoded).
fn secret() -> usize {
    static SECRET: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SECRET.get_or_init(|| {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        (t.map_or(0, |d| d.as_nanos() as usize) ^ std::process::id() as usize) | 1
    })
}

#[allow(non_snake_case)]
pub extern "win64" fn EncodePointer(p: usize) -> usize {
    p ^ secret()
}

#[allow(non_snake_case)]
pub extern "win64" fn DecodePointer(p: usize) -> usize {
    p ^ secret()
}

/// The top-level filter's answer when none is installed: on to the next
/// handler (the C runtime then ends the process itself).
#[allow(non_snake_case)]
pub extern "win64" fn UnhandledExceptionFilter(_pointers: *const c_void) -> i32 {
    0 // EXCEPTION_CONTINUE_SEARCH
}

#[allow(non_snake_case)]
pub extern "win64" fn RtlPcToFileHeader(pc: usize, base: *mut usize) -> usize {
    let found = leonida_loader::image_base(pc).unwrap_or(0);
    if !base.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *base = found };
    }
    found
}

/// shortcut: no frames are given (telemetry and asserts take it as an
/// unknown stack); walk the title's frames with `unwind` if one needs them.
/// A module by name, or (`GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS`) the one an
/// address lies in. Title images are never unloaded, so pinning changes nothing.
#[allow(non_snake_case)]
pub extern "win64" fn GetModuleHandleExW(flags: u32, name: *const u16, out: *mut usize) -> i32 {
    const FROM_ADDRESS: u32 = 4;
    let module = if flags & FROM_ADDRESS != 0 {
        leonida_loader::image_base(name as usize).unwrap_or(0)
    } else {
        crate::modules::kernel::GetModuleHandleW(name) as usize
    };
    if !out.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *out = module };
    }
    if module == 0 {
        set_last_error(126); // ERROR_MOD_NOT_FOUND
    }
    (module != 0).into()
}

#[allow(non_snake_case)]
pub extern "win64" fn RtlCaptureStackBackTrace(
    _: u32,
    _: u32,
    _: *mut usize,
    hash: *mut u32,
) -> u16 {
    if !hash.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *hash = 0 };
    }
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn DebugBreak() {
    // A breakpoint exception, through the title's handlers as on Windows.
    // SAFETY: int3 only raises SIGTRAP, which the exception dispatcher takes.
    unsafe { std::arch::asm!("int3") };
}

#[allow(non_snake_case)]
pub extern "win64" fn RaiseFailFastException(_: *const c_void, _: *const c_void, _: u32) -> ! {
    let text = "Leonida Legacy: the title failed fast (RaiseFailFastException)";
    tracing::error!("{text}");
    eprintln!("{text}");
    std::process::abort()
}

/// shortcut: a full fence on this thread; Windows also makes every other
/// processor drain its writes, which macOS gives no call for.
#[allow(non_snake_case)]
pub extern "win64" fn FlushProcessWriteBuffers() {
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}

// Local and global heaps are the process heap here.

#[allow(non_snake_case)]
pub extern "win64" fn LocalAlloc(flags: u32, size: usize) -> *mut c_void {
    // LMEM_ZEROINIT is HEAP_ZERO_MEMORY's place in these flags.
    let zero = if flags & 0x40 != 0 { 0x08 } else { 0 };
    super::memory::HeapAlloc(super::memory::GetProcessHeap(), zero, size)
}

#[allow(non_snake_case)]
pub extern "win64" fn LocalFree(block: *mut c_void) -> *mut c_void {
    super::memory::HeapFree(super::memory::GetProcessHeap(), 0, block);
    std::ptr::null_mut()
}

#[allow(non_snake_case)]
pub extern "win64" fn GlobalFree(block: *mut c_void) -> *mut c_void {
    LocalFree(block)
}

/// shortcut: system messages are "Error <n>" (no message tables here);
/// messages from a module's table are not formatted.
#[allow(non_snake_case)]
pub extern "win64" fn FormatMessageW(
    flags: u32,
    _source: *const c_void,
    id: u32,
    _language: u32,
    buffer: *mut u16,
    size: u32,
    _args: *const c_void,
) -> u32 {
    const ALLOCATE: u32 = 0x100;
    const FROM_SYSTEM: u32 = 0x1000;
    if flags & FROM_SYSTEM == 0 || buffer.is_null() {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let text: Vec<u16> = format!("Error {id}\r\n")
        .encode_utf16()
        .chain([0])
        .collect();
    let n = text.len() - 1;
    let out = if flags & ALLOCATE != 0 {
        let block = LocalAlloc(0, text.len() * 2).cast::<u16>();
        // SAFETY: with ALLOCATE the buffer argument is where the block goes.
        unsafe { *buffer.cast::<*mut u16>() = block };
        block
    } else if (size as usize) < text.len() {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    } else {
        buffer
    };
    // SAFETY: room for the text and its terminator, allocated or checked.
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), out, text.len()) };
    n as u32
}

#[allow(non_snake_case)]
pub extern "win64" fn FormatMessageA(
    flags: u32,
    source: *const c_void,
    id: u32,
    language: u32,
    buffer: *mut u8,
    size: u32,
    args: *const c_void,
) -> u32 {
    let mut wide = [0u16; 64];
    let n = FormatMessageW(
        flags & !0x100,
        source,
        id,
        language,
        wide.as_mut_ptr(),
        64,
        args,
    ) as usize;
    if n == 0 {
        return 0;
    }
    let text: Vec<u8> = String::from_utf16_lossy(&wide[..n])
        .bytes()
        .chain([0])
        .collect();
    let out = if flags & 0x100 != 0 {
        let block = LocalAlloc(0, text.len()).cast::<u8>();
        // SAFETY: with ALLOCATE the buffer argument is where the block goes.
        unsafe { *buffer.cast::<*mut u8>() = block };
        block
    } else if (size as usize) < text.len() {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    } else {
        buffer
    };
    // SAFETY: room for the text and its terminator.
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), out, text.len()) };
    n as u32
}

/// # Safety
/// `s` is `n` units, or NUL-terminated when `n` is negative.
unsafe fn units<'a>(s: *const u16, n: i32) -> &'a [u16] {
    let len = if n < 0 {
        // SAFETY: up to the terminator.
        (0..).take_while(|&i| unsafe { *s.add(i) } != 0).count()
    } else {
        n as usize
    };
    // SAFETY: as counted.
    unsafe { std::slice::from_raw_parts(s, len) }
}

fn fold(u: u16) -> u16 {
    char::from_u32(u.into()).map_or(u, |c| {
        let mut l = c.to_lowercase();
        match (l.next(), l.next()) {
            (Some(c), None) if (c as u32) < 0x1_0000 => c as u16,
            _ => u,
        }
    })
}

#[allow(non_snake_case)]
pub extern "win64" fn lstrlenW(s: *const u16) -> i32 {
    if s.is_null() {
        return 0;
    }
    // SAFETY: a NUL-terminated string.
    unsafe { units(s, -1) }.len() as i32
}

#[allow(non_snake_case)]
pub extern "win64" fn lstrcmpiW(a: *const u16, b: *const u16) -> i32 {
    // SAFETY: NUL-terminated strings.
    let (a, b) = unsafe { (units(a, -1), units(b, -1)) };
    let order = a.iter().map(|&u| fold(u)).cmp(b.iter().map(|&u| fold(u)));
    order as i32
}

/// shortcut: an ordinal comparison (case folded with `NORM_IGNORECASE`),
/// not the locale's collation.
#[allow(non_snake_case)]
pub extern "win64" fn CompareStringEx(
    _locale: *const u16,
    flags: u32,
    a: *const u16,
    na: i32,
    b: *const u16,
    nb: i32,
    _: *const c_void,
    _: *const c_void,
    _: isize,
) -> i32 {
    // SAFETY: the title's strings, as counted.
    let (a, b) = unsafe { (units(a, na), units(b, nb)) };
    let key = |u: u16| if flags & 1 != 0 { fold(u) } else { u };
    // CSTR_LESS_THAN 1, CSTR_EQUAL 2, CSTR_GREATER_THAN 3.
    a.iter().map(|&u| key(u)).cmp(b.iter().map(|&u| key(u))) as i32 + 2
}

fn ctype1(u: u16) -> u16 {
    let Some(c) = char::from_u32(u.into()) else {
        return 0;
    };
    let mut t = 0x200; // C1_DEFINED
    for (has, bit) in [
        (c.is_uppercase(), 0x1),
        (c.is_lowercase(), 0x2),
        (c.is_ascii_digit(), 0x4),
        (c.is_whitespace(), 0x8),
        (c.is_ascii_punctuation(), 0x10),
        (c.is_control(), 0x20),
        (c == ' ' || c == '\t', 0x40),
        (c.is_ascii_hexdigit(), 0x80),
        (c.is_alphabetic(), 0x100),
    ] {
        if has {
            t |= bit;
        }
    }
    t
}

#[allow(non_snake_case)]
pub extern "win64" fn GetStringTypeW(kind: u32, s: *const u16, n: i32, out: *mut u16) -> i32 {
    if kind != 1 {
        // Only CT_CTYPE1.
        set_last_error(ERROR_INVALID_FLAGS);
        return 0;
    }
    // SAFETY: the title's string, as counted.
    let s = unsafe { units(s, n) };
    for (i, &u) in s.iter().enumerate() {
        // SAFETY: the title passes room for one type per unit.
        unsafe { *out.add(i) = ctype1(u) };
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn GetStringTypeExW(
    _locale: u32,
    kind: u32,
    s: *const u16,
    n: i32,
    out: *mut u16,
) -> i32 {
    GetStringTypeW(kind, s, n, out)
}

#[allow(non_snake_case)]
pub extern "win64" fn LCMapStringEx(
    _locale: *const u16,
    flags: u32,
    s: *const u16,
    n: i32,
    out: *mut u16,
    size: i32,
    _: *const c_void,
    _: *const c_void,
    _: isize,
) -> i32 {
    const LOWER: u32 = 0x100;
    const UPPER: u32 = 0x200;
    // SAFETY: the title's string, as counted.
    let mut s: Vec<u16> = unsafe { units(s, n) }.to_vec();
    if n < 0 {
        s.push(0);
    }
    let text = String::from_utf16_lossy(&s);
    let mapped: Vec<u16> = match flags & (LOWER | UPPER) {
        LOWER => text.to_lowercase().encode_utf16().collect(),
        UPPER => text.to_uppercase().encode_utf16().collect(),
        _ => s,
    };
    if size == 0 {
        return mapped.len() as i32;
    }
    if mapped.len() > size as usize {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    // SAFETY: the title's buffer of `size` units.
    unsafe { std::ptr::copy_nonoverlapping(mapped.as_ptr(), out, mapped.len()) };
    mapped.len() as i32
}

/// shortcut: the console's locale is en-US here; only its name is given.
#[allow(non_snake_case)]
pub extern "win64" fn GetLocaleInfoEx(
    _locale: *const u16,
    kind: u32,
    out: *mut u16,
    size: i32,
) -> i32 {
    const LOCALE_SNAME: u32 = 0x5c;
    if kind & 0xFFFF != LOCALE_SNAME {
        set_last_error(ERROR_INVALID_FLAGS);
        return 0;
    }
    let name: Vec<u16> = "en-US\0".encode_utf16().collect();
    if size == 0 {
        return name.len() as i32;
    }
    if (size as usize) < name.len() {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    // SAFETY: the title's buffer of `size` units.
    unsafe { std::ptr::copy_nonoverlapping(name.as_ptr(), out, name.len()) };
    name.len() as i32
}

#[allow(non_snake_case)]
pub extern "win64" fn GetUserDefaultUILanguage() -> u16 {
    0x0409 // en-US
}

#[allow(non_snake_case)]
pub extern "win64" fn SetEnvironmentVariableA(name: *const u8, value: *const u8) -> i32 {
    let wide = |s: *const u8| -> Option<Vec<u16>> {
        (!s.is_null()).then(|| {
            // SAFETY: a NUL-terminated string.
            let s = unsafe { std::ffi::CStr::from_ptr(s.cast()) }
                .to_string_lossy()
                .into_owned();
            s.encode_utf16().chain([0]).collect()
        })
    };
    let (Some(name), value) = (wide(name), wide(value)) else {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    };
    let value_ptr = value.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
    super::system::SetEnvironmentVariableW(name.as_ptr(), value_ptr)
}

#[allow(non_snake_case)]
pub extern "win64" fn SetThreadIdealProcessorEx(
    _: usize,
    _: *const c_void,
    previous: *mut u32,
) -> i32 {
    if !previous.is_null() {
        // SAFETY: the title's PROCESSOR_NUMBER (group u16, number u8, reserved u8).
        unsafe { *previous = 0 };
    }
    1
}

/// The clock ticks every 15.625 ms and is not adjusted, as by default.
#[allow(non_snake_case)]
pub extern "win64" fn GetSystemTimeAdjustment(
    adjustment: *mut u32,
    increment: *mut u32,
    disabled: *mut i32,
) -> i32 {
    // SAFETY: the title's out pointers.
    unsafe {
        *adjustment = 156_250;
        *increment = 156_250;
        *disabled = 1;
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn GetConsoleWindow() -> usize {
    0
}

// Event tracing (ETW): a console without a tracing session; every event is
// accepted and dropped.

#[allow(non_snake_case)]
pub extern "win64" fn EventRegister(_: *const c_void, _: usize, _: usize, handle: *mut u64) -> u32 {
    if !handle.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *handle = 1 };
    }
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn EventUnregister(_: u64) -> u32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn EventSetInformation(_: u64, _: u32, _: *const c_void, _: u32) -> u32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn EventWriteTransfer(
    _: u64,
    _: *const c_void,
    _: *const c_void,
    _: *const c_void,
    _: u32,
    _: *const c_void,
) -> u32 {
    0
}

// App state notices (suspend and resume): a console title registers for
// them. shortcut: nothing suspends a title here yet, so none is sent.

#[allow(non_snake_case)]
pub extern "win64" fn RegisterAppStateChangeNotification(
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
pub extern "win64" fn UnregisterAppStateChangeNotification(_: usize) {}

// Certificates (crypt32): a console with an empty certificate store; no
// certificate is ever found, so nothing can be verified or presented.

const CRYPT_E_NOT_FOUND: u32 = 0x8009_2004;
/// The one store handle; it holds nothing.
const STORE: usize = 0x5703_0000;

#[allow(non_snake_case)]
pub extern "win64" fn CertOpenStore(_: usize, _: u32, _: usize, _: u32, _: *const c_void) -> usize {
    STORE
}

#[allow(non_snake_case)]
pub extern "win64" fn CertOpenSystemStoreW(_: usize, _: *const u16) -> usize {
    STORE
}

#[allow(non_snake_case)]
pub extern "win64" fn CertCloseStore(_: usize, _: u32) -> i32 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn CertFindCertificateInStore(
    _: usize,
    _: u32,
    _: u32,
    _: u32,
    _: *const c_void,
    _: usize,
) -> usize {
    set_last_error(CRYPT_E_NOT_FOUND);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn CertFindChainInStore(
    _: usize,
    _: u32,
    _: u32,
    _: u32,
    _: *const c_void,
    _: usize,
) -> usize {
    set_last_error(CRYPT_E_NOT_FOUND);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn CertFreeCertificateContext(_: usize) -> i32 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn CertFreeCertificateChain(_: usize) {}

/// Base64 (with or without a PEM header line) to bytes; other encodings are
/// refused. As Windows: with no output buffer, the size it needs.
#[allow(non_snake_case)]
pub extern "win64" fn CryptStringToBinaryW(
    text: *const u16,
    length: u32,
    flags: u32,
    out: *mut u8,
    size: *mut u32,
    _skip: *mut u32,
    used_flags: *mut u32,
) -> i32 {
    const BASE64HEADER: u32 = 0;
    const BASE64: u32 = 1;
    if flags != BASE64HEADER && flags != BASE64 || size.is_null() {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    // SAFETY: the title's text, `length` units or NUL-terminated.
    let text = String::from_utf16_lossy(unsafe {
        units(text, if length == 0 { -1 } else { length as i32 })
    });
    let body: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
    let Some(bytes) = base64(&body) else {
        set_last_error(0x8009_0005); // NTE_BAD_DATA
        return 0;
    };
    // SAFETY: the title's in-out size, and its buffer of that size or null.
    unsafe {
        if !used_flags.is_null() {
            *used_flags = flags;
        }
        if !out.is_null() {
            if (*size as usize) < bytes.len() {
                *size = bytes.len() as u32;
                set_last_error(ERROR_INSUFFICIENT_BUFFER);
                return 0;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
        }
        *size = bytes.len() as u32;
    }
    1
}

fn base64(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        bits = bits << 6 | u32::from(v);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

// Classic event tracing: no session ever enables a provider.

#[allow(non_snake_case)]
pub extern "win64" fn RegisterTraceGuidsW(
    _: usize,
    _: usize,
    _: *const c_void,
    _: u32,
    _: *const c_void,
    _: *const u16,
    _: *const u16,
    handle: *mut u64,
) -> u32 {
    if !handle.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *handle = 1 };
    }
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn UnregisterTraceGuids(_: u64) -> u32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn GetTraceLoggerHandle(_: *const c_void) -> u64 {
    set_last_error(ERROR_INVALID_PARAMETER);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn GetTraceEnableFlags(_: u64) -> u32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn GetTraceEnableLevel(_: u64) -> u8 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn TraceMessage(_: u64, _: u32, _: *const c_void, _: u16) -> u32 {
    0
}

/// shortcut: images' resources are not read; no resource is found.
#[allow(non_snake_case)]
pub extern "win64" fn FindResourceExW(_: usize, _: *const u16, _: *const u16, _: u16) -> usize {
    set_last_error(1813); // ERROR_RESOURCE_TYPE_NOT_FOUND
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn LoadResource(_: usize, _: usize) -> usize {
    set_last_error(1812); // ERROR_RESOURCE_DATA_NOT_FOUND
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn GetConsoleScreenBufferInfo(_: usize, _: *mut c_void) -> i32 {
    set_last_error(6); // ERROR_INVALID_HANDLE: no console
    0
}

// Thread attribute lists: accepted and not applied (`CreateRemoteThreadEx`
// starts the thread as `CreateThread` does).

const ATTRIBUTE_SLOT: usize = 24;

#[allow(non_snake_case)]
pub extern "win64" fn InitializeProcThreadAttributeList(
    list: *mut c_void,
    count: u32,
    _: u32,
    size: *mut usize,
) -> i32 {
    let need = 16 + count as usize * ATTRIBUTE_SLOT;
    // SAFETY: the title's in-out size.
    let have = unsafe { std::mem::replace(&mut *size, need) };
    if list.is_null() || have < need {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn UpdateProcThreadAttribute(
    _: *mut c_void,
    _: u32,
    _: usize,
    _: *const c_void,
    _: usize,
    _: *mut c_void,
    _: *const usize,
) -> i32 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn DeleteProcThreadAttributeList(_: *mut c_void) {}

#[allow(non_snake_case)]
pub extern "win64" fn CreateRemoteThreadEx(
    process: usize,
    security: *const c_void,
    stack: usize,
    start: usize,
    parameter: usize,
    flags: u32,
    _attributes: *const c_void,
    id: *mut u32,
) -> usize {
    if process != super::sync::CURRENT_PROCESS {
        set_last_error(5); // ERROR_ACCESS_DENIED: no other process here
        return 0;
    }
    super::thread::CreateThread(security, stack, start, parameter, flags, id)
}

/// Cores (`RelationProcessorCore`), and with `RelationAll` one package
/// holding them all: one logical processor per core (no SMT is reported).
#[allow(non_snake_case)]
pub extern "win64" fn GetLogicalProcessorInformationEx(
    relation: u32,
    out: *mut u8,
    length: *mut u32,
) -> i32 {
    const CORE: u32 = 0;
    const PACKAGE: u32 = 3;
    const ALL: u32 = 0xFFFF;
    const ENTRY: usize = 48;
    let n = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(64);
    let mut entries: Vec<(u32, u64)> = Vec::new();
    if relation == CORE || relation == ALL {
        entries.extend((0..n).map(|i| (CORE, 1u64 << i)));
    }
    if relation == PACKAGE || relation == ALL {
        entries.push((PACKAGE, if n == 64 { u64::MAX } else { (1 << n) - 1 }));
    }
    let need = (entries.len() * ENTRY) as u32;
    // SAFETY: the title's in-out length.
    let have = unsafe { std::mem::replace(&mut *length, need) };
    if out.is_null() || have < need {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    for (i, (kind, mask)) in entries.into_iter().enumerate() {
        let mut e = [0u8; ENTRY];
        e[0..4].copy_from_slice(&kind.to_le_bytes());
        e[4..8].copy_from_slice(&(ENTRY as u32).to_le_bytes());
        // PROCESSOR_RELATIONSHIP: flags, efficiency, reserved, one group.
        e[30..32].copy_from_slice(&1u16.to_le_bytes());
        e[32..40].copy_from_slice(&mask.to_le_bytes());
        // SAFETY: the title's buffer, `need` bytes long (checked).
        unsafe { std::ptr::copy_nonoverlapping(e.as_ptr(), out.add(i * ENTRY), ENTRY) };
    }
    1
}

/// shortcut: no sound yet on macOS; XAudio2 cannot be made, as on a
/// console with no audio device (`XAUDIO2_E_DEVICE_INVALIDATED`).
#[allow(non_snake_case)]
pub extern "win64" fn XAudio2Create(out: *mut usize, _: u32, _: u32) -> i32 {
    if !out.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *out = 0 };
    }
    0x8896_0004_u32 as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    #[test]
    fn pointers_decode_to_what_was_encoded() {
        let p = 0x1234_5678_9abc_usize;
        assert_ne!(EncodePointer(p), p);
        assert_eq!(DecodePointer(EncodePointer(p)), p);
    }

    #[test]
    fn strings_compare_map_and_classify() {
        assert_eq!(lstrlenW(w("abc").as_ptr()), 3);
        assert_eq!(lstrcmpiW(w("Hello").as_ptr(), w("hELLO").as_ptr()), 0);
        assert!(lstrcmpiW(w("a").as_ptr(), w("B").as_ptr()) < 0);
        let (a, b) = (w("abc"), w("ABD"));
        assert_eq!(
            CompareStringEx(
                std::ptr::null(),
                1,
                a.as_ptr(),
                -1,
                b.as_ptr(),
                -1,
                std::ptr::null(),
                std::ptr::null(),
                0
            ),
            1
        );
        let mut out = [0u16; 8];
        let n = LCMapStringEx(
            std::ptr::null(),
            0x200,
            w("abé").as_ptr(),
            -1,
            out.as_mut_ptr(),
            8,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        assert_eq!(String::from_utf16_lossy(&out[..n as usize]), "ABÉ\0");
        let mut types = [0u16; 3];
        GetStringTypeW(1, w("A1 ").as_ptr(), 3, types.as_mut_ptr());
        assert_eq!(types.map(|t| t & 0x10f), [0x101, 0x4, 0x8]);
    }

    #[test]
    fn messages_and_local_blocks() {
        let mut block: *mut u16 = std::ptr::null_mut();
        let n = FormatMessageW(
            0x1100,
            std::ptr::null(),
            5,
            0,
            (&raw mut block).cast(),
            0,
            std::ptr::null(),
        );
        // SAFETY: the allocated message, `n` units and a terminator.
        let text =
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(block, n as usize) });
        assert_eq!(text, "Error 5\r\n");
        assert!(LocalFree(block.cast()).is_null());
        let z = LocalAlloc(0x40, 16).cast::<u8>();
        // SAFETY: a zeroed 16-byte block.
        assert!(
            unsafe { std::slice::from_raw_parts(z, 16) }
                .iter()
                .all(|&b| b == 0)
        );
        LocalFree(z.cast());
    }
}
