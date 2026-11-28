//! `printf` and `scanf` as the Microsoft C runtime reads them: Windows x64
//! `va_list`s (a pointer to 8-byte slots, floats as doubles), `long` of 32
//! bits, `%I64d` and `%I32d`, `%ls`/`%S` for UTF-16 text, `%hs`, `%zu`,
//! three-digit-free exponents (`1.000000e+00`, as since Visual Studio 2015).

/// The arguments of one call, one 8-byte slot each.
pub struct Args {
    at: *const u64,
}

impl Args {
    /// # Safety
    /// `va_list` is a Windows x64 argument list holding what the format asks.
    pub unsafe fn new(va_list: *const u64) -> Self {
        Args { at: va_list }
    }

    fn next(&mut self) -> u64 {
        // SAFETY: the caller of `new` vouches for as many slots as the format reads.
        unsafe {
            let v = self.at.read_unaligned();
            self.at = self.at.add(1);
            v
        }
    }
}

/// Text read through a pointer an argument gives: narrow, or UTF-16.
///
/// # Safety
/// `p` is null or NUL-terminated, read up to `limit` units.
unsafe fn text_at(p: u64, wide: bool, limit: Option<usize>) -> String {
    if p == 0 {
        return "(null)".into();
    }
    let max = limit.unwrap_or(usize::MAX);
    if wide {
        let p = p as *const u16;
        // SAFETY: up to the terminator or the precision.
        let n = (0..max).take_while(|&i| unsafe { *p.add(i) } != 0).count();
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
    } else {
        let p = p as *const u8;
        // SAFETY: as above.
        let n = (0..max).take_while(|&i| unsafe { *p.add(i) } != 0).count();
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(p, n) }).into_owned()
    }
}

fn exponent(v: f64, precision: usize, upper: bool) -> String {
    if v == 0.0 {
        let mantissa = format!("{:.precision$}", 0.0);
        return format!("{mantissa}{}+00", if upper { 'E' } else { 'e' });
    }
    let mut e = v.abs().log10().floor() as i32;
    let mut m = v / 10f64.powi(e);
    // Rounding can carry to 10.
    let rounded: f64 = format!("{:.precision$}", m).parse().unwrap_or(m);
    if rounded.abs() >= 10.0 {
        e += 1;
        m = v / 10f64.powi(e);
    }
    let sign = if e < 0 { '-' } else { '+' };
    format!(
        "{m:.precision$}{}{sign}{:02}",
        if upper { 'E' } else { 'e' },
        e.abs()
    )
}

fn general(v: f64, precision: usize, upper: bool, alternate: bool) -> String {
    let p = precision.max(1);
    if v == 0.0 {
        return if alternate {
            format!("{:.prec$}", 0.0, prec = p - 1)
        } else {
            "0".into()
        };
    }
    let e = v.abs().log10().floor() as i32;
    let text = if e < -4 || e >= p as i32 {
        exponent(v, p - 1, upper)
    } else {
        format!("{v:.prec$}", prec = (p as i32 - 1 - e).max(0) as usize)
    };
    if alternate {
        return text;
    }
    // %g drops trailing zeros (and a lone point) from the fraction.
    let (body, tail) = match text.find(['e', 'E']) {
        Some(i) => text.split_at(i),
        None => (text.as_str(), ""),
    };
    let body = if body.contains('.') {
        body.trim_end_matches('0').trim_end_matches('.')
    } else {
        body
    };
    format!("{body}{tail}")
}

/// Formats `format` with `args`: the text, as Unicode (callers encode it).
///
/// # Safety
/// `format` is NUL-terminated (UTF-16 when `wide`), and `args` holds what
/// it asks for.
pub unsafe fn format(format: *const u8, wide: bool, args: &mut Args) -> String {
    // SAFETY: the caller's format string.
    let f: Vec<char> = unsafe { text_at(format as u64, wide, None) }
        .chars()
        .collect();
    let mut out = String::new();
    let mut i = 0;
    while i < f.len() {
        if f[i] != '%' {
            out.push(f[i]);
            i += 1;
            continue;
        }
        i += 1;
        let (mut left, mut plus, mut space, mut alt, mut zero) =
            (false, false, false, false, false);
        while let Some(&c) = f.get(i) {
            match c {
                '-' => left = true,
                '+' => plus = true,
                ' ' => space = true,
                '#' => alt = true,
                '0' => zero = true,
                _ => break,
            }
            i += 1;
        }
        let mut width = 0usize;
        if f.get(i) == Some(&'*') {
            let w = args.next() as i32;
            if w < 0 {
                left = true;
            }
            width = w.unsigned_abs() as usize;
            i += 1;
        } else {
            while let Some(d) = f.get(i).and_then(|c| c.to_digit(10)) {
                width = width * 10 + d as usize;
                i += 1;
            }
        }
        let mut precision = None;
        if f.get(i) == Some(&'.') {
            i += 1;
            if f.get(i) == Some(&'*') {
                let p = args.next() as i32;
                precision = (p >= 0).then_some(p as usize);
                i += 1;
            } else {
                let mut p = 0;
                while let Some(d) = f.get(i).and_then(|c| c.to_digit(10)) {
                    p = p * 10 + d as usize;
                    i += 1;
                }
                precision = Some(p);
            }
        }
        // Length: bits of an integer, or narrow/wide text.
        let mut bits = 32;
        let mut text_wide = wide;
        loop {
            match (f.get(i), f.get(i + 1), f.get(i + 2)) {
                (Some('I'), Some('6'), Some('4')) => {
                    bits = 64;
                    i += 3;
                }
                (Some('I'), Some('3'), Some('2')) => {
                    bits = 32;
                    i += 3;
                }
                (Some('l'), Some('l'), _) => {
                    bits = 64;
                    i += 2;
                }
                (Some('h'), Some('h'), _) => {
                    bits = 8;
                    i += 2;
                }
                (Some('h'), ..) => {
                    bits = 16;
                    text_wide = false;
                    i += 1;
                }
                (Some('l' | 'w'), ..) => {
                    // `long` is 32 bits on Windows; `%ls` is UTF-16.
                    text_wide = true;
                    i += 1;
                }
                (Some('I' | 'z' | 'j' | 't'), ..) => {
                    bits = 64;
                    i += 1;
                }
                (Some('L'), ..) => i += 1,
                _ => break,
            }
        }
        let Some(&conversion) = f.get(i) else { break };
        i += 1;
        let signed = |v: u64| -> i64 {
            match bits {
                8 => v as i8 as i64,
                16 => v as i16 as i64,
                32 => v as i32 as i64,
                _ => v as i64,
            }
        };
        let unsigned = |v: u64| -> u64 {
            match bits {
                8 => v as u8 as u64,
                16 => v as u16 as u64,
                32 => v as u32 as u64,
                _ => v,
            }
        };
        let (mut sign, body, numeric) = match conversion {
            'd' | 'i' => {
                let v = signed(args.next());
                let digits = v.unsigned_abs().to_string();
                let s = if v < 0 {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                (s.to_string(), pad_precision(digits, precision), true)
            }
            'u' => (
                String::new(),
                pad_precision(unsigned(args.next()).to_string(), precision),
                true,
            ),
            'o' => {
                let d = format!("{:o}", unsigned(args.next()));
                (
                    String::new(),
                    pad_precision(if alt { format!("0{d}") } else { d }, precision),
                    true,
                )
            }
            'x' | 'X' => {
                let v = unsigned(args.next());
                let d = if conversion == 'x' {
                    format!("{v:x}")
                } else {
                    format!("{v:X}")
                };
                let prefix = if alt && v != 0 {
                    if conversion == 'x' { "0x" } else { "0X" }
                } else {
                    ""
                };
                (prefix.to_string(), pad_precision(d, precision), true)
            }
            'p' => (String::new(), format!("{:016X}", args.next()), false),
            'c' | 'C' => {
                let v = args.next() as u32;
                let c = if conversion == 'C' || text_wide {
                    char::from_u32(v & 0xFFFF).unwrap_or('\u{FFFD}')
                } else {
                    (v as u8) as char
                };
                (String::new(), c.to_string(), false)
            }
            's' | 'S' => {
                let w = if conversion == 'S' {
                    !text_wide
                } else {
                    text_wide
                };
                // SAFETY: the argument is a string of that width.
                (
                    String::new(),
                    unsafe { text_at(args.next(), w, precision) },
                    false,
                )
            }
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'a' | 'A' => {
                let v = f64::from_bits(args.next());
                let p = precision.unwrap_or(6);
                let s = if v.is_sign_negative() {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                let a = v.abs();
                let body = if a.is_nan() {
                    "nan".into()
                } else if a.is_infinite() {
                    "inf".into()
                } else {
                    match conversion {
                        'f' | 'F' => format!("{a:.p$}"),
                        'e' | 'E' => exponent(a, p, conversion == 'E'),
                        'a' | 'A' => format!("{a:e}"),
                        _ => general(a, p, conversion == 'G', alt),
                    }
                };
                let body = if conversion.is_ascii_uppercase() {
                    body.to_uppercase()
                } else {
                    body
                };
                (s.to_string(), body, a.is_finite())
            }
            'n' => {
                // SAFETY: the argument is where to write the count so far.
                unsafe { (args.next() as *mut i32).write(out.chars().count() as i32) };
                continue;
            }
            '%' => (String::new(), "%".into(), false),
            other => (String::new(), format!("%{other}"), false),
        };
        let len = sign.chars().count() + body.chars().count();
        if len >= width {
            out.push_str(&sign);
            out.push_str(&body);
        } else if left {
            out.push_str(&sign);
            out.push_str(&body);
            out.extend(std::iter::repeat_n(' ', width - len));
        } else if zero
            && numeric
            && precision.is_none_or(|_| !matches!(conversion, 'd' | 'i' | 'u' | 'x' | 'X' | 'o'))
        {
            out.push_str(&std::mem::take(&mut sign));
            out.extend(std::iter::repeat_n('0', width - len));
            out.push_str(&body);
        } else {
            out.extend(std::iter::repeat_n(' ', width - len));
            out.push_str(&sign);
            out.push_str(&body);
        }
    }
    out
}

fn pad_precision(digits: String, precision: Option<usize>) -> String {
    match precision {
        Some(0) if digits == "0" => String::new(),
        Some(p) if digits.len() < p => format!("{}{digits}", "0".repeat(p - digits.len())),
        _ => digits,
    }
}

/// `sscanf`: reads `input` as `format` says, writing through the pointers in
/// `args`; the number of fields assigned, or -1 if input ran out first.
/// shortcut: scansets (`%[...]`) take any of the listed characters, without
/// ranges; `%n` and assignment suppression work.
///
/// # Safety
/// As [`format`], and every pointer `args` gives is writable for its type.
pub unsafe fn scan(input: &str, format: *const u8, wide: bool, args: &mut Args) -> i32 {
    // SAFETY: the caller's format string.
    let f: Vec<char> = unsafe { text_at(format as u64, wide, None) }
        .chars()
        .collect();
    let s: Vec<char> = input.chars().collect();
    let (mut i, mut at, mut assigned) = (0, 0, 0);
    while i < f.len() {
        let c = f[i];
        if c.is_whitespace() {
            while s.get(at).is_some_and(|c| c.is_whitespace()) {
                at += 1;
            }
            i += 1;
            continue;
        }
        if c != '%' || f.get(i + 1) == Some(&'%') {
            if s.get(at) != Some(&c) {
                break;
            }
            at += 1;
            i += if c == '%' { 2 } else { 1 };
            continue;
        }
        i += 1;
        let skip = f.get(i) == Some(&'*');
        if skip {
            i += 1;
        }
        let mut width = 0;
        while let Some(d) = f.get(i).and_then(|c| c.to_digit(10)) {
            width = width * 10 + d as usize;
            i += 1;
        }
        let mut bits = 32;
        let mut text_wide = wide;
        while let Some(&l) = f.get(i) {
            match l {
                'l' if f.get(i + 1) == Some(&'l') => {
                    bits = 64;
                    i += 1;
                }
                'l' | 'w' => {
                    text_wide = true;
                    if matches!(f.get(i + 1), Some('f' | 'e' | 'g')) {
                        bits = 64;
                    }
                }
                'h' => {
                    bits = if f.get(i + 1) == Some(&'h') { 8 } else { 16 };
                    text_wide = false;
                }
                'I' if f.get(i + 1) == Some(&'6') => {
                    bits = 64;
                    i += 2;
                }
                'z' | 'j' | 't' => bits = 64,
                _ => break,
            }
            i += 1;
        }
        let Some(&conv) = f.get(i) else { break };
        i += 1;
        if conv == 'n' {
            if !skip {
                // SAFETY: an int pointer, as `%n` takes.
                unsafe { (args.next() as *mut i32).write(at as i32) };
            }
            continue;
        }
        if !matches!(conv, 'c' | '[') {
            while s.get(at).is_some_and(|c| c.is_whitespace()) {
                at += 1;
            }
        }
        if at >= s.len() {
            return if assigned == 0 { -1 } else { assigned };
        }
        let limit = if width == 0 { usize::MAX } else { width };
        let start = at;
        let take = |at: &mut usize, ok: &dyn Fn(char, usize) -> bool| {
            while *at < s.len() && *at - start < limit && ok(s[*at], *at - start) {
                *at += 1;
            }
        };
        match conv {
            'd' | 'i' | 'u' | 'x' | 'X' | 'o' => {
                let radix = match conv {
                    'x' | 'X' => 16,
                    'o' => 8,
                    _ => 10,
                };
                take(&mut at, &|c, k| {
                    c.is_digit(radix) || k == 0 && (c == '-' || c == '+')
                });
                let text: String = s[start..at].iter().collect();
                let text = if radix == 16 {
                    text.trim_start_matches("0x").to_string()
                } else {
                    text
                };
                let Ok(v) = i128::from_str_radix(&text, radix) else {
                    break;
                };
                if !skip {
                    let p = args.next();
                    // SAFETY: a pointer of the size the length asks.
                    unsafe {
                        match bits {
                            8 => (p as *mut u8).write(v as u8),
                            16 => (p as *mut u16).write(v as u16),
                            64 => (p as *mut u64).write(v as u64),
                            _ => (p as *mut u32).write(v as u32),
                        }
                    }
                    assigned += 1;
                }
            }
            'f' | 'e' | 'g' | 'E' | 'G' | 'a' => {
                take(&mut at, &|c, k| {
                    c.is_ascii_digit()
                        || c == '.'
                        || c == 'e'
                        || c == 'E'
                        || (c == '-' || c == '+')
                            && (k == 0 || matches!(s[start + k - 1], 'e' | 'E'))
                });
                let Ok(v) = s[start..at].iter().collect::<String>().parse::<f64>() else {
                    break;
                };
                if !skip {
                    let p = args.next();
                    // SAFETY: a float or double pointer, as `%f` or `%lf` take.
                    unsafe {
                        if bits == 64 {
                            (p as *mut f64).write(v)
                        } else {
                            (p as *mut f32).write(v as f32)
                        }
                    }
                    assigned += 1;
                }
            }
            's' | 'c' | '[' => {
                let set: Vec<char>;
                let (negate, members) = if conv == '[' {
                    let neg = f.get(i) == Some(&'^');
                    if neg {
                        i += 1;
                    }
                    let end = f[i + 1..]
                        .iter()
                        .position(|&c| c == ']')
                        .map_or(f.len(), |p| i + 1 + p);
                    set = f[i..end].to_vec();
                    i = (end + 1).min(f.len());
                    (neg, set.as_slice())
                } else {
                    (false, &[][..])
                };
                let count = if conv == 'c' {
                    if width == 0 { 1 } else { width }
                } else {
                    limit
                };
                while at < s.len() && at - start < count {
                    let c = s[at];
                    let ok = match conv {
                        's' => !c.is_whitespace(),
                        'c' => true,
                        _ => members.contains(&c) != negate,
                    };
                    if !ok {
                        break;
                    }
                    at += 1;
                }
                if at == start {
                    break;
                }
                if !skip {
                    let p = args.next();
                    let got: String = s[start..at].iter().collect();
                    // SAFETY: a buffer for the field (and a NUL but for `%c`).
                    unsafe {
                        if text_wide {
                            let units: Vec<u16> = got.encode_utf16().collect();
                            std::ptr::copy_nonoverlapping(
                                units.as_ptr(),
                                p as *mut u16,
                                units.len(),
                            );
                            if conv != 'c' {
                                (p as *mut u16).add(units.len()).write(0);
                            }
                        } else {
                            std::ptr::copy_nonoverlapping(got.as_ptr(), p as *mut u8, got.len());
                            if conv != 'c' {
                                (p as *mut u8).add(got.len()).write(0);
                            }
                        }
                    }
                    assigned += 1;
                }
            }
            _ => break,
        }
    }
    assigned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printf(f: &str, slots: &[u64]) -> String {
        let f = std::ffi::CString::new(f).unwrap();
        // SAFETY: the slots hold what the format asks.
        unsafe { format(f.as_ptr().cast(), false, &mut Args::new(slots.as_ptr())) }
    }

    #[test]
    fn integers_and_widths() {
        assert_eq!(
            printf("%d|%5d|%-5d|%05d|%+d", &[(-3i64) as u64, 42, 42, 42, 7]),
            "-3|   42|42   |00042|+7"
        );
        assert_eq!(
            printf("%u %x %X %#x %o", &[u64::MAX, 255, 255, 255, 8]),
            "4294967295 ff FF 0xff 10"
        );
        assert_eq!(
            printf("%I64d %lld %zu", &[(-5i64) as u64, 1 << 40, 99]),
            "-5 1099511627776 99"
        );
        assert_eq!(printf("%ld", &[u64::MAX]), "-1", "long is 32 bits");
        assert_eq!(printf("%*d|%.3d", &[4, 7, 7]), "   7|007");
    }

    #[test]
    fn floats_strings_and_pointers() {
        let pi = std::f64::consts::PI.to_bits();
        assert_eq!(
            printf("%f %.2f %e %g", &[pi, pi, pi, pi]),
            "3.141593 3.14 3.141593e+00 3.14159"
        );
        assert_eq!(
            printf("%g %g", &[1e-5f64.to_bits(), 100000.0f64.to_bits()]),
            "1e-05 100000"
        );
        let narrow = c"text";
        let wide: Vec<u16> = "wide".encode_utf16().chain([0]).collect();
        assert_eq!(
            printf(
                "[%s] [%ls] [%.2s] [%c]",
                &[
                    narrow.as_ptr() as u64,
                    wide.as_ptr() as u64,
                    narrow.as_ptr() as u64,
                    b'z'.into()
                ]
            ),
            "[text] [wide] [te] [z]"
        );
        assert_eq!(printf("%p %%", &[0x1234]), "0000000000001234 %");
    }

    #[test]
    fn scanning_fields() {
        let (mut a, mut b, mut c) = (0i32, 0f32, [0u8; 8]);
        let f = c"%d, %f %s";
        let slots = [
            (&raw mut a) as u64,
            (&raw mut b) as u64,
            c.as_mut_ptr() as u64,
        ];
        // SAFETY: the pointers match the format.
        let n = unsafe {
            scan(
                "-12, 2.5 word rest",
                f.as_ptr().cast(),
                false,
                &mut Args::new(slots.as_ptr()),
            )
        };
        assert_eq!((n, a, b), (3, -12, 2.5));
        assert_eq!(&c[..5], b"word\0");
        // SAFETY: as above.
        assert_eq!(
            unsafe { scan("", f.as_ptr().cast(), false, &mut Args::new(slots.as_ptr())) },
            -1
        );
    }
}
