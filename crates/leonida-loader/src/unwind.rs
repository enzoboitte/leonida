//! Windows x64 stack unwinding: what `RtlLookupFunctionEntry` and
//! `RtlVirtualUnwind` do, for hosts without Windows (macOS).
//!
//! It reads an image's `.pdata` (`RUNTIME_FUNCTION`s) and `UNWIND_INFO` as
//! the x64 exception-handling documentation describes them (Microsoft Learn,
//! "x64 exception handling"), straight from mapped title memory. A frame
//! stopped inside an epilog is recognised by its instructions, as Windows
//! does, since unwind codes describe only the prolog.

/// `UNW_FLAG_EHANDLER`: the handler wants to see exceptions.
pub const UNW_FLAG_EHANDLER: u8 = 1;
/// `UNW_FLAG_UHANDLER`: the handler wants to see unwinds.
pub const UNW_FLAG_UHANDLER: u8 = 2;
const UNW_FLAG_CHAININFO: u8 = 4;

const UWOP_PUSH_NONVOL: u8 = 0;
const UWOP_ALLOC_LARGE: u8 = 1;
const UWOP_ALLOC_SMALL: u8 = 2;
const UWOP_SET_FPREG: u8 = 3;
const UWOP_SAVE_NONVOL: u8 = 4;
const UWOP_SAVE_NONVOL_FAR: u8 = 5;
const UWOP_EPILOG: u8 = 6;
const UWOP_SPARE: u8 = 7;
const UWOP_SAVE_XMM128: u8 = 8;
const UWOP_SAVE_XMM128_FAR: u8 = 9;
const UWOP_PUSH_MACHFRAME: u8 = 10;

/// `CONTEXT_AMD64 | CONTROL | INTEGER | SEGMENTS | FLOATING_POINT`.
pub const CONTEXT_FULL: u32 = 0x0010_000F;
/// Offset of XMM0 in [`Context::flt_save`] (the FXSAVE layout).
const XMM0: usize = 0xA0;

/// Windows x64 `CONTEXT`, laid out as title code reads it (0x4D0 bytes).
/// `gpr` is in the machine's register order: rax, rcx, rdx, rbx, rsp, rbp,
/// rsi, rdi, r8 to r15.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct Context {
    pub p_home: [u64; 6],
    pub context_flags: u32,
    pub mx_csr: u32,
    pub seg: [u16; 6],
    pub eflags: u32,
    pub dr: [u64; 6],
    pub gpr: [u64; 16],
    pub rip: u64,
    pub flt_save: [u8; 512],
    pub vector_register: [[u64; 2]; 26],
    pub vector_control: u64,
    pub debug: [u64; 5],
}

const _: () = assert!(size_of::<Context>() == 0x4D0);
const _: () = assert!(std::mem::offset_of!(Context, gpr) == 0x78);
const _: () = assert!(std::mem::offset_of!(Context, rip) == 0xF8);
const _: () = assert!(std::mem::offset_of!(Context, flt_save) == 0x100);

pub const RSP: usize = 4;

impl Context {
    pub fn zeroed() -> Self {
        // SAFETY: every field is an integer or an array of them.
        unsafe { std::mem::zeroed() }
    }

    pub fn rsp(&self) -> u64 {
        self.gpr[RSP]
    }

    pub fn set_xmm(&mut self, n: usize, value: [u8; 16]) {
        self.flt_save[XMM0 + 16 * n..XMM0 + 16 * (n + 1)].copy_from_slice(&value);
    }
}

/// `RUNTIME_FUNCTION`: one entry of `.pdata`, as RVAs.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeFunction {
    pub begin: u32,
    pub end: u32,
    pub unwind: u32,
}

/// The entry of a sorted `.pdata` whose function holds `rva`.
pub fn lookup(pdata: &[RuntimeFunction], rva: u32) -> Option<&RuntimeFunction> {
    let i = pdata.partition_point(|f| f.end <= rva);
    pdata.get(i).filter(|f| f.begin <= rva)
}

/// What [`virtual_unwind`] found about the frame it unwound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unwound {
    pub establisher_frame: u64,
    /// The language handler of the requested kind and its data, if the
    /// frame has one and was stopped past its prolog.
    pub handler: Option<(u64, u64)>,
}

/// Reads a `T` at `address`.
///
/// # Safety
/// `address..address + size_of::<T>()` is readable.
unsafe fn read<T: Copy>(address: u64) -> T {
    // SAFETY: the caller's.
    unsafe { (address as *const T).read_unaligned() }
}

/// Unwinds one frame of `ctx`, which is stopped at `ctx.rip` inside
/// `function` of the image at `base`: on return `ctx` is the caller's
/// context. `kind` is [`UNW_FLAG_EHANDLER`] or [`UNW_FLAG_UHANDLER`].
///
/// # Safety
/// `base` is a mapped image whose `.pdata` holds `function`, and the stack
/// `ctx` describes is readable.
pub unsafe fn virtual_unwind(
    kind: u8,
    base: u64,
    function: &RuntimeFunction,
    ctx: &mut Context,
) -> Unwound {
    let pc = ctx.rip;
    let mut function = *function;
    let mut frame_ret = ctx.rsp();
    let mut mach_frame = false;
    let mut handler = None;
    let mut first = true;
    loop {
        let info = base + function.unwind as u64;
        // SAFETY: `info` is this image's UNWIND_INFO (the caller's promise).
        let [version_flags, prolog, count, frame] = unsafe { read::<[u8; 4]>(info) };
        let flags = version_flags >> 3;
        let (frame_reg, frame_off) = ((frame & 0xF) as usize, (frame >> 4) as u64);
        let codes = info + 4;
        let mut base_frame = ctx.rsp();
        if frame_reg != 0 {
            base_frame = ctx.gpr[frame_reg].wrapping_sub(frame_off * 16);
        }

        let begin = base + function.begin as u64;
        let prolog_offset = if (begin..begin + prolog as u64).contains(&pc) {
            pc - begin
        } else {
            // A chained entry has no epilog of its own.
            // SAFETY: `pc` is in this image's code.
            if flags & UNW_FLAG_CHAININFO == 0 && unsafe { in_epilog(pc, base, &function) } {
                // SAFETY: as above, and the stack is readable.
                unsafe { run_epilog(pc, ctx) };
                return Unwound {
                    establisher_frame: base_frame,
                    handler: None,
                };
            }
            u64::MAX
        };

        let mut i = 0;
        while i < count as usize {
            // SAFETY: inside the unwind codes `count` says there are.
            let [offset, op] = unsafe { read::<[u8; 2]>(codes + 2 * i as u64) };
            let (code, op_info) = (op & 0xF, (op >> 4) as usize);
            // SAFETY: the next slots belong to this code.
            let slot16 = |n: u64| unsafe { read::<u16>(codes + 2 * (i as u64 + n)) } as u64;
            // SAFETY: as above.
            let slot32 = || unsafe { read::<u32>(codes + 2 * (i as u64 + 1)) } as u64;
            let size = match code {
                UWOP_ALLOC_LARGE => 2 + (op_info != 0) as usize,
                UWOP_SAVE_NONVOL | UWOP_SAVE_XMM128 | UWOP_EPILOG => 2,
                UWOP_SAVE_NONVOL_FAR | UWOP_SAVE_XMM128_FAR | UWOP_SPARE => 3,
                _ => 1,
            };
            if prolog_offset >= offset as u64 {
                let rsp = ctx.rsp();
                // SAFETY: the saved values are on the frame's readable stack.
                unsafe {
                    match code {
                        UWOP_PUSH_NONVOL => {
                            ctx.gpr[op_info] = read(rsp);
                            ctx.gpr[RSP] = rsp + 8;
                        }
                        UWOP_ALLOC_LARGE if op_info == 0 => ctx.gpr[RSP] = rsp + slot16(1) * 8,
                        UWOP_ALLOC_LARGE => ctx.gpr[RSP] = rsp + slot32(),
                        UWOP_ALLOC_SMALL => ctx.gpr[RSP] = rsp + op_info as u64 * 8 + 8,
                        UWOP_SET_FPREG => {
                            ctx.gpr[RSP] = base_frame;
                            frame_ret = base_frame;
                        }
                        UWOP_SAVE_NONVOL => ctx.gpr[op_info] = read(base_frame + slot16(1) * 8),
                        UWOP_SAVE_NONVOL_FAR => ctx.gpr[op_info] = read(base_frame + slot32()),
                        UWOP_SAVE_XMM128 => ctx.set_xmm(op_info, read(base_frame + slot16(1) * 16)),
                        UWOP_SAVE_XMM128_FAR => ctx.set_xmm(op_info, read(base_frame + slot32())),
                        UWOP_PUSH_MACHFRAME => {
                            let at = rsp + if op_info != 0 { 8 } else { 0 };
                            ctx.rip = read(at);
                            ctx.gpr[RSP] = read(at + 24);
                            mach_frame = true;
                        }
                        _ => {}
                    }
                }
            }
            i += size;
        }

        let after = codes + 2 * ((count as u64 + 1) & !1);
        if first && flags & kind != 0 && prolog_offset == u64::MAX {
            // SAFETY: the handler's RVA follows the codes when a handler flag is set.
            let rva = unsafe { read::<u32>(after) } as u64;
            handler = Some((base + rva, after + 4));
        }
        first = false;
        if flags & UNW_FLAG_CHAININFO == 0 {
            break;
        }
        // SAFETY: a chained entry follows the codes.
        function = unsafe { read::<RuntimeFunction>(after) };
    }

    if !mach_frame {
        // SAFETY: the return address is on the readable stack.
        ctx.rip = unsafe { read(ctx.rsp()) };
        ctx.gpr[RSP] += 8;
    }
    Unwound {
        establisher_frame: frame_ret,
        handler,
    }
}

/// Unwinds a leaf function (no `.pdata` entry): its return address is at rsp.
///
/// # Safety
/// The stack `ctx` describes is readable.
pub unsafe fn unwind_leaf(ctx: &mut Context) {
    // SAFETY: the caller's.
    ctx.rip = unsafe { read(ctx.rsp()) };
    ctx.gpr[RSP] += 8;
}

/// Whether `pc` stands at an epilog: an optional `add rsp` / `lea rsp`, then
/// pops, then `ret` (or a jump to one inside the function).
///
/// # Safety
/// The code at `pc` is readable.
unsafe fn in_epilog(pc: u64, base: u64, function: &RuntimeFunction) -> bool {
    // SAFETY: the caller's; the reads stay within an instruction or two.
    let byte = |at: u64| unsafe { read::<u8>(at) };
    let mut at = pc;
    if byte(at) & 0xF8 == 0x48 {
        let (rex, op, modrm) = (byte(at), byte(at + 1), byte(at + 2));
        match op {
            0x81 if rex == 0x48 && modrm == 0xC4 => at += 7,
            0x83 if rex == 0x48 && modrm == 0xC4 => at += 4,
            0x8D => {
                if rex & 0x06 != 0 || (modrm >> 3) & 7 != 4 || modrm & 7 == 4 {
                    return false;
                }
                match modrm >> 6 {
                    1 => at += 4,
                    2 => at += 7,
                    _ => return false,
                }
            }
            _ => {}
        }
    }
    let inside = |at: u64| (base + function.begin as u64..base + function.end as u64).contains(&at);
    loop {
        if byte(at) & 0xF0 == 0x40 {
            at += 1;
        }
        match byte(at) {
            0x58..=0x5F => at += 1,
            0xC2 | 0xC3 => return true,
            0xF3 => return byte(at + 1) == 0xC3,
            0xE9 => {
                // SAFETY: a rel32 follows the opcode.
                at = at
                    .wrapping_add(5)
                    .wrapping_add(unsafe { read::<i32>(at + 1) } as i64 as u64);
                if !inside(at) {
                    return false;
                }
            }
            0xEB => {
                at = at
                    .wrapping_add(2)
                    .wrapping_add(byte(at + 1) as i8 as i64 as u64);
                if !inside(at) {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// Runs the rest of an epilog [`in_epilog`] recognised, on `ctx`.
///
/// # Safety
/// As [`in_epilog`], and the stack `ctx` describes is readable.
unsafe fn run_epilog(pc: u64, ctx: &mut Context) {
    // SAFETY: the caller's.
    let byte = |at: u64| unsafe { read::<u8>(at) };
    let mut at = pc;
    loop {
        let mut rex = 0;
        if byte(at) & 0xF0 == 0x40 {
            rex = byte(at) & 0xF;
            at += 1;
        }
        let rsp = ctx.rsp();
        // SAFETY: immediates follow their opcodes; the stack is readable.
        unsafe {
            match byte(at) {
                op @ 0x58..=0x5F => {
                    ctx.gpr[(op - 0x58) as usize + (rex as usize & 1) * 8] = read(rsp);
                    ctx.gpr[RSP] = rsp + 8;
                    at += 1;
                }
                0x81 => {
                    ctx.gpr[RSP] = rsp.wrapping_add(read::<i32>(at + 2) as i64 as u64);
                    at += 6;
                }
                0x83 => {
                    ctx.gpr[RSP] = rsp.wrapping_add(byte(at + 2) as i8 as i64 as u64);
                    at += 3;
                }
                0x8D => {
                    let modrm = byte(at + 1);
                    let reg = ctx.gpr[(modrm & 7) as usize + (rex as usize & 1) * 8];
                    if modrm >> 6 == 1 {
                        ctx.gpr[RSP] = reg.wrapping_add(byte(at + 2) as i8 as i64 as u64);
                        at += 3;
                    } else {
                        ctx.gpr[RSP] = reg.wrapping_add(read::<i32>(at + 2) as i64 as u64);
                        at += 6;
                    }
                }
                0xC2 => {
                    ctx.rip = read(rsp);
                    ctx.gpr[RSP] = rsp + 8 + read::<u16>(at + 1) as u64;
                    return;
                }
                0xC3 | 0xF3 => {
                    ctx.rip = read(rsp);
                    ctx.gpr[RSP] = rsp + 8;
                    return;
                }
                0xE9 => {
                    at = at
                        .wrapping_add(5)
                        .wrapping_add(read::<i32>(at + 1) as i64 as u64)
                }
                0xEB => {
                    at = at
                        .wrapping_add(2)
                        .wrapping_add(byte(at + 1) as i8 as i64 as u64)
                }
                _ => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RBX: usize = 3;
    const RBP: usize = 5;
    const RSI: usize = 6;

    /// A fake image: code at 0x100, unwind info at 0x200, a stack at 0x400.
    struct Image {
        bytes: Vec<u8>,
    }

    impl Image {
        fn new() -> Self {
            Image {
                bytes: vec![0xCC; 0x1000],
            }
        }
        fn base(&self) -> u64 {
            self.bytes.as_ptr() as u64
        }
        fn put(&mut self, at: usize, bytes: &[u8]) {
            self.bytes[at..at + bytes.len()].copy_from_slice(bytes);
        }
        fn put_u64(&mut self, at: usize, value: u64) {
            self.put(at, &value.to_le_bytes());
        }
    }

    /// The prolog `push rbp; push rbx; sub rsp, 0x28; mov rbp, rsp`
    /// (`lea rbp, [rsp]`) at 0x100, and a stack frame at 0x400 for it,
    /// with a body instruction at 0x10A.
    fn framed() -> (Image, RuntimeFunction) {
        let mut image = Image::new();
        image.put(
            0x100,
            &[
                0x55, 0x53, 0x48, 0x83, 0xEC, 0x28, 0x48, 0x8D, 0x2C, 0x24, 0x90,
            ],
        );
        // Version 1, EHANDLER, prolog 10 bytes, 4 codes, frame reg rbp, offset 0.
        image.put(0x200, &[0x01 | (UNW_FLAG_EHANDLER << 3), 10, 4, RBP as u8]);
        image.put(0x204, &[10, UWOP_SET_FPREG]);
        image.put(0x206, &[6, UWOP_ALLOC_SMALL | (4 << 4)]); // 4 * 8 + 8 = 0x28
        image.put(0x208, &[2, UWOP_PUSH_NONVOL | ((RBX as u8) << 4)]);
        image.put(0x20A, &[1, UWOP_PUSH_NONVOL | ((RBP as u8) << 4)]);
        image.put(0x20C, &0x180u32.to_le_bytes()); // handler RVA
        // Stack: 0x28 of locals, then the saved rbx, rbp and the return address.
        image.put_u64(0x400 + 0x28, 0xB0B);
        image.put_u64(0x400 + 0x30, 0xBEEF);
        image.put_u64(0x400 + 0x38, 0x1234_5678);
        let function = RuntimeFunction {
            begin: 0x100,
            end: 0x120,
            unwind: 0x200,
        };
        (image, function)
    }

    #[test]
    fn a_framed_function_unwinds_to_its_caller_with_its_handler() {
        let (image, function) = framed();
        let mut ctx = Context::zeroed();
        ctx.rip = image.base() + 0x10A;
        ctx.gpr[RBP] = image.base() + 0x400;
        ctx.gpr[RSP] = image.base() + 0x3F0; // below the frame: rbp counts, not rsp
        // SAFETY: `ctx` describes the fake image's frame.
        let unwound =
            unsafe { virtual_unwind(UNW_FLAG_EHANDLER, image.base(), &function, &mut ctx) };
        assert_eq!(ctx.rip, 0x1234_5678);
        assert_eq!(ctx.rsp(), image.base() + 0x440);
        assert_eq!((ctx.gpr[RBX], ctx.gpr[RBP]), (0xB0B, 0xBEEF));
        assert_eq!(unwound.establisher_frame, image.base() + 0x400);
        assert_eq!(
            unwound.handler,
            Some((image.base() + 0x180, image.base() + 0x210))
        );
    }

    #[test]
    fn only_the_prolog_done_so_far_is_undone() {
        let (image, function) = framed();
        let mut ctx = Context::zeroed();
        // After `push rbp; push rbx`, before `sub rsp`.
        ctx.rip = image.base() + 0x102;
        ctx.gpr[RSP] = image.base() + 0x428;
        ctx.gpr[RBP] = 0x5555;
        // SAFETY: as above.
        let unwound =
            unsafe { virtual_unwind(UNW_FLAG_EHANDLER, image.base(), &function, &mut ctx) };
        assert_eq!(ctx.rip, 0x1234_5678);
        assert_eq!(ctx.rsp(), image.base() + 0x440);
        assert_eq!((ctx.gpr[RBX], ctx.gpr[RBP]), (0xB0B, 0xBEEF));
        assert_eq!(unwound.handler, None, "no handler inside the prolog");
    }

    #[test]
    fn an_epilog_is_run_instead_of_the_unwind_codes() {
        let (mut image, function) = framed();
        // `add rsp, 0x28; pop rbx; pop rbp; ret` at 0x110.
        image.put(0x110, &[0x48, 0x83, 0xC4, 0x28, 0x5B, 0x5D, 0xC3]);
        let mut ctx = Context::zeroed();
        ctx.rip = image.base() + 0x110;
        ctx.gpr[RSP] = image.base() + 0x400;
        ctx.gpr[RBP] = 0x5555; // not rbp's saved value: the epilog must not use it
        // SAFETY: as above.
        let unwound =
            unsafe { virtual_unwind(UNW_FLAG_EHANDLER, image.base(), &function, &mut ctx) };
        assert_eq!(ctx.rip, 0x1234_5678);
        assert_eq!(ctx.rsp(), image.base() + 0x440);
        assert_eq!((ctx.gpr[RBX], ctx.gpr[RBP]), (0xB0B, 0xBEEF));
        assert_eq!(unwound.handler, None);
    }

    #[test]
    fn saved_registers_and_chained_info_are_followed() {
        let mut image = Image::new();
        // Main part: `sub rsp, 0x20; mov [rsp+0x30], rsi` (codes only).
        image.put(0x300, &[0x01, 9, 3, 0]);
        image.put(0x304, &[9, UWOP_SAVE_NONVOL | ((RSI as u8) << 4)]);
        image.put(0x306, &6u16.to_le_bytes()); // offset 6 * 8 = 0x30
        image.put(0x308, &[4, UWOP_ALLOC_SMALL | (3 << 4)]); // 0x20
        // A part of the function that chains to the main one.
        image.put(0x200, &[0x01 | (UNW_FLAG_CHAININFO << 3), 0, 0, 0]);
        let main = RuntimeFunction {
            begin: 0x100,
            end: 0x110,
            unwind: 0x300,
        };
        let chained = RuntimeFunction {
            begin: 0x110,
            end: 0x120,
            unwind: 0x200,
        };
        image.put(
            0x204,
            &[main.begin, main.end, main.unwind]
                .map(u32::to_le_bytes)
                .concat(),
        );
        image.put_u64(0x400 + 0x30, 0x51);
        image.put_u64(0x400 + 0x20, 0xCA11);
        let mut ctx = Context::zeroed();
        ctx.rip = image.base() + 0x118;
        ctx.gpr[RSP] = image.base() + 0x400;
        // SAFETY: as above.
        unsafe { virtual_unwind(UNW_FLAG_EHANDLER, image.base(), &chained, &mut ctx) };
        assert_eq!(ctx.gpr[RSI], 0x51);
        assert_eq!(ctx.rip, 0xCA11);
        assert_eq!(ctx.rsp(), image.base() + 0x428);
    }

    #[test]
    fn lookup_finds_the_function_holding_an_rva() {
        let f = |begin, end| RuntimeFunction {
            begin,
            end,
            unwind: 0,
        };
        let pdata = [f(0x10, 0x20), f(0x20, 0x40), f(0x50, 0x60)];
        assert_eq!(lookup(&pdata, 0x10), Some(&pdata[0]));
        assert_eq!(lookup(&pdata, 0x3F), Some(&pdata[1]));
        assert_eq!(lookup(&pdata, 0x45), None);
        assert_eq!(lookup(&pdata, 0x60), None);
        assert_eq!(lookup(&pdata, 0x5), None);
    }
}
