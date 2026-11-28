//! COM, WinRT and their strings, as a console title's runtime libraries
//! use them: task memory, GUIDs, apartments, activation (no class is
//! registered here), `HSTRING`s and `BSTR`s.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

const S_OK: i32 = 0;
const S_FALSE: i32 = 1;
const E_NOTIMPL: i32 = 0x8000_4001_u32 as i32;
const E_INVALIDARG: i32 = 0x8007_0057_u32 as i32;
const REGDB_E_CLASSNOTREG: i32 = 0x8004_0154_u32 as i32;

#[allow(non_snake_case)]
pub extern "win64" fn CoTaskMemAlloc(size: usize) -> *mut c_void {
    super::misc::LocalAlloc(0, size)
}

#[allow(non_snake_case)]
pub extern "win64" fn CoTaskMemFree(block: *mut c_void) {
    if !block.is_null() {
        super::misc::LocalFree(block);
    }
}

/// A random (version 4) GUID.
#[allow(non_snake_case)]
pub extern "win64" fn CoCreateGuid(out: *mut [u8; 16]) -> i32 {
    let mut bytes = [0u8; 16];
    // SAFETY: a 16-byte buffer of ours.
    unsafe { libc::arc4random_buf(bytes.as_mut_ptr().cast(), 16) };
    bytes[7] = bytes[7] & 0x0F | 0x40;
    bytes[8] = bytes[8] & 0x3F | 0x80;
    // SAFETY: the title's GUID.
    unsafe { out.write(bytes) };
    S_OK
}

/// No COM class is registered on this console.
#[allow(non_snake_case)]
pub extern "win64" fn CoCreateInstance(
    _: *const c_void,
    _: *mut c_void,
    _: u32,
    _: *const c_void,
    out: *mut usize,
) -> i32 {
    if !out.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *out = 0 };
    }
    REGDB_E_CLASSNOTREG
}

/// Every thread is in the multithreaded apartment.
#[allow(non_snake_case)]
pub extern "win64" fn CoGetApartmentType(kind: *mut i32, qualifier: *mut i32) -> i32 {
    if kind.is_null() || qualifier.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the title's out pointers.
    unsafe {
        *kind = 1; // APTTYPE_MTA
        *qualifier = 0;
    }
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn CoGetContextToken(token: *mut usize) -> i32 {
    // SAFETY: the title's out pointer.
    unsafe { *token = 1 };
    S_OK
}

// shortcut: no object context, marshaling or free-threaded marshaler: every
// interface is used where it was made (all threads share one apartment).
#[allow(non_snake_case)]
pub extern "win64" fn CoGetObjectContext(_: *const c_void, out: *mut usize) -> i32 {
    // SAFETY: the title's out pointer.
    unsafe { *out = 0 };
    E_NOTIMPL
}

#[allow(non_snake_case)]
pub extern "win64" fn CoCreateFreeThreadedMarshaler(_: *mut c_void, out: *mut usize) -> i32 {
    // SAFETY: the title's out pointer.
    unsafe { *out = 0 };
    E_NOTIMPL
}

#[allow(non_snake_case)]
pub extern "win64" fn CoMarshalInterThreadInterfaceInStream(
    _: *const c_void,
    _: *mut c_void,
    out: *mut usize,
) -> i32 {
    // SAFETY: the title's out pointer.
    unsafe { *out = 0 };
    E_NOTIMPL
}

#[allow(non_snake_case)]
pub extern "win64" fn CoGetInterfaceAndReleaseStream(
    _: *mut c_void,
    _: *const c_void,
    out: *mut usize,
) -> i32 {
    // SAFETY: the title's out pointer.
    unsafe { *out = 0 };
    E_NOTIMPL
}

static SERVER: AtomicUsize = AtomicUsize::new(0);

#[allow(non_snake_case)]
pub extern "win64" fn CoAddRefServerProcess() -> u32 {
    SERVER.fetch_add(1, SeqCst) as u32 + 1
}

#[allow(non_snake_case)]
pub extern "win64" fn CoReleaseServerProcess() -> u32 {
    SERVER.fetch_sub(1, SeqCst).saturating_sub(1) as u32
}

/// shortcut: the variant is emptied without freeing what it points to.
#[allow(non_snake_case)]
pub extern "win64" fn PropVariantClear(v: *mut [u64; 3]) -> i32 {
    if !v.is_null() {
        // SAFETY: the title's PROPVARIANT, 24 bytes.
        unsafe { v.write([0; 3]) };
    }
    S_OK
}

// WinRT.

#[allow(non_snake_case)]
pub extern "win64" fn RoInitialize(_kind: i32) -> i32 {
    S_FALSE // already: every thread is in one apartment
}

#[allow(non_snake_case)]
pub extern "win64" fn RoUninitialize() {}

#[allow(non_snake_case)]
pub extern "win64" fn RoGetActivationFactory(
    _class: usize,
    _iid: *const c_void,
    out: *mut usize,
) -> i32 {
    if !out.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *out = 0 };
    }
    REGDB_E_CLASSNOTREG
}

/// shortcut: factories the title registers are accepted, not looked up.
#[allow(non_snake_case)]
pub extern "win64" fn RoRegisterActivationFactories(
    _: *const usize,
    _: *const usize,
    _: u32,
    cookie: *mut usize,
) -> i32 {
    if !cookie.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *cookie = 1 };
    }
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn RoRevokeActivationFactories(_: usize) {}

#[allow(non_snake_case)]
pub extern "win64" fn RoGetApartmentIdentifier(id: *mut u64) -> i32 {
    // SAFETY: the title's out pointer.
    unsafe { *id = 1 };
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn RoRegisterForApartmentShutdown(
    _: *mut c_void,
    id: *mut u64,
    cookie: *mut usize,
) -> i32 {
    // SAFETY: the title's out pointers.
    unsafe {
        if !id.is_null() {
            *id = 1;
        }
        if !cookie.is_null() {
            *cookie = 1;
        }
    }
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn RoUnregisterForApartmentShutdown(_: usize) -> i32 {
    S_OK
}

/// WinRT errors are logged; there is no debugger to hand them to.
#[allow(non_snake_case)]
pub extern "win64" fn RoOriginateError(error: i32, _message: usize) -> i32 {
    tracing::debug!(error = format_args!("{error:#x}"), "RoOriginateError");
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn RoTransformError(old: i32, new: i32, _message: usize) -> i32 {
    tracing::debug!(
        old = format_args!("{old:#x}"),
        new = format_args!("{new:#x}"),
        "RoTransformError"
    );
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn GetRestrictedErrorInfo(out: *mut usize) -> i32 {
    // SAFETY: the title's out pointer.
    unsafe { *out = 0 };
    S_FALSE
}

#[allow(non_snake_case)]
pub extern "win64" fn SetRestrictedErrorInfo(_: *mut c_void) -> i32 {
    S_OK
}

// HSTRING: our header holds the length and the characters (with a NUL);
// a string reference lives in the title's HSTRING_HEADER and points at its
// own characters. The empty string is null.

const OWNED: u32 = 0x4853_544F; // "HSTO"
const REFERENCE: u32 = 0x4853_5452; // "HSTR"

#[repr(C)]
pub struct Header {
    kind: u32,
    length: u32,
    chars: *const u16,
}

/// # Safety
/// `h` is null or an HSTRING of ours.
unsafe fn chars<'a>(h: *const Header) -> &'a [u16] {
    if h.is_null() {
        return &[];
    }
    // SAFETY: the caller's guarantee.
    unsafe { std::slice::from_raw_parts((*h).chars, (*h).length as usize) }
}

fn owned(units: &[u16]) -> *mut Header {
    if units.is_empty() {
        return std::ptr::null_mut();
    }
    let mut copy = units.to_vec();
    copy.push(0);
    let chars = Box::leak(copy.into_boxed_slice()).as_ptr();
    Box::into_raw(Box::new(Header {
        kind: OWNED,
        length: units.len() as u32,
        chars,
    }))
}

#[allow(non_snake_case)]
pub extern "win64" fn WindowsCreateString(
    source: *const u16,
    length: u32,
    out: *mut *mut Header,
) -> i32 {
    if out.is_null() || source.is_null() && length > 0 {
        return E_INVALIDARG;
    }
    let units = if length == 0 {
        &[][..]
    } else {
        // SAFETY: the title passes `length` units.
        unsafe { std::slice::from_raw_parts(source, length as usize) }
    };
    // SAFETY: the title's out pointer.
    unsafe { *out = owned(units) };
    S_OK
}

/// The title's `HSTRING_HEADER` (24 bytes) holds the reference.
#[allow(non_snake_case)]
pub extern "win64" fn WindowsCreateStringReference(
    source: *const u16,
    length: u32,
    header: *mut Header,
    out: *mut *mut Header,
) -> i32 {
    if out.is_null() || header.is_null() {
        return E_INVALIDARG;
    }
    let made = if length == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: the title's header, as long as the reference is used.
        unsafe {
            header.write(Header {
                kind: REFERENCE,
                length,
                chars: source,
            })
        };
        header
    };
    // SAFETY: the title's out pointer.
    unsafe { *out = made };
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn WindowsDeleteString(h: *mut Header) -> i32 {
    // SAFETY: null or an HSTRING of ours.
    if !h.is_null() && unsafe { (*h).kind } == OWNED {
        // SAFETY: made by `owned`, deleted once.
        unsafe {
            let h = Box::from_raw(h);
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                h.chars.cast_mut(),
                h.length as usize + 1,
            )));
        }
    }
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn WindowsDuplicateString(h: *const Header, out: *mut *mut Header) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: null or an HSTRING of ours; a copy of its characters.
    unsafe { *out = owned(chars(h)) };
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn WindowsGetStringLen(h: *const Header) -> u32 {
    // SAFETY: null or an HSTRING of ours.
    unsafe { chars(h) }.len() as u32
}

#[allow(non_snake_case)]
pub extern "win64" fn WindowsGetStringRawBuffer(h: *const Header, length: *mut u32) -> *const u16 {
    static EMPTY: u16 = 0;
    // SAFETY: null or an HSTRING of ours.
    let units = unsafe { chars(h) };
    if !length.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *length = units.len() as u32 };
    }
    if h.is_null() { &EMPTY } else { units.as_ptr() }
}

#[allow(non_snake_case)]
pub extern "win64" fn WindowsIsStringEmpty(h: *const Header) -> i32 {
    // SAFETY: null or an HSTRING of ours.
    unsafe { chars(h) }.is_empty().into()
}

#[allow(non_snake_case)]
pub extern "win64" fn WindowsStringHasEmbeddedNull(h: *const Header, out: *mut i32) -> i32 {
    // SAFETY: null or an HSTRING of ours; the title's out pointer.
    unsafe { *out = chars(h).contains(&0).into() };
    S_OK
}

#[allow(non_snake_case)]
pub extern "win64" fn WindowsCompareStringOrdinal(
    a: *const Header,
    b: *const Header,
    out: *mut i32,
) -> i32 {
    // SAFETY: nulls or HSTRINGs of ours; the title's out pointer.
    unsafe { *out = chars(a).cmp(chars(b)) as i32 };
    S_OK
}

// BSTR (oleaut32, by ordinal): a 4-byte byte length, then the characters
// and a NUL; the pointer is to the characters.

/// `SysAllocStringLen` (#4): `length` units from `source` (or zeros).
#[allow(non_snake_case)]
pub extern "win64" fn SysAllocStringLen(source: *const u16, length: u32) -> *mut u16 {
    let bytes = length as usize * 2;
    let block = super::misc::LocalAlloc(0x40, 4 + bytes + 2).cast::<u8>();
    if block.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a zeroed block of 4 + bytes + 2; the title passes `length` units or null.
    unsafe {
        block.cast::<u32>().write(bytes as u32);
        let chars = block.add(4).cast::<u16>();
        if !source.is_null() {
            std::ptr::copy_nonoverlapping(source, chars, length as usize);
        }
        chars
    }
}

/// `SysFreeString` (#6).
#[allow(non_snake_case)]
pub extern "win64" fn SysFreeString(s: *mut u16) {
    if !s.is_null() {
        // SAFETY: a BSTR of ours: its block starts 4 bytes before.
        super::misc::LocalFree(unsafe { s.cast::<u8>().sub(4) }.cast());
    }
}

pub fn oleaut32() -> crate::HleModule {
    let alloc = crate::HleExport {
        name: "#4",
        address: SysAllocStringLen as *const () as usize,
    };
    let free = crate::HleExport {
        name: "#6",
        address: SysFreeString as *const () as usize,
    };
    crate::HleModule {
        name: "oleaut32",
        dlls: &["oleaut32.dll"],
        exports: vec![
            alloc,
            free,
            crate::HleExport {
                name: "SysAllocStringLen",
                ..alloc
            },
            crate::HleExport {
                name: "SysFreeString",
                ..free
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hstrings_are_made_referenced_duplicated_and_deleted() {
        let text: Vec<u16> = "Leonida".encode_utf16().collect();
        let mut h = std::ptr::null_mut();
        assert_eq!(WindowsCreateString(text.as_ptr(), 7, &mut h), S_OK);
        assert_eq!(WindowsGetStringLen(h), 7);
        let mut n = 0;
        let raw = WindowsGetStringRawBuffer(h, &mut n);
        // SAFETY: 7 units and a NUL.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(raw, 8) },
            [&text[..], &[0]].concat()
        );
        let mut header = std::mem::MaybeUninit::<Header>::uninit();
        let mut r = std::ptr::null_mut();
        assert_eq!(
            WindowsCreateStringReference(text.as_ptr(), 7, header.as_mut_ptr(), &mut r),
            S_OK
        );
        let mut order = 9;
        WindowsCompareStringOrdinal(h, r, &mut order);
        assert_eq!(order, 0);
        let mut d = std::ptr::null_mut();
        WindowsDuplicateString(r, &mut d);
        assert_eq!(WindowsGetStringLen(d), 7);
        assert_eq!(WindowsIsStringEmpty(std::ptr::null()), 1);
        assert_eq!(
            *unsafe { &*WindowsGetStringRawBuffer(std::ptr::null(), &mut n) },
            0
        );
        WindowsDeleteString(h);
        WindowsDeleteString(d);
        WindowsDeleteString(r);
    }

    #[test]
    fn bstrs_carry_their_byte_length() {
        let text: Vec<u16> = "abc".encode_utf16().collect();
        let s = SysAllocStringLen(text.as_ptr(), 3);
        // SAFETY: a BSTR of ours: length before, NUL after.
        unsafe {
            assert_eq!(s.cast::<u8>().sub(4).cast::<u32>().read(), 6);
            assert_eq!(*s.add(3), 0);
        }
        SysFreeString(s);
        let mut g = [0u8; 16];
        assert_eq!(CoCreateGuid(&mut g), S_OK);
        assert_eq!(g[7] >> 4, 4, "version 4");
    }
}
