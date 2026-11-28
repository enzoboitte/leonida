//! The rest of the C runtime the game calls: math, strings (narrow and
//! UTF-16), character classes, conversions, sorting, random numbers, time,
//! the C locale, and the file system calls of the runtime. Where Windows and
//! the host agree the host's libc does the work; where they differ (`long`
//! is 32 bits, `wchar_t` 16, `rand`'s formula, `struct _stat64`) Windows'
//! way is kept.

use std::cell::Cell;
use std::ffi::{c_char, c_int, c_long, c_void};

use crate::{HleExport, hle_export};

const ERANGE: i32 = 34;
const EINVAL: i32 = 22;

fn set_errno(e: i32) {
    // SAFETY: this thread's errno.
    unsafe { *libc::__error() = e };
}

unsafe extern "C" {
    fn frexp(x: f64, e: *mut c_int) -> f64;
    fn ldexp(x: f64, e: c_int) -> f64;
    fn modf(x: f64, i: *mut f64) -> f64;
    fn modff(x: f32, i: *mut f32) -> f32;
    fn nextafterf(x: f32, y: f32) -> f32;
    fn nextafter(x: f64, y: f64) -> f64;
    fn remainderf(x: f32, y: f32) -> f32;
    fn erff(x: f32) -> f32;
    fn lroundf(x: f32) -> c_long;
    fn strtok_r(s: *mut c_char, d: *const c_char, ctx: *mut *mut c_char) -> *mut c_char;
    fn timegm(tm: *mut libc::tm) -> libc::time_t;
}

macro_rules! unary {
    ($($name:ident($t:ty) $f:expr;)*) => {
        $(
            #[allow(non_snake_case)]
            pub extern "win64" fn $name(x: $t) -> $t {
                let f: fn($t) -> $t = $f;
                f(x)
            }
        )*
    };
}

unary! {
    acos(f64) f64::acos;
    acosf(f32) f32::acos;
    asin(f64) f64::asin;
    asinf(f32) f32::asin;
    atan(f64) f64::atan;
    atanf(f32) f32::atan;
    cbrtf(f32) f32::cbrt;
    ceilf(f32) f32::ceil;
    cos(f64) f64::cos;
    cosf(f32) f32::cos;
    exp(f64) f64::exp;
    expf(f32) f32::exp;
    exp2(f64) f64::exp2;
    exp2f(f32) f32::exp2;
    log(f64) f64::ln;
    logf(f32) f32::ln;
    log2(f64) f64::log2;
    log2f(f32) f32::log2;
    log10f(f32) f32::log10;
    sin(f64) f64::sin;
    sinf(f32) f32::sin;
    sinhf(f32) f32::sinh;
    tan(f64) f64::tan;
    tanf(f32) f32::tan;
}

#[allow(non_snake_case)]
pub extern "win64" fn erff_(x: f32) -> f32 {
    // SAFETY: a pure function of the host's libm.
    unsafe { erff(x) }
}

#[allow(non_snake_case)]
pub extern "win64" fn atan2(y: f64, x: f64) -> f64 {
    y.atan2(x)
}

#[allow(non_snake_case)]
pub extern "win64" fn atan2f(y: f32, x: f32) -> f32 {
    y.atan2(x)
}

#[allow(non_snake_case)]
pub extern "win64" fn pow(x: f64, y: f64) -> f64 {
    x.powf(y)
}

#[allow(non_snake_case)]
pub extern "win64" fn powf(x: f32, y: f32) -> f32 {
    x.powf(y)
}

#[allow(non_snake_case)]
pub extern "win64" fn fmod(x: f64, y: f64) -> f64 {
    x % y
}

#[allow(non_snake_case)]
pub extern "win64" fn fmodf(x: f32, y: f32) -> f32 {
    x % y
}

#[allow(non_snake_case)]
pub extern "win64" fn frexp_(x: f64, e: *mut i32) -> f64 {
    // SAFETY: the title's out pointer.
    unsafe { frexp(x, e) }
}

#[allow(non_snake_case)]
pub extern "win64" fn ldexp_(x: f64, e: i32) -> f64 {
    // SAFETY: a pure function.
    unsafe { ldexp(x, e) }
}

#[allow(non_snake_case)]
pub extern "win64" fn modf_(x: f64, i: *mut f64) -> f64 {
    // SAFETY: the title's out pointer.
    unsafe { modf(x, i) }
}

#[allow(non_snake_case)]
pub extern "win64" fn modff_(x: f32, i: *mut f32) -> f32 {
    // SAFETY: the title's out pointer.
    unsafe { modff(x, i) }
}

#[allow(non_snake_case)]
pub extern "win64" fn nextafterf_(x: f32, y: f32) -> f32 {
    // SAFETY: a pure function.
    unsafe { nextafterf(x, y) }
}

/// `long double` is `double` on Windows.
#[allow(non_snake_case)]
pub extern "win64" fn nexttowardf(x: f32, y: f64) -> f32 {
    // SAFETY: a pure function.
    unsafe { nextafter(x.into(), y) as f32 }
}

#[allow(non_snake_case)]
pub extern "win64" fn remainderf_(x: f32, y: f32) -> f32 {
    // SAFETY: a pure function.
    unsafe { remainderf(x, y) }
}

/// `long` is 32 bits on Windows.
#[allow(non_snake_case)]
pub extern "win64" fn lroundf_(x: f32) -> i32 {
    // SAFETY: a pure function.
    unsafe { lroundf(x) as i32 }
}

/// Windows' classes: `FP_INFINITE` 1, `FP_NAN` 2, `FP_NORMAL` -1,
/// `FP_SUBNORMAL` -2, `FP_ZERO` 0.
fn class(c: std::num::FpCategory) -> i16 {
    use std::num::FpCategory::*;
    match c {
        Infinite => 1,
        Nan => 2,
        Normal => -1,
        Subnormal => -2,
        Zero => 0,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn _dtest(x: *const f64) -> i16 {
    // SAFETY: the title's double.
    class(unsafe { *x }.classify())
}

#[allow(non_snake_case)]
pub extern "win64" fn _fdtest(x: *const f32) -> i16 {
    // SAFETY: the title's float.
    class(unsafe { *x }.classify())
}

#[allow(non_snake_case)]
pub extern "win64" fn _fdsign(x: f32) -> i32 {
    (x.to_bits() >> 16 & 0x8000) as i32
}

#[allow(non_snake_case)]
pub extern "win64" fn _isnan(x: f64) -> i32 {
    x.is_nan().into()
}

#[allow(non_snake_case)]
pub extern "win64" fn __setusermatherr(_: usize) {}

// Narrow strings: the host's, which agree.

macro_rules! host {
    ($($name:ident($($a:ident: $t:ty),*) -> $r:ty;)*) => {
        $(
            #[allow(non_snake_case)]
            pub extern "win64" fn $name($($a: $t),*) -> $r {
                // SAFETY: the title's arguments, as the function takes them.
                unsafe { libc::$name($($a),*) }
            }
        )*
    };
}

host! {
    strlen(s: *const c_char) -> usize;
    strcmp(a: *const c_char, b: *const c_char) -> c_int;
    strncmp(a: *const c_char, b: *const c_char, n: usize) -> c_int;
    strcpy(d: *mut c_char, s: *const c_char) -> *mut c_char;
    strncpy(d: *mut c_char, s: *const c_char, n: usize) -> *mut c_char;
    strcat(d: *mut c_char, s: *const c_char) -> *mut c_char;
    strncat(d: *mut c_char, s: *const c_char, n: usize) -> *mut c_char;
    strcspn(s: *const c_char, r: *const c_char) -> usize;
    strpbrk(s: *const c_char, a: *const c_char) -> *mut c_char;
    strtok(s: *mut c_char, d: *const c_char) -> *mut c_char;
    strtod(s: *const c_char, e: *mut *mut c_char) -> f64;
    strtof(s: *const c_char, e: *mut *mut c_char) -> f32;
    strtoll(s: *const c_char, e: *mut *mut c_char, b: c_int) -> i64;
    strtoull(s: *const c_char, e: *mut *mut c_char, b: c_int) -> u64;
    atof(s: *const c_char) -> f64;
    atoi(s: *const c_char) -> c_int;
    atoll(s: *const c_char) -> i64;
}

#[allow(non_snake_case)]
pub extern "win64" fn _stricmp(a: *const c_char, b: *const c_char) -> i32 {
    // SAFETY: NUL-terminated strings.
    unsafe { libc::strcasecmp(a, b) }
}

#[allow(non_snake_case)]
pub extern "win64" fn _strnicmp(a: *const c_char, b: *const c_char, n: usize) -> i32 {
    // SAFETY: NUL-terminated strings.
    unsafe { libc::strncasecmp(a, b, n) }
}

#[allow(non_snake_case)]
pub extern "win64" fn _strtoi64(s: *const c_char, e: *mut *mut c_char, b: c_int) -> i64 {
    strtoll(s, e, b)
}

#[allow(non_snake_case)]
pub extern "win64" fn _strtoui64(s: *const c_char, e: *mut *mut c_char, b: c_int) -> u64 {
    strtoull(s, e, b)
}

/// 32 bits, clamped with `ERANGE`, as Windows' `long`.
#[allow(non_snake_case)]
pub extern "win64" fn strtol(s: *const c_char, e: *mut *mut c_char, b: c_int) -> i32 {
    let v = strtoll(s, e, b);
    i32::try_from(v).unwrap_or_else(|_| {
        set_errno(ERANGE);
        if v < 0 { i32::MIN } else { i32::MAX }
    })
}

#[allow(non_snake_case)]
pub extern "win64" fn strtoul(s: *const c_char, e: *mut *mut c_char, b: c_int) -> u32 {
    let v = strtoll(s, e, b);
    if v > i64::from(u32::MAX) {
        set_errno(ERANGE);
        return u32::MAX;
    }
    v as u32 // a negative number wraps, as in C
}

#[allow(non_snake_case)]
pub extern "win64" fn atol(s: *const c_char) -> i32 {
    atoll(s) as i32
}

#[allow(non_snake_case)]
pub extern "win64" fn strtok_s(
    s: *mut c_char,
    d: *const c_char,
    ctx: *mut *mut c_char,
) -> *mut c_char {
    // SAFETY: the title's string, delimiters and context.
    unsafe { strtok_r(s, d, ctx) }
}

#[allow(non_snake_case)]
pub extern "win64" fn __strncnt(s: *const c_char, n: usize) -> usize {
    // SAFETY: up to `n` bytes of the title's string.
    unsafe { libc::strnlen(s, n) }
}

/// The `_s` copies: `EINVAL` for a null, `ERANGE` (and an empty result) when
/// it does not fit.
fn copy_s(d: *mut u8, size: usize, s: &[u8], keep: usize) -> i32 {
    if d.is_null() || size == 0 {
        return EINVAL;
    }
    if keep + s.len() >= size {
        // SAFETY: at least one byte.
        unsafe { *d = 0 };
        return ERANGE;
    }
    // SAFETY: room for `keep + s.len()` and a NUL.
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), d.add(keep), s.len());
        *d.add(keep + s.len()) = 0;
    }
    0
}

fn bytes<'a>(s: *const c_char) -> &'a [u8] {
    // SAFETY: a NUL-terminated string of the title's.
    unsafe { std::ffi::CStr::from_ptr(s) }.to_bytes()
}

#[allow(non_snake_case)]
pub extern "win64" fn strcpy_s(d: *mut c_char, size: usize, s: *const c_char) -> i32 {
    if s.is_null() {
        return EINVAL;
    }
    copy_s(d.cast(), size, bytes(s), 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn strcat_s(d: *mut c_char, size: usize, s: *const c_char) -> i32 {
    if s.is_null() || d.is_null() {
        return EINVAL;
    }
    // SAFETY: the destination holds a string within `size`.
    let have = unsafe { libc::strnlen(d, size) };
    copy_s(d.cast(), size, bytes(s), have)
}

#[allow(non_snake_case)]
pub extern "win64" fn strncpy_s(
    d: *mut c_char,
    size: usize,
    s: *const c_char,
    count: usize,
) -> i32 {
    if s.is_null() {
        return EINVAL;
    }
    // SAFETY: up to `count` bytes of the source.
    let n = unsafe { libc::strnlen(s, count.min(usize::MAX - 1)) };
    // SAFETY: as counted.
    let src = unsafe { std::slice::from_raw_parts(s.cast::<u8>(), n) };
    if count == usize::MAX && n >= size {
        // _TRUNCATE: what fits.
        copy_s(d.cast(), size, &src[..size.saturating_sub(1)], 0);
        return 80; // STRUNCATE
    }
    copy_s(d.cast(), size, src, 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn _itoa(value: i32, out: *mut c_char, radix: i32) -> *mut c_char {
    let text = match radix {
        10 => value.to_string(),
        16 => format!("{:x}", value as u32),
        8 => format!("{:o}", value as u32),
        2 => format!("{:b}", value as u32),
        _ => value.to_string(),
    };
    // SAFETY: the title's buffer, large enough by contract (33 bytes).
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), out.cast(), text.len());
        *out.add(text.len()) = 0;
    }
    out
}

// Character classes of the C locale, as Windows numbers them.

const UPPER: u16 = 0x1;
const LOWER: u16 = 0x2;
const DIGIT: u16 = 0x4;
const SPACE: u16 = 0x8;
const PUNCT: u16 = 0x10;
const CONTROL: u16 = 0x20;
const BLANK: u16 = 0x40;
const HEX: u16 = 0x80;
const ALPHA: u16 = 0x100;

fn class_of(c: u8) -> u16 {
    let mut t = 0;
    if c.is_ascii_uppercase() {
        t |= UPPER | ALPHA;
    }
    if c.is_ascii_lowercase() {
        t |= LOWER | ALPHA;
    }
    if c.is_ascii_digit() {
        t |= DIGIT;
    }
    if matches!(c, b' ' | b'\t'..=b'\r') {
        t |= SPACE;
    }
    if c.is_ascii_punctuation() {
        t |= PUNCT;
    }
    if c.is_ascii_control() {
        t |= CONTROL;
    }
    if c == b' ' {
        t |= BLANK;
    }
    if c.is_ascii_hexdigit() {
        t |= HEX;
    }
    t
}

/// The table `__pctype_func` gives: entry -1 (EOF) first, then 0 to 255.
fn table() -> &'static [u16; 257] {
    static TABLE: std::sync::OnceLock<[u16; 257]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| if i == 0 { 0 } else { class_of((i - 1) as u8) }))
}

#[allow(non_snake_case)]
pub extern "win64" fn __pctype_func() -> *const u16 {
    // Index 0 of the result is character 0.
    table()[1..].as_ptr()
}

fn is(c: i32, mask: u16) -> i32 {
    match c {
        0..=255 => i32::from(table()[c as usize + 1] & mask),
        _ => 0,
    }
}

macro_rules! ctype {
    ($($name:ident $mask:expr;)*) => {
        $(
            #[allow(non_snake_case)]
            pub extern "win64" fn $name(c: i32) -> i32 {
                is(c, $mask)
            }
        )*
    };
}

ctype! {
    isalpha ALPHA;
    isdigit DIGIT;
    isalnum ALPHA | DIGIT;
    islower LOWER;
    isupper UPPER;
    isspace SPACE;
    isxdigit HEX;
}

#[allow(non_snake_case)]
pub extern "win64" fn tolower(c: i32) -> i32 {
    if (b'A' as i32..=b'Z' as i32).contains(&c) {
        c + 32
    } else {
        c
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn toupper(c: i32) -> i32 {
    if (b'a' as i32..=b'z' as i32).contains(&c) {
        c - 32
    } else {
        c
    }
}

fn wide_char(c: u16) -> Option<char> {
    char::from_u32(c.into())
}

#[allow(non_snake_case)]
pub extern "win64" fn iswalnum(c: u16) -> i32 {
    wide_char(c).is_some_and(char::is_alphanumeric).into()
}

#[allow(non_snake_case)]
pub extern "win64" fn iswdigit(c: u16) -> i32 {
    wide_char(c).is_some_and(|c| c.is_ascii_digit()).into()
}

#[allow(non_snake_case)]
pub extern "win64" fn iswspace(c: u16) -> i32 {
    wide_char(c).is_some_and(char::is_whitespace).into()
}

#[allow(non_snake_case)]
pub extern "win64" fn iswxdigit(c: u16) -> i32 {
    wide_char(c).is_some_and(|c| c.is_ascii_hexdigit()).into()
}

/// A byte as a wide character, or `WEOF`, in the C locale.
#[allow(non_snake_case)]
pub extern "win64" fn btowc(c: i32) -> u32 {
    if (0..0x80).contains(&c) {
        c as u32
    } else {
        0xFFFF
    }
}

// UTF-16 strings.

/// # Safety
/// `s` is NUL-terminated UTF-16, read up to `max` units.
unsafe fn units<'a>(s: *const u16, max: usize) -> &'a [u16] {
    // SAFETY: the caller's guarantee.
    let n = (0..max).take_while(|&i| unsafe { *s.add(i) } != 0).count();
    // SAFETY: as counted.
    unsafe { std::slice::from_raw_parts(s, n) }
}

#[allow(non_snake_case)]
pub extern "win64" fn wcslen(s: *const u16) -> usize {
    // SAFETY: a NUL-terminated string.
    unsafe { units(s, usize::MAX) }.len()
}

#[allow(non_snake_case)]
pub extern "win64" fn wcsnlen(s: *const u16, n: usize) -> usize {
    // SAFETY: up to `n` units.
    unsafe { units(s, n) }.len()
}

#[allow(non_snake_case)]
pub extern "win64" fn wcscmp(a: *const u16, b: *const u16) -> i32 {
    // SAFETY: NUL-terminated strings.
    unsafe { units(a, usize::MAX).cmp(units(b, usize::MAX)) as i32 }
}

#[allow(non_snake_case)]
pub extern "win64" fn wcscoll(a: *const u16, b: *const u16) -> i32 {
    wcscmp(a, b)
}

fn fold(u: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&u) {
        u + 32
    } else {
        u
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn _wcsnicmp(a: *const u16, b: *const u16, n: usize) -> i32 {
    // SAFETY: NUL-terminated strings, up to `n` units.
    let (a, b) = unsafe { (units(a, n), units(b, n)) };
    a.iter().map(|&u| fold(u)).cmp(b.iter().map(|&u| fold(u))) as i32
}

#[allow(non_snake_case)]
pub extern "win64" fn _wcsicoll(a: *const u16, b: *const u16) -> i32 {
    _wcsnicmp(a, b, usize::MAX)
}

#[allow(non_snake_case)]
pub extern "win64" fn wcsncpy(d: *mut u16, s: *const u16, n: usize) -> *mut u16 {
    // SAFETY: up to `n` units of the source; `n` units of room at `d`.
    unsafe {
        let src = units(s, n);
        std::ptr::copy_nonoverlapping(src.as_ptr(), d, src.len());
        std::ptr::write_bytes(d.add(src.len()), 0, n - src.len());
    }
    d
}

fn wide_copy_s(d: *mut u16, size: usize, s: &[u16], keep: usize) -> i32 {
    if d.is_null() || size == 0 {
        return EINVAL;
    }
    if keep + s.len() >= size {
        // SAFETY: at least one unit.
        unsafe { *d = 0 };
        return ERANGE;
    }
    // SAFETY: room for `keep + s.len()` and a NUL.
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), d.add(keep), s.len());
        *d.add(keep + s.len()) = 0;
    }
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn wcscpy_s(d: *mut u16, size: usize, s: *const u16) -> i32 {
    if s.is_null() {
        return EINVAL;
    }
    // SAFETY: a NUL-terminated source.
    wide_copy_s(d, size, unsafe { units(s, usize::MAX) }, 0)
}

#[allow(non_snake_case)]
pub extern "win64" fn wcscat_s(d: *mut u16, size: usize, s: *const u16) -> i32 {
    if s.is_null() || d.is_null() {
        return EINVAL;
    }
    // SAFETY: the destination holds a string within `size`; the source ends.
    let (have, src) = unsafe { (units(d, size).len(), units(s, usize::MAX)) };
    wide_copy_s(d, size, src, have)
}

#[allow(non_snake_case)]
pub extern "win64" fn _wcsdup(s: *const u16) -> *mut u16 {
    // SAFETY: a NUL-terminated string.
    let src = unsafe { units(s, usize::MAX) };
    let out = super::crt::malloc((src.len() + 1) * 2).cast::<u16>();
    if !out.is_null() {
        // SAFETY: room for the string and its NUL.
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), out, src.len());
            *out.add(src.len()) = 0;
        }
    }
    out
}

#[allow(non_snake_case)]
pub extern "win64" fn wcstok_s(
    s: *mut u16,
    delimiters: *const u16,
    ctx: *mut *mut u16,
) -> *mut u16 {
    // SAFETY: the title's string, delimiters and context.
    unsafe {
        let delims = units(delimiters, usize::MAX);
        let mut at = if s.is_null() { *ctx } else { s };
        if at.is_null() {
            return std::ptr::null_mut();
        }
        while *at != 0 && delims.contains(&*at) {
            at = at.add(1);
        }
        if *at == 0 {
            *ctx = at;
            return std::ptr::null_mut();
        }
        let start = at;
        while *at != 0 && !delims.contains(&*at) {
            at = at.add(1);
        }
        if *at != 0 {
            *at = 0;
            at = at.add(1);
        }
        *ctx = at;
        start
    }
}

/// UTF-16 to the narrow code page (UTF-8 here).
#[allow(non_snake_case)]
pub extern "win64" fn wcstombs(out: *mut c_char, s: *const u16, n: usize) -> usize {
    // SAFETY: a NUL-terminated source.
    let text = String::from_utf16_lossy(unsafe { units(s, usize::MAX) });
    if out.is_null() {
        return text.len();
    }
    let k = text.len().min(n);
    // SAFETY: the title's buffer of `n` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), out.cast(), k);
        if k < n {
            *out.add(k) = 0;
        }
    }
    k
}

#[allow(non_snake_case)]
pub extern "win64" fn wcstoull(s: *const u16, end: *mut *mut u16, base: c_int) -> u64 {
    // SAFETY: a NUL-terminated string.
    let text = String::from_utf16_lossy(unsafe { units(s, usize::MAX) });
    let c = std::ffi::CString::new(text).unwrap_or_default();
    let mut e: *mut c_char = std::ptr::null_mut();
    let v = strtoull(c.as_ptr(), &mut e, base);
    if !end.is_null() {
        // SAFETY: `e` points into `c`, as far in as ASCII digits go.
        unsafe { *end = s.add(e.offset_from(c.as_ptr()) as usize).cast_mut() };
    }
    v
}

#[allow(non_snake_case)]
pub extern "win64" fn _wtoi(s: *const u16) -> i32 {
    wcstoull(s, std::ptr::null_mut(), 10) as i32
}

// Sorting with the title's comparators (Windows x64 convention).

type Compare = unsafe extern "win64" fn(*const c_void, *const c_void) -> c_int;

#[allow(non_snake_case)]
pub extern "win64" fn qsort(base: *mut c_void, n: usize, size: usize, compare: Compare) {
    if n < 2 || size == 0 {
        return;
    }
    // SAFETY: the title's array of `n` elements of `size` bytes.
    let all = unsafe { std::slice::from_raw_parts_mut(base.cast::<u8>(), n * size) };
    let mut items: Vec<Vec<u8>> = all.chunks_exact(size).map(<[u8]>::to_vec).collect();
    // SAFETY: the title's comparator, given pointers to two elements.
    items.sort_by(|a, b| unsafe { compare(a.as_ptr().cast(), b.as_ptr().cast()) }.cmp(&0));
    for (slot, item) in all.chunks_exact_mut(size).zip(items) {
        slot.copy_from_slice(&item);
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn bsearch(
    key: *const c_void,
    base: *const c_void,
    n: usize,
    size: usize,
    compare: Compare,
) -> *mut c_void {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        // SAFETY: inside the title's array.
        let at = unsafe { base.cast::<u8>().add(mid * size) }.cast::<c_void>();
        // SAFETY: the title's comparator.
        match unsafe { compare(key, at) } {
            0 => return at.cast_mut(),
            c if c < 0 => hi = mid,
            _ => lo = mid + 1,
        }
    }
    std::ptr::null_mut()
}

thread_local! {
    static SEED: Cell<u32> = const { Cell::new(1) };
}

/// Microsoft's generator, per thread, as titles that seed it expect.
#[allow(non_snake_case)]
pub extern "win64" fn rand() -> i32 {
    SEED.with(|s| {
        let next = s.get().wrapping_mul(214_013).wrapping_add(2_531_011);
        s.set(next);
        (next >> 16 & 0x7FFF) as i32
    })
}

#[allow(non_snake_case)]
pub extern "win64" fn srand(seed: u32) {
    SEED.with(|s| s.set(seed));
}

#[allow(non_snake_case)]
pub extern "win64" fn rand_s(out: *mut u32) -> i32 {
    // SAFETY: the title's out pointer.
    unsafe { *out = libc::arc4random() };
    0
}

// Time: `struct tm` is nine ints on both.

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Tm([c_int; 9]);

fn to_host(t: &Tm) -> libc::tm {
    let [sec, min, hour, mday, mon, year, wday, yday, isdst] = t.0;
    libc::tm {
        tm_sec: sec,
        tm_min: min,
        tm_hour: hour,
        tm_mday: mday,
        tm_mon: mon,
        tm_year: year,
        tm_wday: wday,
        tm_yday: yday,
        tm_isdst: isdst,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null_mut(),
    }
}

fn from_host(t: &libc::tm) -> Tm {
    Tm([
        t.tm_sec, t.tm_min, t.tm_hour, t.tm_mday, t.tm_mon, t.tm_year, t.tm_wday, t.tm_yday,
        t.tm_isdst,
    ])
}

#[allow(non_snake_case)]
pub extern "win64" fn _time64(out: *mut i64) -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if !out.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *out = t };
    }
    t
}

#[allow(non_snake_case)]
pub extern "win64" fn _difftime64(a: i64, b: i64) -> f64 {
    (a - b) as f64
}

fn broken_down(t: i64, local: bool) -> Option<Tm> {
    // SAFETY: plain data filled in by the call.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = t as libc::time_t;
    // SAFETY: valid pointers to our own values.
    let ok = unsafe {
        if local {
            !libc::localtime_r(&t, &mut tm).is_null()
        } else {
            !libc::gmtime_r(&t, &mut tm).is_null()
        }
    };
    ok.then(|| from_host(&tm))
}

#[allow(non_snake_case)]
pub extern "win64" fn _gmtime64(t: *const i64) -> *mut Tm {
    thread_local! {
        static TM: Cell<Tm> = Cell::new(Tm::default());
    }
    // SAFETY: the title's time.
    match broken_down(unsafe { *t }, false) {
        Some(tm) => TM.with(|c| {
            c.set(tm);
            c.as_ptr()
        }),
        None => std::ptr::null_mut(),
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn _gmtime64_s(out: *mut Tm, t: *const i64) -> i32 {
    // SAFETY: the title's pointers.
    match broken_down(unsafe { *t }, false) {
        Some(tm) => {
            unsafe { *out = tm };
            0
        }
        None => EINVAL,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn _localtime64_s(out: *mut Tm, t: *const i64) -> i32 {
    // SAFETY: the title's pointers.
    match broken_down(unsafe { *t }, true) {
        Some(tm) => {
            unsafe { *out = tm };
            0
        }
        None => EINVAL,
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn _mktime64(t: *mut Tm) -> i64 {
    // SAFETY: the title's struct tm, normalised in place as mktime does.
    unsafe {
        let mut tm = to_host(&*t);
        let r = libc::mktime(&mut tm);
        *t = from_host(&tm);
        r
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn _mkgmtime64(t: *mut Tm) -> i64 {
    // SAFETY: as above.
    unsafe {
        let mut tm = to_host(&*t);
        let r = timegm(&mut tm);
        *t = from_host(&tm);
        r
    }
}

/// `__timeb64`: seconds, milliseconds, time zone minutes, daylight flag.
#[allow(non_snake_case)]
pub extern "win64" fn _ftime64(out: *mut u8) {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // SAFETY: the title's __timeb64 (16 bytes).
    unsafe {
        out.cast::<i64>().write_unaligned(d.as_secs() as i64);
        out.add(8)
            .cast::<u16>()
            .write_unaligned(d.subsec_millis() as u16);
        out.add(10).cast::<i16>().write_unaligned(0);
        out.add(12).cast::<i16>().write_unaligned(0);
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn strftime(
    out: *mut c_char,
    max: usize,
    fmt: *const c_char,
    t: *const Tm,
) -> usize {
    // SAFETY: the title's buffer, format and struct tm.
    unsafe { libc::strftime(out, max, fmt, &to_host(&*t)) }
}

#[allow(non_snake_case)]
pub extern "win64" fn _Strftime(
    out: *mut c_char,
    max: usize,
    fmt: *const c_char,
    t: *const Tm,
    _: *const c_void,
) -> usize {
    strftime(out, max, fmt, t)
}

#[allow(non_snake_case)]
pub extern "win64" fn _Wcsftime(
    out: *mut u16,
    max: usize,
    fmt: *const u16,
    t: *const Tm,
    _: *const c_void,
) -> usize {
    // SAFETY: a NUL-terminated format.
    let f = String::from_utf16_lossy(unsafe { units(fmt, usize::MAX) });
    let f = std::ffi::CString::new(f).unwrap_or_default();
    let mut buf = vec![0 as c_char; max.max(1) * 4];
    let n = strftime(buf.as_mut_ptr(), buf.len(), f.as_ptr(), t);
    // SAFETY: `n` bytes written by strftime.
    let text = String::from_utf8_lossy(unsafe {
        std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), n)
    })
    .into_owned();
    let w: Vec<u16> = text.encode_utf16().collect();
    if w.len() >= max {
        return 0;
    }
    // SAFETY: the title's buffer of `max` units.
    unsafe {
        std::ptr::copy_nonoverlapping(w.as_ptr(), out, w.len());
        *out.add(w.len()) = 0;
    }
    w.len()
}

const DAYS: [(&str, &str); 7] = [
    ("Sun", "Sunday"),
    ("Mon", "Monday"),
    ("Tue", "Tuesday"),
    ("Wed", "Wednesday"),
    ("Thu", "Thursday"),
    ("Fri", "Friday"),
    ("Sat", "Saturday"),
];
const MONTHS: [(&str, &str); 12] = [
    ("Jan", "January"),
    ("Feb", "February"),
    ("Mar", "March"),
    ("Apr", "April"),
    ("May", "May"),
    ("Jun", "June"),
    ("Jul", "July"),
    ("Aug", "August"),
    ("Sep", "September"),
    ("Oct", "October"),
    ("Nov", "November"),
    ("Dec", "December"),
];

/// The C++ library's name lists: `:Sun:Sunday:Mon:Monday...`, in a block
/// it frees with `free`.
fn names(list: &[(&str, &str)], wide: bool) -> *mut c_void {
    let text: String = list.iter().map(|(a, f)| format!(":{a}:{f}")).collect();
    if wide {
        let w: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let out = super::crt::malloc(w.len() * 2).cast::<u16>();
        // SAFETY: room for the list and its NUL.
        unsafe { std::ptr::copy_nonoverlapping(w.as_ptr(), out, w.len()) };
        out.cast()
    } else {
        let out = super::crt::malloc(text.len() + 1).cast::<u8>();
        // SAFETY: room for the list and its NUL.
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), out, text.len());
            *out.add(text.len()) = 0;
        }
        out.cast()
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn _Getdays() -> *mut c_void {
    names(&DAYS, false)
}

#[allow(non_snake_case)]
pub extern "win64" fn _Getmonths() -> *mut c_void {
    names(&MONTHS, false)
}

#[allow(non_snake_case)]
pub extern "win64" fn _W_Getdays() -> *mut c_void {
    names(&DAYS, true)
}

#[allow(non_snake_case)]
pub extern "win64" fn _W_Getmonths() -> *mut c_void {
    names(&MONTHS, true)
}

/// `__crt_lc_time_data` of the C locale: narrow names and formats, the
/// calendar and a reference count, then the same names in UTF-16 and the
/// locale's name. Made once; the C++ library frees its copy with `free`, so
/// each call gives a fresh block.
fn time_names(_wide: bool) -> *mut c_void {
    fn narrow(s: &str) -> usize {
        Box::leak(
            std::ffi::CString::new(s)
                .unwrap_or_default()
                .into_boxed_c_str(),
        )
        .as_ptr() as usize
    }
    fn wide(s: &str) -> usize {
        Box::leak(
            s.encode_utf16()
                .chain([0])
                .collect::<Vec<u16>>()
                .into_boxed_slice(),
        )
        .as_ptr() as usize
    }
    let mut words: Vec<usize> = Vec::new();
    let each = |f: fn(&str) -> usize, words: &mut Vec<usize>| {
        words.extend(DAYS.iter().map(|d| f(d.0)));
        words.extend(DAYS.iter().map(|d| f(d.1)));
        words.extend(MONTHS.iter().map(|m| f(m.0)));
        words.extend(MONTHS.iter().map(|m| f(m.1)));
        words.extend([
            f("AM"),
            f("PM"),
            f("MM/dd/yy"),
            f("dddd, MMMM dd, yyyy"),
            f("HH:mm:ss"),
        ]);
    };
    each(narrow, &mut words);
    // ww_caltype (int) and refcount (long), one word together.
    words.push(1);
    each(wide, &mut words);
    words.push(wide("en-US"));
    let out = super::crt::malloc(words.len() * 8).cast::<usize>();
    // SAFETY: room for every word.
    unsafe { std::ptr::copy_nonoverlapping(words.as_ptr(), out, words.len()) };
    out.cast()
}

#[allow(non_snake_case)]
pub extern "win64" fn _Gettnames() -> *mut c_void {
    time_names(false)
}

#[allow(non_snake_case)]
pub extern "win64" fn _W_Gettnames() -> *mut c_void {
    time_names(true)
}

// The C locale.

#[allow(non_snake_case)]
pub extern "win64" fn setlocale(_: i32, _: *const c_char) -> *const c_char {
    c"C".as_ptr()
}

/// `lconv` of the C locale: "." and empty strings, `CHAR_MAX` for the
/// numbers, then the UTF-16 strings.
#[allow(non_snake_case)]
pub extern "win64" fn localeconv() -> *const c_void {
    static LCONV: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LCONV.get_or_init(|| {
        let dot = c".".as_ptr() as usize;
        let empty = c"".as_ptr() as usize;
        static WDOT: [u16; 2] = [b'.' as u16, 0];
        static WEMPTY: [u16; 1] = [0];
        let mut block: Vec<u8> = Vec::new();
        for p in [
            dot, empty, empty, empty, empty, empty, empty, empty, empty, empty,
        ] {
            block.extend(p.to_le_bytes());
        }
        block.extend([127u8; 8]);
        let (wdot, wempty) = (WDOT.as_ptr() as usize, WEMPTY.as_ptr() as usize);
        for p in [wdot, wempty, wempty, wempty, wempty, wempty, wempty, wempty] {
            block.extend(p.to_le_bytes());
        }
        Box::leak(block.into_boxed_slice()).as_ptr() as usize
    }) as *const c_void
}

#[allow(non_snake_case)]
pub extern "win64" fn ___mb_cur_max_func() -> i32 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn ___lc_codepage_func() -> u32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn ___lc_collate_cp_func() -> u32 {
    0
}

#[allow(non_snake_case)]
pub extern "win64" fn ___lc_locale_name_func() -> *const usize {
    static NAMES: [usize; 6] = [0; 6]; // all null: the C locale
    NAMES.as_ptr()
}

#[allow(non_snake_case)]
pub extern "win64" fn _configthreadlocale(_: i32) -> i32 {
    2 // _DISABLE_PER_THREAD_LOCALE: one locale for every thread
}

// shortcut: the locale never changes and stream calls are not locked
// against each other; these locks are accepted and do nothing.
#[allow(non_snake_case)]
pub extern "win64" fn _lock_locales() {}

#[allow(non_snake_case)]
pub extern "win64" fn _unlock_locales() {}

#[allow(non_snake_case)]
pub extern "win64" fn _lock_file(_: *mut c_void) {}

#[allow(non_snake_case)]
pub extern "win64" fn _unlock_file(_: *mut c_void) {}

// The file system calls of the runtime: title paths, through our files.

fn ok_or_errno(ok: i32) -> i32 {
    if ok != 0 {
        0
    } else {
        set_errno(2); // ENOENT
        -1
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn _wchdir(path: *const u16) -> i32 {
    ok_or_errno(super::files::SetCurrentDirectoryW(path))
}

#[allow(non_snake_case)]
pub extern "win64" fn _wgetcwd(out: *mut u16, size: i32) -> *mut u16 {
    let need = super::files::GetCurrentDirectoryW(0, std::ptr::null_mut()) as usize;
    let out = if out.is_null() {
        super::crt::malloc(need.max(size as usize) * 2).cast::<u16>()
    } else {
        out
    };
    if (size as usize) < need && size != 0 {
        set_errno(ERANGE);
        return std::ptr::null_mut();
    }
    super::files::GetCurrentDirectoryW(need as u32, out);
    out
}

#[allow(non_snake_case)]
pub extern "win64" fn _wremove(path: *const u16) -> i32 {
    ok_or_errno(super::files::DeleteFileW(path))
}

#[allow(non_snake_case)]
pub extern "win64" fn remove(path: *const c_char) -> i32 {
    ok_or_errno(super::files::DeleteFileA(path.cast()))
}

#[allow(non_snake_case)]
pub extern "win64" fn _wrename(from: *const u16, to: *const u16) -> i32 {
    ok_or_errno(super::files::MoveFileExW(from, to, 0))
}

#[allow(non_snake_case)]
pub extern "win64" fn _wrmdir(path: *const u16) -> i32 {
    ok_or_errno(super::files::RemoveDirectoryW(path))
}

/// `struct _stat64` (56 bytes), from the file's attributes.
#[allow(non_snake_case)]
pub extern "win64" fn _wstat64(path: *const u16, out: *mut u8) -> i32 {
    let mut data = [0u32; 9];
    if super::files::GetFileAttributesExW(path, 0, data.as_mut_ptr().cast()) == 0 {
        set_errno(2);
        return -1;
    }
    let directory = data[0] & 0x10 != 0;
    let readonly = data[0] & 0x1 != 0;
    let mode: u16 =
        if directory { 0x4000 | 0x40 } else { 0x8000 } | 0x100 | if readonly { 0 } else { 0x80 };
    let size = u64::from(data[7]) << 32 | u64::from(data[8]);
    let secs = |lo: u32, hi: u32| {
        ((u64::from(hi) << 32 | u64::from(lo)).saturating_sub(116_444_736_000_000_000) / 10_000_000)
            as i64
    };
    // SAFETY: the title's struct _stat64.
    unsafe {
        out.write_bytes(0, 56);
        out.add(6).cast::<u16>().write_unaligned(mode);
        out.add(8).cast::<i16>().write_unaligned(1);
        out.add(24).cast::<i64>().write_unaligned(size as i64);
        out.add(32)
            .cast::<i64>()
            .write_unaligned(secs(data[3], data[4]));
        out.add(40)
            .cast::<i64>()
            .write_unaligned(secs(data[5], data[6]));
        out.add(48)
            .cast::<i64>()
            .write_unaligned(secs(data[1], data[2]));
    }
    0
}

pub fn exports() -> Vec<HleExport> {
    let mut e = vec![
        hle_export!(acos),
        hle_export!(acosf),
        hle_export!(asin),
        hle_export!(asinf),
        hle_export!(atan),
        hle_export!(atanf),
        hle_export!(atan2),
        hle_export!(atan2f),
        hle_export!(cbrtf),
        hle_export!(ceilf),
        hle_export!(cos),
        hle_export!(cosf),
        hle_export!(exp),
        hle_export!(expf),
        hle_export!(exp2),
        hle_export!(exp2f),
        hle_export!(log),
        hle_export!(logf),
        hle_export!(log2),
        hle_export!(log2f),
        hle_export!(log10f),
        hle_export!(sin),
        hle_export!(sinf),
        hle_export!(sinhf),
        hle_export!(tan),
        hle_export!(tanf),
        hle_export!(pow),
        hle_export!(powf),
        hle_export!(fmod),
        hle_export!(fmodf),
        hle_export!(nexttowardf),
        hle_export!(_dtest),
        hle_export!(_fdtest),
        hle_export!(_fdsign),
        hle_export!(_isnan),
        hle_export!(__setusermatherr),
        hle_export!(strlen),
        hle_export!(strcmp),
        hle_export!(strncmp),
        hle_export!(strcpy),
        hle_export!(strncpy),
        hle_export!(strcat),
        hle_export!(strncat),
        hle_export!(strcspn),
        hle_export!(strpbrk),
        hle_export!(strtok),
        hle_export!(strtod),
        hle_export!(strtof),
        hle_export!(strtoll),
        hle_export!(strtoull),
        hle_export!(atof),
        hle_export!(atoi),
        hle_export!(atoll),
        hle_export!(atol),
        hle_export!(_stricmp),
        hle_export!(_strnicmp),
        hle_export!(_strtoi64),
        hle_export!(_strtoui64),
        hle_export!(strtol),
        hle_export!(strtoul),
        hle_export!(strtok_s),
        hle_export!(__strncnt),
        hle_export!(strcpy_s),
        hle_export!(strcat_s),
        hle_export!(strncpy_s),
        hle_export!(_itoa),
        hle_export!(__pctype_func),
        hle_export!(isalpha),
        hle_export!(isdigit),
        hle_export!(isalnum),
        hle_export!(islower),
        hle_export!(isupper),
        hle_export!(isspace),
        hle_export!(isxdigit),
        hle_export!(tolower),
        hle_export!(toupper),
        hle_export!(iswalnum),
        hle_export!(iswdigit),
        hle_export!(iswspace),
        hle_export!(iswxdigit),
        hle_export!(btowc),
        hle_export!(wcslen),
        hle_export!(wcsnlen),
        hle_export!(wcscmp),
        hle_export!(wcscoll),
        hle_export!(_wcsnicmp),
        hle_export!(_wcsicoll),
        hle_export!(wcsncpy),
        hle_export!(wcscpy_s),
        hle_export!(wcscat_s),
        hle_export!(_wcsdup),
        hle_export!(wcstok_s),
        hle_export!(wcstombs),
        hle_export!(wcstoull),
        hle_export!(_wtoi),
        hle_export!(qsort),
        hle_export!(bsearch),
        hle_export!(rand),
        hle_export!(srand),
        hle_export!(rand_s),
        hle_export!(_time64),
        hle_export!(_difftime64),
        hle_export!(_gmtime64),
        hle_export!(_gmtime64_s),
        hle_export!(_localtime64_s),
        hle_export!(_mktime64),
        hle_export!(_mkgmtime64),
        hle_export!(_ftime64),
        hle_export!(strftime),
        hle_export!(_Strftime),
        hle_export!(_Wcsftime),
        hle_export!(_Getdays),
        hle_export!(_Getmonths),
        hle_export!(_W_Getdays),
        hle_export!(_W_Getmonths),
        hle_export!(_Gettnames),
        hle_export!(_W_Gettnames),
        hle_export!(setlocale),
        hle_export!(localeconv),
        hle_export!(___mb_cur_max_func),
        hle_export!(___lc_codepage_func),
        hle_export!(___lc_collate_cp_func),
        hle_export!(___lc_locale_name_func),
        hle_export!(_configthreadlocale),
        hle_export!(_lock_locales),
        hle_export!(_unlock_locales),
        hle_export!(_lock_file),
        hle_export!(_unlock_file),
        hle_export!(_wchdir),
        hle_export!(_wgetcwd),
        hle_export!(_wremove),
        hle_export!(remove),
        hle_export!(_wrename),
        hle_export!(_wrmdir),
        hle_export!(_wstat64),
    ];
    // Names taken by the host libc's own symbols in Rust are given here.
    for (name, f) in [
        ("erff", erff_ as *const () as usize),
        ("frexp", frexp_ as *const () as usize),
        ("ldexp", ldexp_ as *const () as usize),
        ("modf", modf_ as *const () as usize),
        ("modff", modff_ as *const () as usize),
        ("nextafterf", nextafterf_ as *const () as usize),
        ("remainderf", remainderf_ as *const () as usize),
        ("lroundf", lroundf_ as *const () as usize),
    ] {
        e.push(HleExport { name, address: f });
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_ways_where_they_differ() {
        assert_eq!(
            strtol(c"99999999999".as_ptr(), std::ptr::null_mut(), 10),
            i32::MAX,
            "long is 32 bits"
        );
        srand(1);
        assert_eq!(
            [rand(), rand(), rand()],
            [41, 18467, 6334],
            "Microsoft's sequence"
        );
        assert_eq!(isalpha(b'a'.into()), i32::from(ALPHA));
        // SAFETY: index -1 (EOF) is inside the table.
        assert_eq!(unsafe { *__pctype_func().sub(1) }, 0);
        let one = 1.0f64;
        assert_eq!(_dtest(&one), -1);
    }

    #[test]
    fn qsort_calls_the_title_s_comparator() {
        unsafe extern "win64" fn by_value(a: *const c_void, b: *const c_void) -> c_int {
            // SAFETY: pointers to the test's i32s.
            unsafe { (*a.cast::<i32>()).cmp(&*b.cast::<i32>()) as c_int }
        }
        let mut v = [5i32, -2, 9, 0, 3];
        qsort(v.as_mut_ptr().cast(), 5, 4, by_value);
        assert_eq!(v, [-2, 0, 3, 5, 9]);
        let key = 5i32;
        let found = bsearch((&raw const key).cast(), v.as_ptr().cast(), 5, 4, by_value);
        assert_eq!(found, (&raw mut v[3]).cast());
    }

    #[test]
    fn wide_strings_and_copies() {
        let w: Vec<u16> = "a,b,,c".encode_utf16().chain([0]).collect();
        let mut s = w.clone();
        let comma: Vec<u16> = ",".encode_utf16().chain([0]).collect();
        let mut ctx = std::ptr::null_mut();
        let mut parts = Vec::new();
        let mut t = wcstok_s(s.as_mut_ptr(), comma.as_ptr(), &mut ctx);
        while !t.is_null() {
            parts.push(wcslen(t));
            t = wcstok_s(std::ptr::null_mut(), comma.as_ptr(), &mut ctx);
        }
        assert_eq!(parts, [1, 1, 1]);
        let mut small = [0i8; 3];
        assert_eq!(strcpy_s(small.as_mut_ptr(), 3, c"abc".as_ptr()), ERANGE);
        assert_eq!(small[0], 0);
    }
}
