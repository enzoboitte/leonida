//! Virtual memory and heaps, as Windows gives them: an address range is
//! reserved (64 KiB aligned, no access), then pages of it are committed
//! (zeroed, with a protection), decommitted and released.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::Mutex;

use super::set_last_error;

pub(super) const PAGE: usize = 0x1000;
pub(super) const GRANULARITY: usize = 0x1_0000;

const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_DECOMMIT: u32 = 0x4000;
const MEM_RELEASE: u32 = 0x8000;
const MEM_FREE: u32 = 0x1_0000;
const MEM_PRIVATE: u32 = 0x2_0000;
const MEM_RESET: u32 = 0x8_0000;
const MEM_TOP_DOWN: u32 = 0x10_0000;
const MEM_LARGE_PAGES: u32 = 0x2000_0000;

const PAGE_NOACCESS: u32 = 0x01;
const PAGE_READONLY: u32 = 0x02;
const PAGE_READWRITE: u32 = 0x04;
const PAGE_WRITECOPY: u32 = 0x08;
const PAGE_EXECUTE: u32 = 0x10;
const PAGE_EXECUTE_READ: u32 = 0x20;
const PAGE_EXECUTE_READWRITE: u32 = 0x40;
const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;
/// Modifiers: kept in the page's protection, not given to the host.
const PAGE_MODIFIERS: u32 = 0x100 | 0x200 | 0x400;

const ERROR_INVALID_ADDRESS: u32 = 487;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;

/// One reservation: its size, and each page's protection (0: reserved only).
struct Reservation {
    size: usize,
    pages: Vec<u32>,
    /// The protection the reservation was made with.
    initial: u32,
}

static RESERVATIONS: Mutex<BTreeMap<usize, Reservation>> = Mutex::new(BTreeMap::new());

fn reservations() -> std::sync::MutexGuard<'static, BTreeMap<usize, Reservation>> {
    RESERVATIONS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The host protection for a Windows one; `None` for an unknown one.
fn host_protection(protect: u32) -> Option<i32> {
    let (r, w, x) = (libc::PROT_READ, libc::PROT_WRITE, libc::PROT_EXEC);
    Some(match protect & !PAGE_MODIFIERS {
        PAGE_NOACCESS => libc::PROT_NONE,
        PAGE_READONLY => r,
        PAGE_READWRITE | PAGE_WRITECOPY => r | w,
        PAGE_EXECUTE => x,
        PAGE_EXECUTE_READ => r | x,
        PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY => r | w | x,
        _ => return None,
    })
}

/// Reserves `size` bytes aligned to 64 KiB, at `at` if given, with no access.
fn reserve(at: usize, size: usize) -> Option<usize> {
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE;
    if at != 0 {
        // SAFETY: a hint only (no MAP_FIXED), so nothing mapped is replaced.
        let got = unsafe { libc::mmap(at as *mut _, size, libc::PROT_NONE, flags, -1, 0) };
        if got == libc::MAP_FAILED {
            return None;
        }
        if got as usize != at {
            // SAFETY: the mapping just made.
            unsafe { libc::munmap(got, size) };
            return None;
        }
        return Some(at);
    }
    // Over-reserve, then trim to the 64 KiB boundary inside.
    let span = size + GRANULARITY;
    // SAFETY: a fresh anonymous mapping.
    let got = unsafe { libc::mmap(std::ptr::null_mut(), span, libc::PROT_NONE, flags, -1, 0) };
    if got == libc::MAP_FAILED {
        return None;
    }
    let start = got as usize;
    let aligned = start.next_multiple_of(GRANULARITY);
    // SAFETY: both trimmed pieces are parts of the mapping just made.
    unsafe {
        if aligned > start {
            libc::munmap(start as *mut _, aligned - start);
        }
        let end = start + span;
        if end > aligned + size {
            libc::munmap((aligned + size) as *mut _, end - aligned - size);
        }
    }
    Some(aligned)
}

/// Gives `at..at+len` fresh zero pages with no access (what decommit leaves).
fn discard(at: usize, len: usize) {
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE | libc::MAP_FIXED;
    // SAFETY: the range is part of one of our reservations.
    unsafe { libc::mmap(at as *mut _, len, libc::PROT_NONE, flags, -1, 0) };
}

/// The reservation holding `at`, with its base.
fn holding(map: &mut BTreeMap<usize, Reservation>, at: usize) -> Option<(usize, &mut Reservation)> {
    let (&base, r) = map.range_mut(..=at).next_back()?;
    (at < base + r.size).then_some((base, r))
}

/// Reserves `size` bytes aligned to `align` (at least 64 KiB) inside
/// `low..=high`, committed with `protect` when `commit`: what the console's
/// `XMemVirtualAlloc` asks for. Recorded as any reservation is.
pub(super) fn reserve_in(
    size: usize,
    align: usize,
    low: usize,
    high: usize,
    protect: u32,
    commit: bool,
) -> Option<usize> {
    let prot = host_protection(protect)?;
    let align = align.max(GRANULARITY);
    let len = size.next_multiple_of(PAGE);
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE;
    let mut hint = low.next_multiple_of(align);
    // The host takes a free hint as given; try further on while it does not.
    for _ in 0..64 {
        if hint.checked_add(len)? > high {
            return None;
        }
        let span = len + align;
        // SAFETY: a hint only (no MAP_FIXED), so nothing mapped is replaced.
        let got = unsafe { libc::mmap(hint as *mut _, span, libc::PROT_NONE, flags, -1, 0) };
        if got == libc::MAP_FAILED {
            return None;
        }
        let start = got as usize;
        let at = start.next_multiple_of(align);
        if at < low || at + len - 1 > high {
            // SAFETY: the mapping just made.
            unsafe { libc::munmap(got, span) };
            hint = if start > hint {
                hint + span
            } else {
                hint + align
            };
            continue;
        }
        // SAFETY: both trimmed pieces are parts of the mapping just made.
        unsafe {
            if at > start {
                libc::munmap(got, at - start);
            }
            if start + span > at + len {
                libc::munmap((at + len) as *mut _, start + span - at - len);
            }
            if commit && libc::mprotect(at as *mut _, len, prot) != 0 {
                libc::munmap(at as *mut _, len);
                return None;
            }
        }
        let pages = vec![if commit { protect } else { 0 }; len / PAGE];
        reservations().insert(
            at,
            Reservation {
                size: len,
                pages,
                initial: protect,
            },
        );
        return Some(at);
    }
    None
}

/// Records that `at..at+len`, inside one of our reservations, now has
/// `protect` (0: reserved only), when something else mapped it there.
pub(super) fn mark(at: usize, len: usize, protect: u32) {
    let mut map = reservations();
    if let Some((base, r)) = holding(&mut map, at) {
        let first = (at - base) / PAGE;
        let last = ((at + len - base) / PAGE).min(r.pages.len());
        r.pages[first..last].fill(protect);
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn VirtualAlloc(
    address: *mut c_void,
    size: usize,
    kind: u32,
    protect: u32,
) -> *mut c_void {
    let Some(prot) = host_protection(protect) else {
        set_last_error(ERROR_INVALID_PARAMETER);
        return std::ptr::null_mut();
    };
    if size == 0 || kind & (MEM_COMMIT | MEM_RESERVE | MEM_RESET) == 0 {
        set_last_error(ERROR_INVALID_PARAMETER);
        return std::ptr::null_mut();
    }
    // shortcut: MEM_TOP_DOWN and MEM_LARGE_PAGES are hints here; the host
    // picks the address and its own page size.
    let _ = (MEM_TOP_DOWN, MEM_LARGE_PAGES);
    let mut map = reservations();
    let wanted = address as usize;
    let (base, start, len) = if kind & MEM_RESERVE != 0 {
        let at = wanted & !(GRANULARITY - 1);
        let len = (wanted + size).next_multiple_of(PAGE) - at;
        let Some(base) = reserve(at, len) else {
            set_last_error(if at != 0 {
                ERROR_INVALID_ADDRESS
            } else {
                ERROR_NOT_ENOUGH_MEMORY
            });
            return std::ptr::null_mut();
        };
        let pages = len / PAGE;
        map.insert(
            base,
            Reservation {
                size: len,
                pages: vec![0; pages],
                initial: protect,
            },
        );
        (base, base, len)
    } else {
        // Commit (or reset) inside a reservation.
        let start = wanted & !(PAGE - 1);
        let end = (wanted + size).next_multiple_of(PAGE);
        match holding(&mut map, start) {
            Some((base, r)) if end <= base + r.size => (base, start, end - start),
            _ => {
                set_last_error(ERROR_INVALID_ADDRESS);
                return std::ptr::null_mut();
            }
        }
    };
    if kind & MEM_COMMIT != 0 {
        // SAFETY: the range is inside the reservation found or made above.
        if unsafe { libc::mprotect(start as *mut _, len, prot) } != 0 {
            set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            return std::ptr::null_mut();
        }
        let r = map.get_mut(&base).expect("the reservation is in the map");
        let first = (start - base) / PAGE;
        r.pages[first..first + len / PAGE].fill(protect);
    } else if kind & MEM_RESET != 0 {
        // SAFETY: committed pages of ours whose contents the title gives up.
        unsafe { libc::madvise(start as *mut _, len, libc::MADV_FREE) };
    }
    if kind & MEM_RESERVE != 0 { base } else { start }.max(wanted) as *mut c_void
}

#[allow(non_snake_case)]
pub extern "win64" fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32 {
    let at = address as usize;
    let mut map = reservations();
    if kind & MEM_RELEASE != 0 {
        // Releases a whole reservation, given by its base, with size 0.
        if size != 0 || !map.contains_key(&at) {
            set_last_error(ERROR_INVALID_PARAMETER);
            return 0;
        }
        let r = map.remove(&at).expect("checked above");
        // SAFETY: the whole reservation, ours alone.
        unsafe { libc::munmap(at as *mut _, r.size) };
        return 1;
    }
    if kind & MEM_DECOMMIT == 0 {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let start = at & !(PAGE - 1);
    let Some((base, r)) = holding(&mut map, start) else {
        set_last_error(ERROR_INVALID_ADDRESS);
        return 0;
    };
    // Size 0 decommits from `address` to the end of the reservation.
    let end = if size == 0 {
        base + r.size
    } else {
        (at + size).next_multiple_of(PAGE)
    };
    if end > base + r.size {
        set_last_error(ERROR_INVALID_ADDRESS);
        return 0;
    }
    discard(start, end - start);
    let first = (start - base) / PAGE;
    r.pages[first..(end - base) / PAGE].fill(0);
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn VirtualProtect(
    address: *mut c_void,
    size: usize,
    protect: u32,
    old: *mut u32,
) -> i32 {
    let Some(prot) = host_protection(protect) else {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    };
    let start = address as usize & !(PAGE - 1);
    let end = (address as usize + size.max(1)).next_multiple_of(PAGE);
    let mut map = reservations();
    let Some((base, r)) = holding(&mut map, start) else {
        // Memory not ours (the title's image, a host mapping): protected all the same.
        // shortcut: its old protection reads as execute-read, whatever it was.
        // SAFETY: the title asks for its own memory.
        if unsafe { libc::mprotect(start as *mut _, end - start, prot) } != 0 {
            set_last_error(ERROR_INVALID_ADDRESS);
            return 0;
        }
        if !old.is_null() {
            // SAFETY: the title's out pointer.
            unsafe { *old = PAGE_EXECUTE_READ };
        }
        return 1;
    };
    let (first, last) = ((start - base) / PAGE, (end - base) / PAGE);
    if end > base + r.size || r.pages[first..last].contains(&0) {
        set_last_error(ERROR_INVALID_ADDRESS);
        return 0;
    }
    // SAFETY: committed pages of one of our reservations.
    if unsafe { libc::mprotect(start as *mut _, end - start, prot) } != 0 {
        set_last_error(ERROR_INVALID_ADDRESS);
        return 0;
    }
    if !old.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { *old = r.pages[first] };
    }
    r.pages[first..last].fill(protect);
    1
}

/// `MEMORY_BASIC_INFORMATION` (x64).
#[repr(C)]
pub struct BasicInformation {
    base: usize,
    allocation_base: usize,
    allocation_protect: u32,
    partition: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}

impl BasicInformation {
    /// Whether the region is committed memory.
    pub(super) fn committed(&self) -> bool {
        self.state == MEM_COMMIT
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn VirtualQuery(
    address: *const c_void,
    out: *mut BasicInformation,
    length: usize,
) -> usize {
    if out.is_null() || length < size_of::<BasicInformation>() {
        set_last_error(24); // ERROR_BAD_LENGTH
        return 0;
    }
    let at = address as usize & !(PAGE - 1);
    let mut map = reservations();
    let info = match holding(&mut map, at) {
        Some((base, r)) => {
            let first = (at - base) / PAGE;
            let protect = r.pages[first];
            let run = r.pages[first..]
                .iter()
                .take_while(|&&p| p == protect)
                .count();
            BasicInformation {
                base: at,
                allocation_base: base,
                allocation_protect: r.initial,
                partition: 0,
                region_size: run * PAGE,
                state: if protect == 0 {
                    MEM_RESERVE
                } else {
                    MEM_COMMIT
                },
                protect,
                kind: MEM_PRIVATE,
            }
        }
        // shortcut: memory not reserved through us (images, host mappings)
        // reads as free up to our next reservation.
        None => {
            let next = map.range(at..).next().map_or(0x7FFF_FFFF_0000, |(&b, _)| b);
            BasicInformation {
                base: at,
                allocation_base: 0,
                allocation_protect: 0,
                partition: 0,
                region_size: next.saturating_sub(at).max(PAGE),
                state: MEM_FREE,
                protect: PAGE_NOACCESS,
                kind: 0,
            }
        }
    };
    // SAFETY: the title's buffer, at least this large (checked above).
    unsafe { out.write(info) };
    size_of::<BasicInformation>()
}

// Heaps. Every heap is the process heap: blocks carry their size in a header.
// shortcut: HeapDestroy frees nothing and heaps are not separate; enough for
// titles that use heaps as allocators, not for ones that walk them.

const HEAP_ZERO_MEMORY: u32 = 0x08;
const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x10;
const HEADER: usize = 16;
/// The handle every heap has.
const PROCESS_HEAP: usize = 0x0005_0000;

fn layout(size: usize) -> Option<std::alloc::Layout> {
    std::alloc::Layout::from_size_align(size.checked_add(HEADER)?, 16).ok()
}

/// # Safety
/// `block` came from [`HeapAlloc`].
unsafe fn block_size(block: *mut u8) -> usize {
    // SAFETY: the header sits just before the block.
    unsafe { block.sub(HEADER).cast::<usize>().read() }
}

#[allow(non_snake_case)]
pub extern "win64" fn GetProcessHeap() -> usize {
    PROCESS_HEAP
}

#[allow(non_snake_case)]
pub extern "win64" fn HeapCreate(_options: u32, _initial: usize, _maximum: usize) -> usize {
    PROCESS_HEAP
}

#[allow(non_snake_case)]
pub extern "win64" fn HeapDestroy(_heap: usize) -> i32 {
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn HeapAlloc(_heap: usize, flags: u32, size: usize) -> *mut c_void {
    let Some(layout) = layout(size) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a non-zero size (the header).
    let raw = unsafe {
        if flags & HEAP_ZERO_MEMORY != 0 {
            std::alloc::alloc_zeroed(layout)
        } else {
            std::alloc::alloc(layout)
        }
    };
    if raw.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the header is the first HEADER bytes of the allocation.
    unsafe {
        raw.cast::<usize>().write(size);
        raw.add(HEADER).cast()
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn HeapFree(_heap: usize, _flags: u32, block: *mut c_void) -> i32 {
    if block.is_null() {
        return 1;
    }
    let block = block.cast::<u8>();
    // SAFETY: a block of ours, given back once.
    unsafe {
        let size = block_size(block);
        std::alloc::dealloc(block.sub(HEADER), layout(size).expect("it was allocated"));
    }
    1
}

#[allow(non_snake_case)]
pub extern "win64" fn HeapSize(_heap: usize, _flags: u32, block: *const c_void) -> usize {
    if block.is_null() {
        return usize::MAX;
    }
    // SAFETY: a block of ours.
    unsafe { block_size(block as *mut u8) }
}

#[allow(non_snake_case)]
pub extern "win64" fn HeapReAlloc(
    heap: usize,
    flags: u32,
    block: *mut c_void,
    size: usize,
) -> *mut c_void {
    if block.is_null() {
        return HeapAlloc(heap, flags, size);
    }
    let block = block.cast::<u8>();
    // SAFETY: a block of ours.
    let old = unsafe { block_size(block) };
    if flags & HEAP_REALLOC_IN_PLACE_ONLY != 0 {
        if size > old {
            return std::ptr::null_mut();
        }
        // SAFETY: shrinking in place: only the recorded size changes.
        unsafe { block.sub(HEADER).cast::<usize>().write(size) };
        return block.cast();
    }
    let new = HeapAlloc(heap, flags, size).cast::<u8>();
    if new.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: both blocks are ours and at least min(old, size) long.
    unsafe { std::ptr::copy_nonoverlapping(block, new, old.min(size)) };
    HeapFree(heap, 0, block.cast());
    new.cast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_commit_query_decommit_release() {
        let base = VirtualAlloc(std::ptr::null_mut(), 0x2_0000, MEM_RESERVE, PAGE_NOACCESS);
        assert!(!base.is_null());
        assert_eq!(base as usize % GRANULARITY, 0, "64 KiB aligned");

        let mut info = std::mem::MaybeUninit::<BasicInformation>::uninit();
        let n = size_of::<BasicInformation>();
        assert_eq!(VirtualQuery(base, info.as_mut_ptr(), n), n);
        // SAFETY: filled in above.
        let info = unsafe { info.assume_init() };
        assert_eq!((info.state, info.region_size), (MEM_RESERVE, 0x2_0000));

        let page = (base as usize + 0x3000) as *mut c_void;
        assert_eq!(VirtualAlloc(page, 0x100, MEM_COMMIT, PAGE_READWRITE), page);
        // SAFETY: committed read+write above.
        unsafe {
            assert_eq!(*(page as *const u64), 0, "committed pages are zero");
            *(page as *mut u64) = 7;
        }
        let mut old = 0;
        assert_eq!(VirtualProtect(page, 8, PAGE_READONLY, &mut old), 1);
        assert_eq!(old, PAGE_READWRITE);
        assert_eq!(
            VirtualProtect(base, 8, PAGE_READONLY, &mut old),
            0,
            "reserved pages cannot be protected"
        );

        assert_eq!(VirtualFree(page, 0x1000, MEM_DECOMMIT), 1);
        assert_eq!(VirtualAlloc(page, 0x1000, MEM_COMMIT, PAGE_READWRITE), page);
        // SAFETY: committed again.
        assert_eq!(
            unsafe { *(page as *const u64) },
            0,
            "decommitted contents are gone"
        );

        assert_eq!(
            VirtualFree(base, 0x1000, MEM_RELEASE),
            0,
            "release takes size 0"
        );
        assert_eq!(VirtualFree(base, 0, MEM_RELEASE), 1);
    }

    #[test]
    fn reserve_and_commit_at_once() {
        let at = VirtualAlloc(
            std::ptr::null_mut(),
            100,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_READWRITE,
        );
        assert!(!at.is_null());
        // SAFETY: committed read+write.
        unsafe { *(at as *mut u8).add(99) = 1 };
        assert_eq!(VirtualFree(at, 0, MEM_RELEASE), 1);
    }

    #[test]
    fn heap_blocks_keep_their_size_and_contents() {
        let heap = GetProcessHeap();
        let a = HeapAlloc(heap, HEAP_ZERO_MEMORY, 40).cast::<u8>();
        assert_eq!(HeapSize(heap, 0, a.cast()), 40);
        // SAFETY: a 40-byte zeroed block.
        unsafe {
            assert!(std::slice::from_raw_parts(a, 40).iter().all(|&b| b == 0));
            a.write_bytes(0xAB, 40);
        }
        assert!(HeapReAlloc(heap, HEAP_REALLOC_IN_PLACE_ONLY, a.cast(), 80).is_null());
        let b = HeapReAlloc(heap, 0, a.cast(), 80).cast::<u8>();
        assert_eq!(HeapSize(heap, 0, b.cast()), 80);
        // SAFETY: the first 40 bytes were carried over.
        assert!(
            unsafe { std::slice::from_raw_parts(b, 40) }
                .iter()
                .all(|&x| x == 0xAB)
        );
        assert_eq!(HeapFree(heap, 0, b.cast()), 1);
    }
}
