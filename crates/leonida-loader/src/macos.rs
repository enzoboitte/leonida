//! Mapping a title image ourselves, on macOS.
//!
//! There is no Windows loader to lean on, so this does its part of the
//! work: map the sections, apply the base relocations, bind the imports
//! (through the same resolver and stubs as on Windows), and give every
//! thread that runs title code a TEB and static TLS where Win64 code looks
//! for them. The process is x86-64 (Rosetta 2 on Apple Silicon), so the
//! title's code runs as it is. Measured in
//! `docs/notes/2026-11-27-macos-loader.md`.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Mutex;

use anyhow::{Context, Result, bail, ensure};
use leonida_pe::{ImageInfo, layout};

use crate::thunks::{UnimplementedPolicy, resolve_imports};
use crate::{BindReport, ImportResolver};

const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const SCN_MEM_READ: u32 = 0x4000_0000;
const SCN_MEM_WRITE: u32 = 0x8000_0000;
const REL_BASED_ABSOLUTE: u16 = 0;
const REL_BASED_DIR64: u16 = 10;
/// `IMAGE_TLS_DIRECTORY64`: StartAddressOfRawData, EndAddressOfRawData,
/// AddressOfIndex (VAs) and SizeOfZeroFill.
const TLS_START: usize = 0;
const TLS_END: usize = 8;
const TLS_INDEX: usize = 16;
const TLS_ZERO_FILL: usize = 32;
/// TEB fields: `NtTib.Self` and `ThreadLocalStoragePointer`.
const TEB_SELF: usize = 0x30;
const TEB_TLS_POINTER: usize = 0x58;
/// More than a Windows 10 TEB (0x1838 bytes), so stray reads stay inside.
const TEB_SIZE: usize = 0x2000;
/// Slots of a thread's TLS array, one per image with static TLS: the exe's
/// is slot 0, as Windows gives it, DLLs' the next free ones.
const TLS_SLOTS: usize = 64;
const DLL_PROCESS_ATTACH: u32 = 1;
const DLL_THREAD_ATTACH: u32 = 2;
const DLL_THREAD_DETACH: u32 = 3;
const PAGE: usize = 0x1000;

/// A title image mapped into this process, relocated and bound.
#[derive(Debug, Clone)]
pub struct MappedImage {
    base: usize,
    info: ImageInfo,
    /// Its slot in every thread's TLS array, if it has static TLS.
    tls_index: Option<usize>,
}

impl MappedImage {
    /// Maps `bytes` (a PE32+ image as stored on disk), at its preferred base
    /// when that is free, and binds its imports through `resolver`. Runs none
    /// of its code. The mapping lives as long as the process.
    pub fn load(
        bytes: &[u8],
        resolver: &dyn ImportResolver,
        policy: UnimplementedPolicy,
    ) -> Result<(Self, BindReport)> {
        let info = ImageInfo::parse(bytes)?;
        let size = info.size_of_image as usize;
        let base = map(info.image_base, size)?;
        // SAFETY: `map` returned `size` fresh, writable bytes, ours alone until
        // `protect` below.
        let image = unsafe { std::slice::from_raw_parts_mut(base as *mut u8, size) };

        let headers = info
            .sections
            .iter()
            .map(|s| s.file_offset as usize)
            .filter(|&o| o != 0)
            .min()
            .unwrap_or(bytes.len())
            .min(bytes.len())
            .min(size);
        image[..headers].copy_from_slice(&bytes[..headers]);
        for s in &info.sections {
            let len = if s.virtual_size == 0 {
                s.file_size
            } else {
                s.file_size.min(s.virtual_size)
            };
            let (from, to, len) = (s.file_offset as usize, s.rva as usize, len as usize);
            let src = bytes
                .get(from..from + len)
                .context("section past the end of the file")?;
            let dst = image
                .get_mut(to..to + len)
                .context("section past the image's size")?;
            dst.copy_from_slice(src);
        }

        let delta = (base as u64).wrapping_sub(info.image_base);
        if delta != 0 {
            ensure!(
                info.has_relocations,
                "the image could not be mapped at {:#x} and has no relocations",
                info.image_base
            );
            relocate(bytes, image, delta)?;
        }
        let mut mapped = MappedImage {
            base,
            info,
            tls_index: None,
        };
        let index_at = match mapped.tls_field(TLS_INDEX) {
            Some(va) => Some(mapped.rva_of(va).context("TLS index outside the image")?),
            None => None,
        };
        if let Some(at) = index_at {
            let index = reserve_tls_index(mapped.info.is_dll())?;
            image
                .get_mut(at..at + 4)
                .context("TLS index outside the image")?
                .copy_from_slice(&(index as u32).to_le_bytes());
            mapped.tls_index = Some(index);
        }

        let (slots, report) = resolve_imports(&mapped.info, resolver, policy)?;
        for (rva, address) in slots {
            let at = rva as usize;
            let slot = image
                .get_mut(at..at + 8)
                .context("IAT slot outside the image")?;
            slot.copy_from_slice(&(address as u64).to_le_bytes());
        }
        mapped.protect()?;
        let pdata = layout::data_directory_offset(bytes, layout::dir::EXCEPTION)
            .and_then(|at| {
                Some((
                    layout::read_u32(bytes, at)?,
                    layout::read_u32(bytes, at + 4)?,
                ))
            })
            .filter(|&(rva, len)| rva as u64 + len as u64 <= size as u64);
        let (rva, len) = pdata.unwrap_or((0, 0));
        crate::seh::register(base, size, base + rva as usize, len as usize / 12);
        registered(&mapped);
        tracing::info!(
            base = format_args!("{base:#x}"),
            hle = report.hle,
            host = report.host,
            missing = report.missing.len(),
            "image mapped and bound"
        );
        Ok((mapped, report))
    }

    pub fn base(&self) -> usize {
        self.base
    }

    pub fn info(&self) -> &ImageInfo {
        &self.info
    }

    /// The address of an export, by name (`None` for forwarders).
    pub fn export(&self, name: &str) -> Option<usize> {
        let export = self
            .info
            .exports
            .iter()
            .find(|e| e.name.as_deref() == Some(name))?;
        export.rva.map(|rva| self.base + rva as usize)
    }

    /// The image's entry point, if it has one.
    pub fn entry_point(&self) -> Option<usize> {
        (self.info.entry_rva != 0).then(|| self.base + self.info.entry_rva as usize)
    }

    /// Initialises a DLL as the Windows loader would: its TLS callbacks,
    /// then `DllMain(DLL_PROCESS_ATTACH)`. From then on it hears of every
    /// thread [`spawn_thread`] starts and ends.
    ///
    /// # Safety
    /// Runs title code on the calling thread, which must have called
    /// [`enter_thread`]. The DLLs this one depends on must be attached.
    pub unsafe fn attach(&self) -> Result<()> {
        // SAFETY: forwarded from the caller.
        if unsafe { self.notify(DLL_PROCESS_ATTACH, true) } == Some(0) {
            bail!("DllMain returned FALSE");
        }
        attached(self.base);
        Ok(())
    }

    /// Starts the title: on a new thread with the stack size the image asks
    /// for, attaches `dlls` in order, runs the image's TLS callbacks
    /// (`DLL_PROCESS_ATTACH`), then its entry point. The thread's result is
    /// what the entry point returned.
    ///
    /// # Safety
    /// Runs title code. The imports of the image and of `dlls` are bound.
    pub unsafe fn run_main(
        &self,
        dlls: Vec<MappedImage>,
    ) -> Result<std::thread::JoinHandle<Result<u32>>> {
        let entry = self.entry_point().context("image has no entry point")?;
        let stack = usize::try_from(self.info.stack_reserve)
            .unwrap_or(usize::MAX)
            .max(1 << 20);
        let main = self.clone();
        std::thread::Builder::new()
            .name("title-main".into())
            .stack_size(stack)
            .spawn(move || {
                enter_thread();
                for dll in &dlls {
                    // SAFETY: forwarded from the caller of `run_main`.
                    unsafe { dll.attach() }
                        .with_context(|| format!("attaching {:#x}", dll.base))?;
                }
                // SAFETY: forwarded from the caller of `run_main`.
                unsafe { main.notify(DLL_PROCESS_ATTACH, false) };
                attached(main.base);
                type Entry = unsafe extern "win64" fn(*mut c_void) -> u32;
                // SAFETY: the entry RVA came from the image header.
                let entry: Entry = unsafe { std::mem::transmute(entry) };
                // SAFETY: forwarded from the caller of `run_main`.
                Ok(unsafe { entry(std::ptr::null_mut()) })
            })
            .context("spawning the title's main thread")
    }

    /// Runs the image's TLS callbacks with `reason`, then, if `dll_main` and
    /// the image is a DLL with an entry point, its `DllMain`, whose result is
    /// returned.
    ///
    /// # Safety
    /// Runs title code on a thread that called [`enter_thread`].
    unsafe fn notify(&self, reason: u32, dll_main: bool) -> Option<i32> {
        type Notice = unsafe extern "win64" fn(*mut c_void, u32, *mut c_void) -> i32;
        let module = self.base as *mut c_void;
        for &rva in self.info.tls.iter().flat_map(|t| &t.callback_rvas) {
            // SAFETY: the RVA came from the image's own TLS directory. A TLS
            // callback returns nothing; reading rax is harmless.
            let callback: Notice = unsafe { std::mem::transmute(self.base + rva as usize) };
            // SAFETY: forwarded from the caller.
            unsafe { callback(module, reason, std::ptr::null_mut()) };
        }
        if !dll_main || !self.info.is_dll() {
            return None;
        }
        let entry = self.entry_point()?;
        // SAFETY: the entry RVA came from the image header.
        let main: Notice = unsafe { std::mem::transmute(entry) };
        // SAFETY: forwarded from the caller.
        Some(unsafe { main(module, reason, std::ptr::null_mut()) })
    }

    fn rva_of(&self, va: u64) -> Option<usize> {
        let rva = va.checked_sub(self.base as u64)? as usize;
        (rva < self.info.size_of_image as usize).then_some(rva)
    }

    /// A u64 field of the TLS directory, as mapped (VAs already relocated).
    fn tls_field(&self, offset: usize) -> Option<u64> {
        let tls = self.info.tls.as_ref()?;
        let at = tls.directory_rva as usize + offset;
        (at + 8 <= self.info.size_of_image as usize)
            // SAFETY: inside the mapping, which stays readable for the process's life.
            .then(|| unsafe { ((self.base + at) as *const u64).read_unaligned() })
    }

    /// The image's TLS template followed by its zero fill: one thread's block.
    fn tls_block(&self) -> Option<Vec<u8>> {
        let start = self.rva_of(self.tls_field(TLS_START)?)?;
        let end = self
            .rva_of(self.tls_field(TLS_END)?)
            .unwrap_or(start)
            .max(start);
        let zero_fill = self.tls_field(TLS_ZERO_FILL)? as u32 as usize;
        // SAFETY: `start..end` is inside the mapping (checked by `rva_of`).
        let template =
            unsafe { std::slice::from_raw_parts((self.base + start) as *const u8, end - start) };
        let mut block = template.to_vec();
        block.resize(block.len() + zero_fill, 0);
        Some(block)
    }

    /// Gives each section the protection it asks for; the headers become
    /// read-only.
    fn protect(&self) -> Result<()> {
        protect(self.base, PAGE, libc::PROT_READ)?;
        for s in &self.info.sections {
            let c = s.characteristics;
            let mut prot = 0;
            if c & SCN_MEM_READ != 0 {
                prot |= libc::PROT_READ;
            }
            if c & SCN_MEM_WRITE != 0 {
                prot |= libc::PROT_WRITE;
            }
            if c & SCN_MEM_EXECUTE != 0 {
                prot |= libc::PROT_EXEC;
            }
            let start = s.rva as usize & !(PAGE - 1);
            let end =
                (s.rva as usize + s.virtual_size.max(s.file_size) as usize).next_multiple_of(PAGE);
            protect(self.base + start, end - start, prot)
                .with_context(|| format!("section {}", s.name))?;
        }
        Ok(())
    }
}

/// Reserves `size` bytes, at `preferred` if that range is free.
fn map(preferred: u64, size: usize) -> Result<usize> {
    let prot = libc::PROT_READ | libc::PROT_WRITE;
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
    // SAFETY: without MAP_FIXED the address is only a hint: no existing
    // mapping is replaced.
    let at = unsafe { libc::mmap(preferred as *mut _, size, prot, flags, -1, 0) };
    if at == libc::MAP_FAILED {
        bail!(
            "mapping {size:#x} bytes failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(at as usize)
}

fn protect(at: usize, len: usize, prot: i32) -> Result<()> {
    // SAFETY: only ever called on ranges of the image's own mapping.
    if unsafe { libc::mprotect(at as *mut _, len, prot) } != 0 {
        bail!("mprotect failed: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

/// Applies the base relocations of `bytes` to `image`, mapped `delta` bytes
/// away from its preferred base.
fn relocate(bytes: &[u8], image: &mut [u8], delta: u64) -> Result<()> {
    let entry = layout::data_directory_offset(bytes, layout::dir::BASERELOC)
        .context("no relocation directory")?;
    let rva = layout::read_u32(bytes, entry).context("relocation directory")? as usize;
    let size = layout::read_u32(bytes, entry + 4).context("relocation directory")? as usize;
    let table = image
        .get(rva..rva + size)
        .context("relocations outside the image")?
        .to_vec();
    let mut blocks = &table[..];
    while blocks.len() >= 8 {
        let page = u32::from_le_bytes(blocks[0..4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(blocks[4..8].try_into().unwrap()) as usize;
        ensure!(
            (8..=blocks.len()).contains(&len),
            "malformed relocation block at page {page:#x}"
        );
        for entry in blocks[8..len].chunks_exact(2) {
            let entry = u16::from_le_bytes([entry[0], entry[1]]);
            let at = page + (entry & 0xFFF) as usize;
            match entry >> 12 {
                REL_BASED_ABSOLUTE => {}
                REL_BASED_DIR64 => {
                    let slot = image
                        .get_mut(at..at + 8)
                        .context("relocation outside the image")?;
                    let value = u64::from_le_bytes(slot[..8].try_into().unwrap());
                    slot.copy_from_slice(&value.wrapping_add(delta).to_le_bytes());
                }
                kind => bail!("relocation type {kind} at {at:#x} is not handled"),
            }
        }
        blocks = &blocks[len..];
    }
    Ok(())
}

/// An image we mapped, in load order, and what the threads it hears of
/// need.
struct Registered {
    image: MappedImage,
    /// Its `DLL_PROCESS_ATTACH` is done; only then does it hear of threads.
    attached: bool,
    /// `DisableThreadLibraryCalls`: no more `DllMain` thread notices.
    quiet: bool,
}

/// Every image we mapped, then every thread's TLS array (as addresses), so
/// that an image mapped later gets its block in threads already running, as
/// on Windows. Taken in this order.
static IMAGES: Mutex<Vec<Registered>> = Mutex::new(Vec::new());
static ARRAYS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// TLS slots handed out, one bit each.
static TLS_TAKEN: Mutex<u64> = Mutex::new(0);

fn images() -> std::sync::MutexGuard<'static, Vec<Registered>> {
    IMAGES.lock().unwrap_or_else(|e| e.into_inner())
}

fn arrays() -> std::sync::MutexGuard<'static, Vec<usize>> {
    ARRAYS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Slot 0 for an exe (its code reads its block at `gs:[0x58]` then `[0]`
/// without reading `_tls_index`), the lowest free one from 1 for a DLL.
fn reserve_tls_index(dll: bool) -> Result<usize> {
    let mut taken = TLS_TAKEN.lock().unwrap_or_else(|e| e.into_inner());
    let free = if dll { 1..TLS_SLOTS } else { 0..1 };
    let Some(index) = free.into_iter().find(|i| *taken & (1 << i) == 0) else {
        bail!("no free TLS slot for this image (an exe's is slot 0)");
    };
    *taken |= 1 << index;
    Ok(index)
}

/// Records a mapped image, and gives threads already running its TLS block.
fn registered(image: &MappedImage) {
    let mut images = images();
    if let (Some(index), Some(block)) = (image.tls_index, image.tls_block()) {
        for &array in arrays().iter() {
            let block = Box::leak(block.clone().into_boxed_slice()).as_mut_ptr() as usize;
            // SAFETY: each array is a leaked `[usize; TLS_SLOTS]`; `index` is
            // below `TLS_SLOTS` and reserved for this image alone.
            unsafe { (array as *mut usize).add(index).write(block) };
        }
    }
    images.push(Registered {
        image: image.clone(),
        attached: false,
        quiet: false,
    });
}

fn attached(base: usize) {
    if let Some(r) = images().iter_mut().find(|r| r.image.base == base) {
        r.attached = true;
    }
}

/// `DisableThreadLibraryCalls`: the image's `DllMain` hears of no more
/// threads (its TLS callbacks still do). Whether `base` is one of ours.
pub fn disable_thread_calls(base: usize) -> bool {
    match images().iter_mut().find(|r| r.image.base == base) {
        Some(r) => {
            r.quiet = true;
            true
        }
        None => false,
    }
}

/// Tells every attached image of a thread start or end: in attach order for
/// a start, the reverse for an end, as the Windows loader does.
///
/// # Safety
/// Runs title code on a thread that called [`enter_thread`].
unsafe fn thread_notice(reason: u32) {
    // Copied, so the title can map images while it hears of the thread.
    let mut told: Vec<(MappedImage, bool)> = images()
        .iter()
        .filter(|r| r.attached)
        .map(|r| (r.image.clone(), !r.quiet))
        .collect();
    if reason == DLL_THREAD_DETACH {
        told.reverse();
    }
    for (image, dll_main) in told {
        // SAFETY: forwarded from the caller.
        unsafe { image.notify(reason, dll_main) };
    }
}

/// Starts a title thread: `start(param)` with the Windows x64 convention, on
/// a thread of `stack` bytes (at least 64 KiB) that has a TEB, and that every
/// attached image hears of when it starts and ends. Its result is
/// `start`'s.
///
/// # Safety
/// `start` is title code with the signature of a thread procedure.
pub unsafe fn spawn_thread(
    start: usize,
    param: usize,
    stack: usize,
) -> std::io::Result<std::thread::JoinHandle<u32>> {
    type Start = unsafe extern "win64" fn(usize) -> u32;
    // SAFETY: the caller's guarantee.
    let start: Start = unsafe { std::mem::transmute(start) };
    std::thread::Builder::new()
        .name("title".into())
        .stack_size(stack.max(64 << 10))
        .spawn(move || {
            enter_thread();
            // SAFETY: this thread has a TEB; the caller vouches for `start`.
            unsafe {
                thread_notice(DLL_THREAD_ATTACH);
                let result = start(param);
                thread_notice(DLL_THREAD_DETACH);
                result
            }
        })
}

thread_local! {
    static TEB: Cell<usize> = const { Cell::new(0) };
}

/// Makes the calling thread ready to run title code: a TEB, and a TLS array
/// with every mapped image's static TLS block, reachable through `gs` as
/// Win64 code expects. Returns the TEB's address; calling it again on the
/// same thread returns the same. [`spawn_thread`] and
/// [`MappedImage::run_main`] call it; a thread of ours that calls into the
/// title (a callback) must call it first.
///
/// macOS keeps a thread's pthread TSD array at `gs`, so `gs:0x30` and
/// `gs:0x58` are TSD slots 6 and 11. The TEB's `Self` and TLS pointers are
/// stored there, as Wine does on macOS.
///
/// shortcut: only `Self` and the TLS pointer are reachable through `gs`;
/// the other TEB fields (stack bounds at `gs:0x08`/`0x10`, ids at `0x40`/`0x48`,
/// the PEB at `0x60`) overlap TSD slots macOS uses, so a `gs` read of them
/// gets macOS's values. Rewrite such reads in the image when a title needs them.
/// shortcut: the TEB and TLS blocks are leaked when the thread ends; free them
/// on thread exit once titles start many short threads.
pub fn enter_thread() -> usize {
    let teb = TEB.get();
    if teb != 0 {
        return teb;
    }
    let tls: &'static mut [usize] = Box::leak(vec![0usize; TLS_SLOTS].into_boxed_slice());
    {
        let images = images();
        for r in images.iter() {
            if let (Some(index), Some(block)) = (r.image.tls_index, r.image.tls_block()) {
                tls[index] = Box::leak(block.into_boxed_slice()).as_mut_ptr() as usize;
            }
        }
        // Under the images' lock, so an image mapped now reaches this array.
        arrays().push(tls.as_ptr() as usize);
    }
    let teb: &'static mut [u64] = Box::leak(vec![0u64; TEB_SIZE / 8].into_boxed_slice());
    let address = teb.as_ptr() as usize;
    teb[TEB_SELF / 8] = address as u64;
    teb[TEB_TLS_POINTER / 8] = tls.as_ptr() as u64;
    // SAFETY: writes two of this thread's own TSD slots. Slot 6 is unused by
    // macOS; slot 11 holds ttyname's buffer, which nothing of ours calls.
    unsafe {
        std::arch::asm!("mov gs:[0x30], {}", in(reg) address as u64, options(nostack));
        std::arch::asm!("mov gs:[0x58], {}", in(reg) tls.as_ptr() as u64, options(nostack));
    }
    TEB.set(address);
    address
}
