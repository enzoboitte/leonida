//! Winsock and WinHTTP on macOS: a console without a network cable. On
//! Windows `modules::net` vets each call and hands the rest to the host's
//! Winsock, reaching loopback only; here there is no Winsock below, and the
//! title gets no network at all, so it cannot reach one (the project's
//! "offline by design").
//!
//! `WSAStartup` succeeds, as on a console with its cable out; creating a
//! socket fails with `WSAENETDOWN`, so every call on one fails with
//! `WSAENOTSOCK`; name lookups fail with `WSAHOST_NOT_FOUND`. A WinHTTP
//! session opens, but `WinHttpConnect` fails, so no request can exist, and
//! proxy detection fails.
//!
//! shortcut: no loopback sockets either; a title that talks to itself over
//! 127.0.0.1 needs BSD sockets behind these calls, limited to loopback.

use std::ffi::c_void;

use super::set_last_error;
use crate::{HleExport, HleModule, hle_export};

const SOCKET_ERROR: i32 = -1;
const INVALID_SOCKET: usize = !0;
const WSAENOTSOCK: u32 = 10038;
const WSAEFAULT: u32 = 10014;
const WSAENETDOWN: u32 = 10050;
const WSAHOST_NOT_FOUND: u32 = 11001;
const ERROR_WINHTTP_CANNOT_CONNECT: u32 = 12029;
const ERROR_WINHTTP_AUTODETECTION_FAILED: u32 = 12180;

/// `WSADATA` (x64): versions and limits, vendor info, then two strings.
#[repr(C)]
pub struct WsaData {
    version: u16,
    high_version: u16,
    max_sockets: u16,
    max_udp: u16,
    vendor: usize,
    description: [u8; 257],
    status: [u8; 129],
}

#[allow(non_snake_case)]
pub extern "win64" fn WSAStartup(version: u16, data: *mut WsaData) -> i32 {
    if data.is_null() {
        return WSAEFAULT as i32;
    }
    let mut d = WsaData {
        version,
        high_version: 0x0202,
        max_sockets: 0,
        max_udp: 0,
        vendor: 0,
        description: [0; 257],
        status: [0; 129],
    };
    d.description[..7].copy_from_slice(b"WinSock");
    d.status[..7].copy_from_slice(b"Running");
    // SAFETY: the title's WSADATA.
    unsafe { data.write(d) };
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn WSACleanup() -> i32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn WSAGetLastError() -> i32 {
    super::GetLastError() as i32
}

#[allow(non_snake_case)]
pub extern "win64" fn WSASetLastError(error: i32) {
    set_last_error(error as u32);
}

/// Every socket constructor: no network to make one on.
fn no_socket() -> usize {
    set_last_error(WSAENETDOWN);
    INVALID_SOCKET
}

#[allow(non_snake_case)]
pub extern "win64" fn socket(_: i32, _: i32, _: i32) -> usize {
    no_socket()
}

#[allow(non_snake_case)]
pub extern "win64" fn WSASocketW(
    _: i32,
    _: i32,
    _: i32,
    _: *const c_void,
    _: u32,
    _: u32,
) -> usize {
    no_socket()
}

#[allow(non_snake_case)]
pub extern "win64" fn WSASocketA(
    _: i32,
    _: i32,
    _: i32,
    _: *const c_void,
    _: u32,
    _: u32,
) -> usize {
    no_socket()
}

#[allow(non_snake_case)]
pub extern "win64" fn accept(_: usize, _: *mut c_void, _: *mut i32) -> usize {
    set_last_error(WSAENOTSOCK);
    INVALID_SOCKET
}

/// Every call on a socket: there is none.
fn not_a_socket() -> i32 {
    set_last_error(WSAENOTSOCK);
    SOCKET_ERROR
}

macro_rules! on_socket {
    ($($name:ident($($t:ty),*);)*) => {
        $(
            #[allow(non_snake_case)]
            pub extern "win64" fn $name(_: usize $(, _: $t)*) -> i32 {
                not_a_socket()
            }
        )*
    };
}

on_socket! {
    closesocket();
    connect(*const c_void, i32);
    bind(*const c_void, i32);
    listen(i32);
    send(*const u8, i32, i32);
    recv(*mut u8, i32, i32);
    sendto(*const u8, i32, i32, *const c_void, i32);
    recvfrom(*mut u8, i32, i32, *mut c_void, *mut i32);
    shutdown(i32);
    ioctlsocket(i32, *mut u32);
    setsockopt(i32, i32, *const u8, i32);
    getsockopt(i32, i32, *mut u8, *mut i32);
    getsockname(*mut c_void, *mut i32);
    getpeername(*mut c_void, *mut i32);
    WSAConnect(*const c_void, i32, *const c_void, *mut c_void, *const c_void, *const c_void);
    WSAIoctl(u32, *const c_void, u32, *mut c_void, u32, *mut u32, *mut c_void, *const c_void);
    WSASend(*const c_void, u32, *mut u32, u32, *mut c_void, *const c_void);
    WSARecv(*mut c_void, u32, *mut u32, *mut u32, *mut c_void, *const c_void);
    WSASendTo(*const c_void, u32, *mut u32, u32, *const c_void, i32, *mut c_void, *const c_void);
    WSARecvFrom(*mut c_void, u32, *mut u32, *mut u32, *mut c_void, *mut i32, *mut c_void, *const c_void);
}

/// `select` on no sockets waits out its time; with sockets it fails.
#[allow(non_snake_case)]
pub extern "win64" fn select(
    _: i32,
    read: *const u32,
    write: *const u32,
    except: *const u32,
    timeout: *const [i32; 2],
) -> i32 {
    // An fd_set starts with its count.
    // SAFETY: the title's fd_sets, or null.
    let count = |s: *const u32| if s.is_null() { 0 } else { unsafe { *s } };
    if count(read) + count(write) + count(except) > 0 {
        return not_a_socket();
    }
    if !timeout.is_null() {
        // SAFETY: the title's timeval.
        let [s, us] = unsafe { *timeout };
        let wait = std::time::Duration::from_secs(s.max(0) as u64)
            + std::time::Duration::from_micros(us.max(0) as u64);
        std::thread::sleep(wait);
    }
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn __WSAFDIsSet(_: usize, _: *const u32) -> i32 {
    0
}

fn not_found() -> i32 {
    set_last_error(WSAHOST_NOT_FOUND);
    WSAHOST_NOT_FOUND as i32
}

#[allow(non_snake_case)]
pub extern "win64" fn getaddrinfo(
    _: *const u8,
    _: *const u8,
    _: *const c_void,
    out: *mut usize,
) -> i32 {
    if !out.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *out = 0 };
    }
    not_found()
}

#[allow(non_snake_case)]
pub extern "win64" fn GetAddrInfoW(
    _: *const u16,
    _: *const u16,
    _: *const c_void,
    out: *mut usize,
) -> i32 {
    if !out.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *out = 0 };
    }
    not_found()
}

#[allow(non_snake_case)]
pub extern "win64" fn freeaddrinfo(_: *mut c_void) {}

#[allow(non_snake_case)]
pub extern "win64" fn FreeAddrInfoW(_: *mut c_void) {}

#[allow(non_snake_case)]
pub extern "win64" fn getnameinfo(
    _: *const c_void,
    _: i32,
    _: *mut u8,
    _: u32,
    _: *mut u8,
    _: u32,
    _: i32,
) -> i32 {
    not_found()
}

#[allow(non_snake_case)]
pub extern "win64" fn gethostbyname(_: *const u8) -> *mut c_void {
    not_found();
    std::ptr::null_mut()
}

#[allow(non_snake_case)]
pub extern "win64" fn gethostbyaddr(_: *const u8, _: i32, _: i32) -> *mut c_void {
    not_found();
    std::ptr::null_mut()
}

/// The console's own name.
#[allow(non_snake_case)]
pub extern "win64" fn gethostname(out: *mut u8, size: i32) -> i32 {
    const NAME: &[u8] = b"XBOX\0";
    if out.is_null() || (size as usize) < NAME.len() {
        set_last_error(WSAEFAULT);
        return SOCKET_ERROR;
    }
    // SAFETY: the title's buffer of `size` bytes.
    unsafe { std::ptr::copy_nonoverlapping(NAME.as_ptr(), out, NAME.len()) };
    0
}

/// A dotted IPv4 address in network order, or `INADDR_NONE`.
#[allow(non_snake_case)]
pub extern "win64" fn inet_addr(text: *const u8) -> u32 {
    if text.is_null() {
        return u32::MAX;
    }
    // SAFETY: a NUL-terminated string.
    let text = unsafe { std::ffi::CStr::from_ptr(text.cast()) }.to_string_lossy();
    text.trim()
        .parse::<std::net::Ipv4Addr>()
        .map_or(u32::MAX, |a| u32::from_ne_bytes(a.octets()))
}

#[allow(non_snake_case)]
pub extern "win64" fn htons(v: u16) -> u16 {
    v.to_be()
}

#[allow(non_snake_case)]
pub extern "win64" fn ntohs(v: u16) -> u16 {
    u16::from_be(v)
}

#[allow(non_snake_case)]
pub extern "win64" fn htonl(v: u32) -> u32 {
    v.to_be()
}

#[allow(non_snake_case)]
pub extern "win64" fn ntohl(v: u32) -> u32 {
    u32::from_be(v)
}

// WinHTTP: sessions open, connections do not.

/// A session handle; it holds nothing.
const SESSION: usize = 0xC0DE_0000;

#[allow(non_snake_case)]
pub extern "win64" fn WinHttpOpen(
    _: *const u16,
    _: u32,
    _: *const u16,
    _: *const u16,
    _: u32,
) -> usize {
    SESSION
}

#[allow(non_snake_case)]
pub extern "win64" fn WinHttpConnect(_: usize, _: *const u16, _: u16, _: u32) -> usize {
    set_last_error(ERROR_WINHTTP_CANNOT_CONNECT);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn WinHttpCloseHandle(_: usize) -> i32 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn WinHttpSetOption(_: usize, _: u32, _: *const c_void, _: u32) -> i32 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn WinHttpSetTimeouts(_: usize, _: i32, _: i32, _: i32, _: i32) -> i32 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn WinHttpDetectAutoProxyConfigUrl(_: u32, _: *mut *mut u16) -> i32 {
    set_last_error(ERROR_WINHTTP_AUTODETECTION_FAILED);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn WinHttpGetProxyForUrl(
    _: usize,
    _: *const u16,
    _: *const c_void,
    _: *mut c_void,
) -> i32 {
    set_last_error(ERROR_WINHTTP_AUTODETECTION_FAILED);
    0
}

/// No connection can exist (see `WinHttpConnect`), so nothing made from one
/// can either: every call on a request fails as on a handle of the wrong kind.
const ERROR_WINHTTP_INCORRECT_HANDLE_TYPE: u32 = 12018;
const ERROR_WINHTTP_INVALID_URL: u32 = 12005;

fn no_request() -> i32 {
    set_last_error(ERROR_WINHTTP_INCORRECT_HANDLE_TYPE);
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn WinHttpOpenRequest(
    _: usize,
    _: *const u16,
    _: *const u16,
    _: *const u16,
    _: *const u16,
    _: *const usize,
    _: u32,
) -> usize {
    no_request();
    0
}

macro_rules! on_request {
    ($($name:ident($($t:ty),*);)*) => {
        $(
            #[allow(non_snake_case)]
            pub extern "win64" fn $name(_: usize $(, _: $t)*) -> i32 {
                no_request()
            }
        )*
    };
}

on_request! {
    WinHttpAddRequestHeaders(*const u16, u32, u32);
    WinHttpSendRequest(*const u16, u32, *const c_void, u32, u32, usize);
    WinHttpReceiveResponse(*mut c_void);
    WinHttpQueryHeaders(u32, *const u16, *mut c_void, *mut u32, *mut u32);
    WinHttpQueryDataAvailable(*mut u32);
    WinHttpReadData(*mut c_void, u32, *mut u32);
    WinHttpWriteData(*const c_void, u32, *mut u32);
    WinHttpQueryOption(u32, *mut c_void, *mut u32);
}

/// No previous callback; none will be called (no request ever runs).
#[allow(non_snake_case)]
pub extern "win64" fn WinHttpSetStatusCallback(_: usize, _: usize, _: u32, _: usize) -> usize {
    0
}

/// shortcut: URLs are not taken apart (nothing could be fetched from one).
#[allow(non_snake_case)]
pub extern "win64" fn WinHttpCrackUrl(_: *const u16, _: u32, _: u32, _: *mut c_void) -> i32 {
    set_last_error(ERROR_WINHTTP_INVALID_URL);
    0
}

pub fn module() -> HleModule {
    let mut exports = vec![
        hle_export!(WSAStartup),
        hle_export!(WSACleanup),
        hle_export!(WSAGetLastError),
        hle_export!(WSASetLastError),
        hle_export!(socket),
        hle_export!(WSASocketW),
        hle_export!(WSASocketA),
        hle_export!(accept),
        hle_export!(closesocket),
        hle_export!(connect),
        hle_export!(bind),
        hle_export!(listen),
        hle_export!(send),
        hle_export!(recv),
        hle_export!(sendto),
        hle_export!(recvfrom),
        hle_export!(shutdown),
        hle_export!(ioctlsocket),
        hle_export!(setsockopt),
        hle_export!(getsockopt),
        hle_export!(getsockname),
        hle_export!(getpeername),
        hle_export!(WSAConnect),
        hle_export!(WSAIoctl),
        hle_export!(WSASend),
        hle_export!(WSARecv),
        hle_export!(WSASendTo),
        hle_export!(WSARecvFrom),
        hle_export!(select),
        hle_export!(__WSAFDIsSet),
        hle_export!(getaddrinfo),
        hle_export!(GetAddrInfoW),
        hle_export!(freeaddrinfo),
        hle_export!(FreeAddrInfoW),
        hle_export!(getnameinfo),
        hle_export!(gethostbyname),
        hle_export!(gethostbyaddr),
        hle_export!(gethostname),
        hle_export!(inet_addr),
        hle_export!(htons),
        hle_export!(ntohs),
        hle_export!(htonl),
        hle_export!(ntohl),
        hle_export!(WinHttpOpen),
        hle_export!(WinHttpConnect),
        hle_export!(WinHttpCloseHandle),
        hle_export!(WinHttpSetOption),
        hle_export!(WinHttpSetTimeouts),
        hle_export!(WinHttpDetectAutoProxyConfigUrl),
        hle_export!(WinHttpGetProxyForUrl),
        hle_export!(WinHttpOpenRequest),
        hle_export!(WinHttpAddRequestHeaders),
        hle_export!(WinHttpSendRequest),
        hle_export!(WinHttpReceiveResponse),
        hle_export!(WinHttpQueryHeaders),
        hle_export!(WinHttpQueryDataAvailable),
        hle_export!(WinHttpReadData),
        hle_export!(WinHttpWriteData),
        hle_export!(WinHttpQueryOption),
        hle_export!(WinHttpSetStatusCallback),
        hle_export!(WinHttpCrackUrl),
    ];
    // Winsock's well-known ordinals, for titles that import it by number.
    let ordinals: [(&'static str, &str); 30] = [
        ("#1", "accept"),
        ("#2", "bind"),
        ("#3", "closesocket"),
        ("#4", "connect"),
        ("#5", "getpeername"),
        ("#6", "getsockname"),
        ("#7", "getsockopt"),
        ("#8", "htonl"),
        ("#9", "htons"),
        ("#10", "ioctlsocket"),
        ("#11", "inet_addr"),
        ("#13", "listen"),
        ("#14", "ntohl"),
        ("#15", "ntohs"),
        ("#16", "recv"),
        ("#17", "recvfrom"),
        ("#18", "select"),
        ("#19", "send"),
        ("#20", "sendto"),
        ("#21", "setsockopt"),
        ("#22", "shutdown"),
        ("#23", "socket"),
        ("#51", "gethostbyaddr"),
        ("#52", "gethostbyname"),
        ("#57", "gethostname"),
        ("#111", "WSAGetLastError"),
        ("#112", "WSASetLastError"),
        ("#115", "WSAStartup"),
        ("#116", "WSACleanup"),
        ("#151", "__WSAFDIsSet"),
    ];
    for (ordinal, name) in ordinals {
        if let Some(e) = exports.iter().find(|e| e.name == name).copied() {
            exports.push(HleExport {
                name: ordinal,
                address: e.address,
            });
        }
    }
    HleModule {
        name: "net",
        dlls: &["ws2_32.dll", "winhttp.dll"],
        exports,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_works_and_nothing_reaches_a_network() {
        let mut data = std::mem::MaybeUninit::<WsaData>::uninit();
        assert_eq!(WSAStartup(0x0202, data.as_mut_ptr()), 0);
        assert_eq!(socket(2, 1, 6), INVALID_SOCKET);
        assert_eq!(WSAGetLastError(), WSAENETDOWN as i32);
        assert_eq!(connect(INVALID_SOCKET, std::ptr::null(), 0), SOCKET_ERROR);
        assert_eq!(WSAGetLastError(), WSAENOTSOCK as i32);
        let mut list = 1usize;
        let found = getaddrinfo(
            c"example.com".as_ptr().cast(),
            std::ptr::null(),
            std::ptr::null(),
            &mut list,
        );
        assert_eq!((found, list), (WSAHOST_NOT_FOUND as i32, 0));
        assert_eq!(
            WinHttpConnect(
                WinHttpOpen(std::ptr::null(), 0, std::ptr::null(), std::ptr::null(), 0),
                std::ptr::null(),
                443,
                0
            ),
            0
        );
        assert_eq!(htons(0x1234), 0x3412);
    }

    #[test]
    fn ordinals_name_the_same_functions() {
        let m = module();
        let at = |n: &str| m.exports.iter().find(|e| e.name == n).map(|e| e.address);
        assert_eq!(at("#23"), at("socket"));
        assert_eq!(at("#115"), at("WSAStartup"));
    }
}
