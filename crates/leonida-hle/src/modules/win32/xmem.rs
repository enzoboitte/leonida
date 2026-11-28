//! `xmem.dll` on macOS: console memory from host memory, with the same
//! contracts as `modules::xmem` on Windows (alignment, address ranges, 64
//! KiB physical pages a title maps at one or more addresses).
//!
//! A separate implementation, not that module made portable: Windows' is
//! tied to the GPU side (write-combined memory, write watches, remapping
//! texture views), which has no macOS back end yet, and it is the measured
//! memory core of the Windows build, which a Mac cannot run. Merge the two
//! once a Metal back end needs the same hooks.
//!
//! Physical pages are 64 KiB slices of host shared-memory objects; mapping
//! them is `mmap(MAP_SHARED | MAP_FIXED)` of the object at the page's
//! offset, so two addresses mapping one page see the same bytes.

use std::alloc::{Layout, alloc, dealloc};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::memory;

const MIB: usize = 1 << 20;
const PAGE: usize = 64 << 10;
/// Page numbers count 4 KiB frames (see `modules::xmem`).
const FRAMES: usize = PAGE >> 12;

const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_REPLACE_PLACEHOLDER: u32 = 0x4000;
const PAGE_NOACCESS: u32 = 0x01;
const PAGE_READWRITE: u32 = 0x04;
// Console additions to the host's flags, as in `modules::xmem`.
const PAGES_2M: u32 = 0x2000_0000;
const PAGES_64K_BIT: u32 = 0x40_0000;
const PAGES_64K: u32 = PAGES_2M | PAGES_64K_BIT;
const HOST_PROTECTION: u32 = 0x1ff;
const GRAPHICS: u64 = 3;
const ADDRESS_32BIT: u64 = 4;
const ALIGNMENT_SHIFT: u64 = 24;
const ALLOC_32BIT: u64 = 1 << 33;
const HEADER: usize = 2 * size_of::<usize>();

/// The addresses XMem `flags` allow, as `modules::xmem::address_range`.
fn address_range(flags: u64) -> (usize, usize) {
    let bits = match (flags & ADDRESS_32BIT, flags >> 12 & 7) {
        (0, 0) => 40,
        (0, field) => 47 - field,
        _ => return (0x1_0000, u32::MAX as usize),
    };
    (1 << 32, (1 << bits) - 1)
}

#[allow(non_snake_case)]
pub extern "win64" fn XMemVirtualAlloc(
    base: *mut c_void,
    size: usize,
    kind: u32,
    flags: u64,
    protect: u32,
) -> *mut c_void {
    let protect = match protect & HOST_PROTECTION {
        0 => PAGE_NOACCESS,
        p => p,
    };
    // A reservation of 64 KiB pages takes physical pages later.
    let placeholder = kind & PAGES_64K == PAGES_64K && kind & MEM_COMMIT == 0;
    if placeholder && kind & MEM_REPLACE_PLACEHOLDER != 0 {
        return base;
    }
    let kind = kind & !(PAGES_2M | PAGES_64K_BIT);
    if !base.is_null() {
        return memory::VirtualAlloc(base, size, kind, protect);
    }
    let align = if kind & PAGES_2M != 0 || flags & ADDRESS_32BIT == 0 && size >= 2 * MIB {
        2 * MIB
    } else {
        0
    };
    let (low, high) = address_range(flags);
    let commit = kind & MEM_COMMIT != 0 && !placeholder;
    let protect = if placeholder { PAGE_NOACCESS } else { protect };
    match memory::reserve_in(size, align, low, high, protect, commit) {
        Some(at) => at as *mut c_void,
        None => {
            tracing::warn!(
                size,
                flags = format_args!("{flags:#x}"),
                "XMemVirtualAlloc failed"
            );
            std::ptr::null_mut()
        }
    }
}

/// `MEMORY_BASIC_INFORMATION`, then the XMem flags and tag.
#[repr(C)]
pub struct BasicInformation {
    host: memory::BasicInformation,
    flags: u64,
    tag_code: u32,
    tag: [u8; 4],
}

#[allow(non_snake_case)]
pub extern "win64" fn XMemVirtualQuery(
    _process: usize,
    address: *const c_void,
    out: *mut BasicInformation,
) -> i32 {
    if out.is_null() {
        return 0;
    }
    let n = size_of::<memory::BasicInformation>();
    // SAFETY: the title passes room for the whole structure.
    unsafe {
        if memory::VirtualQuery(address, out.cast(), n) == 0 {
            return 0;
        }
        let info = &mut *out;
        info.flags = if info.host.committed() { GRAPHICS } else { 0 };
        info.tag_code = 0;
        info.tag = [0; 4];
    }
    1
}

fn layout(size: usize, attributes: u64) -> Option<Layout> {
    let align = 1usize << (attributes >> ALIGNMENT_SHIFT & 0x1f).max(4);
    Layout::from_size_align(size.checked_add(align)?, align).ok()
}

type AllocRoutine = extern "win64" fn(usize, u64) -> *mut c_void;
type FreeRoutine = extern "win64" fn(*mut c_void, u64);

/// Where the title's exe keeps `__XMemAllocationHooks__`: two routine
/// pointers `XMemAlloc` and `XMemFree` call (see `modules::xmem`).
static HOOKS: AtomicUsize = AtomicUsize::new(0);

/// Called by the runner with the title's export, once it is mapped.
pub fn set_allocation_hooks(at: usize) {
    tracing::info!(found = at != 0, "xmem allocation hooks");
    HOOKS.store(at, Ordering::Relaxed);
}

fn hooks() -> [usize; 2] {
    match HOOKS.load(Ordering::Relaxed) {
        0 => [0; 2],
        // SAFETY: the export is two routine pointers.
        at => unsafe { (at as *const [usize; 2]).read() },
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn XMemAlloc(size: usize, attributes: u64) -> *mut c_void {
    match hooks()[0] {
        0 => XMemAllocDefault(size, attributes),
        hook => {
            // SAFETY: the title's own routine of this type.
            let alloc: AllocRoutine = unsafe { std::mem::transmute(hook) };
            alloc(size, attributes)
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn XMemFree(address: *mut c_void, attributes: u64) {
    match hooks()[1] {
        0 => XMemFreeDefault(address, attributes),
        hook => {
            // SAFETY: the title's own routine of this type.
            let free: FreeRoutine = unsafe { std::mem::transmute(hook) };
            free(address, attributes)
        }
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn XMemAllocDefault(size: usize, attributes: u64) -> *mut c_void {
    if attributes & ALLOC_32BIT != 0 {
        return XMemVirtualAlloc(
            std::ptr::null_mut(),
            size,
            MEM_RESERVE | MEM_COMMIT,
            GRAPHICS | ADDRESS_32BIT,
            PAGE_READWRITE,
        );
    }
    let Some(layout) = layout(size, attributes) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the layout has a non-zero size.
    let block = unsafe { alloc(layout) };
    if block.is_null() {
        return block.cast();
    }
    // SAFETY: the block holds the alignment (at least the header), then `size` bytes.
    unsafe {
        let user = block.add(layout.align());
        user.sub(HEADER)
            .cast::<[usize; 2]>()
            .write([layout.size(), layout.align()]);
        user.cast()
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn XMemFreeDefault(address: *mut c_void, attributes: u64) {
    if address.is_null() {
        return;
    }
    if attributes & ALLOC_32BIT != 0 {
        memory::VirtualFree(address, 0, 0x8000);
        return;
    }
    // SAFETY: a header-prefixed block of `XMemAllocDefault`.
    unsafe {
        let user = address.cast::<u8>();
        let [size, align] = user.sub(HEADER).cast::<[usize; 2]>().read();
        dealloc(
            user.sub(align),
            Layout::from_size_align_unchecked(size, align),
        );
    }
}

#[allow(non_snake_case)]
pub extern "win64" fn XMemIsToolsMemAvailable() -> i32 {
    0
}

/// A host shared-memory object, closed when no page or view needs it.
struct Section(i32);

impl Drop for Section {
    fn drop(&mut self) {
        // SAFETY: our descriptor; mappings keep the object alive on their own.
        unsafe { libc::close(self.0) };
    }
}

/// A shared-memory object of `size` bytes, with no name left behind.
fn section(size: usize) -> Option<Section> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let name = format!(
        "/leonida-{}-{}\0",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    // SAFETY: a NUL-terminated name; the object is unlinked at once, so only
    // our descriptor (and the mappings of it) keep it.
    unsafe {
        let fd = libc::shm_open(
            name.as_ptr().cast(),
            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
            0o600,
        );
        if fd < 0 {
            return None;
        }
        libc::shm_unlink(name.as_ptr().cast());
        let section = Section(fd);
        (libc::ftruncate(fd, size as i64) == 0).then_some(section)
    }
}

/// Where a page lives: a section and its 64 KiB index there.
#[derive(Clone)]
struct Slice {
    section: Arc<Section>,
    index: usize,
}

struct Catalog {
    next: usize,
    pages: BTreeMap<usize, Slice>,
}

/// The first page number given out; 0 stays unused.
const FIRST_PAGE: usize = 0x10_0000;
/// Numbers from here on do not survive the title's 21-bit `number >> 4`.
const PAGE_LIMIT: usize = 1 << 25;

static CATALOG: Mutex<Catalog> = Mutex::new(Catalog {
    next: FIRST_PAGE,
    pages: BTreeMap::new(),
});

// After trustno1-coder/VIctory-NVIDIA 108a897, a port of
// avatarmanz/VIctory-7900XTX#316: numbers only grew, so past about 124 GiB
// allocated over a run one came back truncated to the title.
impl Catalog {
    /// The first of `n` numbers, [`FRAMES`] apart, for new pages: after the
    /// last given while they fit the title's 21 bits, then the lowest run of
    /// `n` freed ones. `None` if there is no such run.
    fn take(&mut self, n: usize) -> Option<usize> {
        let len = n.checked_mul(FRAMES)?;
        if let Some(end) = self.next.checked_add(len).filter(|&end| end <= PAGE_LIMIT) {
            let first = self.next;
            self.next = end;
            return Some(first);
        }
        free_run(self.pages.keys().copied(), len)
    }
}

/// The lowest start from [`FIRST_PAGE`] where `len` numbers (a multiple of
/// [`FRAMES`]) up to [`PAGE_LIMIT`] are all outside `used` (in order).
fn free_run(used: impl IntoIterator<Item = usize>, len: usize) -> Option<usize> {
    let mut start = FIRST_PAGE;
    for at in used.into_iter().chain([PAGE_LIMIT]) {
        if at.checked_sub(start).is_some_and(|gap| gap >= len) {
            return Some(start);
        }
        start = start.max(at.checked_add(FRAMES)?);
    }
    None
}

fn catalog() -> MutexGuard<'static, Catalog> {
    CATALOG.lock().unwrap_or_else(|e| e.into_inner())
}

/// `count` is in and out (pages wanted, pages given); all or nothing here.
#[allow(non_snake_case)]
pub extern "win64" fn XMemAllocatePhysicalPages(
    _flags: u64,
    count: *mut usize,
    pages: *mut usize,
) -> i32 {
    if count.is_null() || pages.is_null() {
        return 0;
    }
    // SAFETY: checked non-null.
    let n = unsafe { *count };
    let Some(section) = n.checked_mul(PAGE).filter(|&s| s > 0).and_then(section) else {
        tracing::warn!(n, "XMemAllocatePhysicalPages failed");
        // SAFETY: checked non-null.
        unsafe { count.write(0) };
        return 0;
    };
    let section = Arc::new(section);
    let mut catalog = catalog();
    let Some(first) = catalog.take(n) else {
        tracing::warn!(
            n,
            "XMemAllocatePhysicalPages: no page numbers the title can keep"
        );
        // SAFETY: checked non-null.
        unsafe { count.write(0) };
        return 0;
    };
    for index in 0..n {
        let number = first + index * FRAMES;
        catalog.pages.insert(
            number,
            Slice {
                section: section.clone(),
                index,
            },
        );
        // SAFETY: the title passes room for `n` page numbers.
        unsafe { pages.add(index).write(number) };
    }
    1
}

/// Pages still mapped stay readable through their mappings until unmapped.
#[allow(non_snake_case)]
pub extern "win64" fn XMemFreePhysicalPages(n: usize, pages: *const usize) -> i32 {
    if pages.is_null() {
        return 0;
    }
    let mut catalog = catalog();
    for i in 0..n {
        // SAFETY: the title passes `n` page numbers.
        catalog.pages.remove(&unsafe { *pages.add(i) });
    }
    1
}

/// The slices behind `pages`, or `None` if one is unknown.
fn slices(pages: &[usize]) -> Option<Vec<Slice>> {
    let catalog = catalog();
    pages
        .iter()
        .map(|p| catalog.pages.get(p).cloned())
        .collect()
}

/// Whether every page holding `offset..offset + len` is known.
pub(crate) fn pages_known(pages: &[usize], offset: usize, len: usize) -> bool {
    let n = (offset + len).div_ceil(PAGE).min(pages.len());
    slices(&pages[..n]).is_some()
}

/// A view of our own over consecutive pages of one section, unmapped when
/// dropped: DirectStorage writes its destinations through it.
pub(crate) struct PageWindow {
    at: usize,
    map_len: usize,
    from: usize,
    len: usize,
}

impl PageWindow {
    pub(crate) fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: the view holds `from + len` read-write bytes, ours until dropped.
        unsafe { std::slice::from_raw_parts_mut((self.at + self.from) as *mut u8, self.len) }
    }
}

impl Drop for PageWindow {
    fn drop(&mut self) {
        // SAFETY: the view made in `page_window`.
        unsafe { libc::munmap(self.at as *mut _, self.map_len) };
    }
}

/// One view over `offset..offset + len` of `pages`, if they are consecutive
/// pages of one section.
pub(crate) fn page_window(pages: &[usize], offset: usize, len: usize) -> Option<PageWindow> {
    let n = (offset + len).div_ceil(PAGE);
    let s = slices(pages.get(..n)?)?;
    let first = s.first()?;
    let consecutive = s
        .iter()
        .enumerate()
        .all(|(k, x)| Arc::ptr_eq(&x.section, &first.section) && x.index == first.index + k);
    if !consecutive {
        return None;
    }
    let map_len = n * PAGE;
    let prot = libc::PROT_READ | libc::PROT_WRITE;
    // SAFETY: a fresh shared view of the section's pages.
    let at = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            map_len,
            prot,
            libc::MAP_SHARED,
            first.section.0,
            (first.index * PAGE) as i64,
        )
    };
    (at != libc::MAP_FAILED).then(|| PageWindow {
        at: at as usize,
        map_len,
        from: offset,
        len,
    })
}

/// Writes `data` into `pages` from `offset`, page by page; whether all of
/// it was written.
pub(crate) fn write_physical(pages: &[usize], offset: usize, data: &[u8]) -> bool {
    let n = (offset + data.len()).div_ceil(PAGE);
    let Some(s) = pages.get(..n).and_then(slices) else {
        return false;
    };
    let mut done = 0;
    for (k, slice) in s.iter().enumerate() {
        let start = if k == 0 { offset } else { 0 };
        let take = (PAGE - start).min(data.len() - done);
        let Some(mut w) = page_window(&pages[k..=k], start, take) else {
            return false;
        };
        let _ = slice;
        w.bytes().copy_from_slice(&data[done..done + take]);
        done += take;
    }
    done == data.len()
}

/// What the GPU side is told of pages written; it has nothing to learn
/// on macOS yet.
pub(crate) fn pages_written(_pages: &[usize], _offset: usize, _len: usize) {}

/// Shows `slice` at `at`, or nothing (no access) when `None`.
fn show(at: usize, slice: Option<&Slice>) -> bool {
    let (prot, flags, fd, offset) = match slice {
        Some(s) => (
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_FIXED,
            s.section.0,
            (s.index * PAGE) as i64,
        ),
        None => (
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED | libc::MAP_NORESERVE,
            -1,
            0,
        ),
    };
    // SAFETY: `at` is a page of a reservation the title made for these
    // pages; MAP_FIXED replaces only that page.
    let got = unsafe { libc::mmap(at as *mut _, PAGE, prot, flags, fd, offset) };
    let ok = got as usize == at;
    if ok {
        memory::mark(at, PAGE, if slice.is_some() { PAGE_READWRITE } else { 0 });
    }
    ok
}

/// Maps `n` pages at `address` (no pages: unmaps them).
#[allow(non_snake_case)]
pub extern "win64" fn XMemMapPhysicalPages(
    address: *mut c_void,
    n: usize,
    pages: *const usize,
) -> *mut c_void {
    let base = address as usize;
    let slices: Vec<Option<Slice>> = if pages.is_null() {
        vec![None; n]
    } else {
        let catalog = catalog();
        // SAFETY: the title passes `n` page numbers.
        let numbers = unsafe { std::slice::from_raw_parts(pages, n) };
        let mut slices = Vec::with_capacity(n);
        for page in numbers {
            match catalog.pages.get(page) {
                Some(s) => slices.push(Some(s.clone())),
                None => {
                    tracing::warn!(page, "XMemMapPhysicalPages: unknown page");
                    return std::ptr::null_mut();
                }
            }
        }
        slices
    };
    for (i, slice) in slices.iter().enumerate() {
        if !show(base + i * PAGE, slice.as_ref()) {
            return std::ptr::null_mut();
        }
    }
    address
}

#[allow(non_snake_case)]
pub extern "win64" fn XMemMapPhysicalPagesScatter(
    addresses: *const *mut c_void,
    n: usize,
    pages: *const usize,
) -> i32 {
    if addresses.is_null() {
        return 0;
    }
    for i in 0..n {
        // SAFETY: the title passes `n` addresses, and `n` pages if any (0: unmap).
        let (address, page) = unsafe {
            let page = if pages.is_null() || *pages.add(i) == 0 {
                std::ptr::null()
            } else {
                pages.add(i)
            };
            (*addresses.add(i), page)
        };
        if XMemMapPhysicalPages(address, 1, page).is_null() {
            return 0;
        }
    }
    1
}

/// As `modules::xmem::WorkingSet`.
#[repr(C)]
#[derive(Default)]
pub struct WorkingSet {
    pub title_limit: usize,
    pub title_used: usize,
    pub tools_limit: usize,
    pub tools_used: usize,
    pub stack_used: u32,
    pub physical_pages: u32,
    pub gpu_optimal_limit: usize,
    pub gpu_optimal_used: usize,
    pub tools_gpu_optimal_limit: usize,
    pub tools_gpu_optimal_used: usize,
    pub kernel_pool_used: u32,
    pub kernel_pool_limit: u32,
}

/// Standard memory the title has handed to the system.
static TRANSFERRED: AtomicUsize = AtomicUsize::new(0);

/// shortcut: "used" is the process's peak resident size (macOS reports it
/// in bytes), not the title's own memory alone.
#[allow(non_snake_case)]
pub extern "win64" fn XMemGetWorkingSetStatistics(_which: u32, out: *mut WorkingSet) -> i32 {
    if out.is_null() {
        return 0;
    }
    // SAFETY: plain data filled in by the call.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let (gpu_optimal, standard) = leonida_gpu::console::current().title_memory();
    let stats = WorkingSet {
        title_limit: gpu_optimal + standard - TRANSFERRED.load(Ordering::Relaxed),
        title_used: usage.ru_maxrss as usize,
        gpu_optimal_limit: gpu_optimal,
        ..Default::default()
    };
    // SAFETY: checked non-null; the title passes room for the structure.
    unsafe { out.write(stats) };
    1
}

/// Hands title memory (standard, in 2 MiB pages) to the system.
#[allow(non_snake_case)]
pub extern "win64" fn XMemTransferMemory(kind: u32, size: usize) -> i32 {
    const TITLE_TO_SYSTEM: u32 = 0;
    if size == 0 {
        return 1;
    }
    let Some(size) = size.checked_next_multiple_of(2 * MIB) else {
        return 0;
    };
    let standard = leonida_gpu::console::current().title_memory().1;
    let moved = kind == TITLE_TO_SYSTEM
        && TRANSFERRED
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |t| {
                t.checked_add(size).filter(|&t| t <= standard)
            })
            .is_ok();
    moved.into()
}

pub fn module() -> crate::HleModule {
    use crate::hle_export;
    crate::HleModule {
        name: "xmem",
        dlls: &["xmem.dll"],
        exports: vec![
            hle_export!(XMemAlloc),
            hle_export!(XMemFree),
            hle_export!(XMemAllocDefault),
            hle_export!(XMemFreeDefault),
            hle_export!(XMemIsToolsMemAvailable),
            hle_export!(XMemAllocatePhysicalPages),
            hle_export!(XMemFreePhysicalPages),
            hle_export!(XMemMapPhysicalPages),
            hle_export!(XMemMapPhysicalPagesScatter),
            hle_export!(XMemVirtualAlloc),
            hle_export!(XMemVirtualQuery),
            hle_export!(XMemGetWorkingSetStatistics),
            hle_export!(XMemTransferMemory),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocations_honour_alignment_and_address_ranges() {
        let p = XMemAllocDefault(100, 12 << ALIGNMENT_SHIFT).cast::<u8>();
        assert!((p as usize).is_multiple_of(4096), "2^12 alignment");
        // SAFETY: 100 writable bytes.
        unsafe { p.write_bytes(1, 100) };
        XMemFreeDefault(p.cast(), 12 << ALIGNMENT_SHIFT);

        let low = XMemAllocDefault(4096, ALLOC_32BIT) as usize;
        assert!(
            low != 0 && low + 4096 <= u32::MAX as usize,
            "{low:#x} below 4 GiB"
        );
        XMemFreeDefault(low as *mut c_void, ALLOC_32BIT);

        let p = XMemVirtualAlloc(
            std::ptr::null_mut(),
            4 * MIB,
            MEM_RESERVE | MEM_COMMIT | PAGES_2M,
            0,
            PAGE_READWRITE,
        );
        let at = p as usize;
        assert!(
            at.is_multiple_of(2 * MIB) && at >= 1 << 32 && at + 4 * MIB < 1 << 40,
            "{at:#x}"
        );
        let mut info = std::mem::MaybeUninit::<BasicInformation>::uninit();
        assert_eq!(XMemVirtualQuery(0, p, info.as_mut_ptr()), 1);
        // SAFETY: filled in above.
        assert_eq!(unsafe { info.assume_init_ref() }.flags, GRAPHICS);
    }

    #[test]
    fn a_physical_page_mapped_twice_shows_the_same_bytes() {
        let (mut n, mut pages) = (2usize, [0usize; 2]);
        assert_eq!(XMemAllocatePhysicalPages(0, &mut n, pages.as_mut_ptr()), 1);
        assert_eq!(
            pages[1] - pages[0],
            FRAMES,
            "page numbers count 4 KiB frames"
        );
        let reserve = |size| {
            XMemVirtualAlloc(
                std::ptr::null_mut(),
                size,
                MEM_RESERVE | PAGES_64K,
                0,
                PAGE_READWRITE,
            )
        };
        let (a, b) = (reserve(2 * PAGE), reserve(PAGE));
        assert!(!a.is_null() && !b.is_null());
        assert_eq!(XMemMapPhysicalPages(a, 2, pages.as_ptr()), a);
        assert_eq!(XMemMapPhysicalPages(b, 1, pages[1..].as_ptr()), b);
        // SAFETY: both mapped read+write above.
        unsafe {
            *(a as *mut u8).add(PAGE + 7) = 0x5A;
            assert_eq!(*(b as *const u8).add(7), 0x5A, "one page, two addresses");
        }
        let mut info = std::mem::MaybeUninit::<BasicInformation>::uninit();
        XMemVirtualQuery(0, b, info.as_mut_ptr());
        // SAFETY: filled in above.
        assert_eq!(
            unsafe { info.assume_init_ref() }.flags,
            GRAPHICS,
            "mapped reads as committed"
        );
        assert_eq!(XMemMapPhysicalPages(b, 1, std::ptr::null()), b, "unmapped");
        XMemVirtualQuery(0, b, info.as_mut_ptr());
        // SAFETY: filled in above.
        assert_eq!(unsafe { info.assume_init_ref() }.flags, 0);
        assert_eq!(XMemFreePhysicalPages(2, pages.as_ptr()), 1);
        // SAFETY: still mapped at `a`: freed pages stay readable there.
        assert_eq!(unsafe { *(a as *const u8).add(PAGE + 7) }, 0x5A);
    }

    #[test]
    fn page_numbers_freed_are_given_again_past_the_title_s_21_bits() {
        let mut table = Catalog {
            next: PAGE_LIMIT - 2 * FRAMES,
            pages: BTreeMap::new(),
        };
        assert_eq!(
            table.take(2),
            Some(PAGE_LIMIT - 2 * FRAMES),
            "the last that fit"
        );
        assert_eq!(table.take(3), Some(FIRST_PAGE), "then the lowest free run");
        assert_eq!(table.take(usize::MAX), None);
        let f = |k: usize| FIRST_PAGE + k * FRAMES;
        assert_eq!(free_run([f(0), f(1), f(3)], FRAMES), Some(f(2)));
        assert_eq!(free_run([f(0), f(1), f(3)], 2 * FRAMES), Some(f(4)));
        assert_eq!(free_run([f(0)], PAGE_LIMIT - FIRST_PAGE), None);
        let p = free_run([PAGE_LIMIT - FRAMES], FRAMES).unwrap();
        assert_eq!((p >> 4 & 0x1f_ffff) << 4, p, "kept whole in 21 bits");
    }

    #[test]
    fn hooks_take_over_xmem_alloc() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        extern "win64" fn alloc(size: usize, _: u64) -> *mut c_void {
            CALLS.fetch_add(1, Ordering::Relaxed);
            size as *mut c_void
        }
        extern "win64" fn free(_: *mut c_void, _: u64) {
            CALLS.fetch_add(10, Ordering::Relaxed);
        }
        let table: &'static [usize; 2] = Box::leak(Box::new([
            alloc as *const () as usize,
            free as *const () as usize,
        ]));
        set_allocation_hooks(table.as_ptr() as usize);
        assert_eq!(XMemAlloc(77, 0) as usize, 77);
        XMemFree(std::ptr::null_mut(), 0);
        assert_eq!(CALLS.load(Ordering::Relaxed), 11);
        set_allocation_hooks(0);
    }
}
