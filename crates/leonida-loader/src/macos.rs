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
/// Slots of a thread's TLS array; the image's static TLS is slot 0.
const TLS_SLOTS: usize = 64;
const PAGE: usize = 0x1000;

/// A title image mapped into this process, relocated and bound.
#[derive(Debug)]
pub struct MappedImage {
    base: usize,
    info: ImageInfo,
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
        let mapped = MappedImage { base, info };
        if let Some(at) = mapped.tls_field(TLS_INDEX) {
            // An exe's static TLS is slot 0 of the thread's array; `enter_thread` puts it there.
            let at = mapped.rva_of(at).context("TLS index outside the image")?;
            image
                .get_mut(at..at + 4)
                .context("TLS index outside the image")?
                .fill(0);
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

thread_local! {
    static TEB: Cell<usize> = const { Cell::new(0) };
}

/// Makes the calling thread ready to run `image`'s code: a TEB with its
/// static TLS block, reachable through `gs` as Win64 code expects. Returns
/// the TEB's address; calling it again on the same thread returns the same.
///
/// macOS keeps a thread's pthread TSD array at `gs`, so `gs:0x30` and
/// `gs:0x58` are TSD slots 6 and 11. The TEB's `Self` and TLS pointers are
/// stored there, as Wine does on macOS.
///
/// shortcut: only `Self` and the TLS pointer are reachable through `gs`;
/// the other TEB fields (stack bounds at `gs:0x08`/`0x10`, ids at `0x40`/`0x48`,
/// the PEB at `0x60`) overlap TSD slots macOS uses, so a `gs` read of them
/// gets macOS's values. Rewrite such reads in the image when a title needs them.
/// shortcut: the TEB and TLS block are leaked when the thread ends; free them
/// on thread exit once titles start many short threads.
pub fn enter_thread(image: &MappedImage) -> usize {
    let teb = TEB.get();
    if teb != 0 {
        return teb;
    }
    let tls: &'static mut [usize] = Box::leak(vec![0usize; TLS_SLOTS].into_boxed_slice());
    if let Some(block) = image.tls_block() {
        tls[0] = Box::leak(block.into_boxed_slice()).as_mut_ptr() as usize;
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
