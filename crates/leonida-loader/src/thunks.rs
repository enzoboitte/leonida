//! Stubs for imports nobody implements yet.
//!
//! Each unresolved import gets 32 bytes of generated code:
//!
//! ```text
//! mov rcx, <&'static ImportSite>   ; 48 B9 imm64
//! mov rax, <unimplemented_import>  ; 48 B8 imm64
//! jmp rax                          ; FF E0
//! ```
//!
//! The jump keeps the title's return address on the stack, so the handler
//! returns straight to the caller. Bring-up then shows exactly which API to
//! implement next, rather than failing at load time.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, bail};

use crate::{BindReport, ImportResolver, MissingImport, Provenance, Resolution};
use leonida_pe::{ImageInfo, ImportSymbol};

/// What an unimplemented import does when called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnimplementedPolicy {
    /// Log, break into an attached debugger, then abort. The default:
    /// silently continuing usually crashes later and further from the cause.
    #[default]
    Trap,
    /// Log once per import and return 0. Useful to push through bring-up.
    ReturnZero,
}

/// Identifies one unresolved import. Leaked: lives as long as the process.
#[derive(Debug)]
pub struct ImportSite {
    pub dll: String,
    pub symbol: String,
    pub policy: UnimplementedPolicy,
    pub calls: AtomicU64,
}

static UNIMPLEMENTED_CALLS: AtomicU64 = AtomicU64::new(0);

/// Total number of calls that reached an unimplemented-import stub.
pub fn unimplemented_call_count() -> u64 {
    UNIMPLEMENTED_CALLS.load(Ordering::Relaxed)
}

// Title code calls it with the Windows x64 convention, also on hosts where
// "system" is another one.
extern "win64" fn unimplemented_import(site: &'static ImportSite) -> u64 {
    UNIMPLEMENTED_CALLS.fetch_add(1, Ordering::Relaxed);
    let previous = site.calls.fetch_add(1, Ordering::Relaxed);
    match site.policy {
        UnimplementedPolicy::ReturnZero => {
            if previous == 0 {
                tracing::warn!(dll = %site.dll, symbol = %site.symbol, "unimplemented import called, returning 0");
            }
            0
        }
        UnimplementedPolicy::Trap => {
            tracing::error!(dll = %site.dll, symbol = %site.symbol, "unimplemented import called");
            // Into the crash log too: `abort` ends the process without
            // running the handlers that write it.
            let (dll, symbol) = (&site.dll, &site.symbol);
            let text = format!("Leonida Legacy: unimplemented import {dll}!{symbol} called");
            #[cfg(windows)]
            crate::log_crash(&text);
            #[cfg(not(windows))]
            eprintln!("{text}");
            #[cfg(windows)]
            // SAFETY: plain Win32 calls without preconditions.
            unsafe {
                use windows_sys::Win32::System::Diagnostics::Debug::{
                    DebugBreak, IsDebuggerPresent,
                };
                if IsDebuggerPresent() != 0 {
                    DebugBreak();
                }
            }
            std::process::abort();
        }
    }
}

const THUNK_SIZE: usize = 32;

/// Executable memory holding one stub per unresolved import.
pub(crate) struct ThunkBlock {
    base: *mut u8,
    len: usize,
    used: usize,
}

impl ThunkBlock {
    /// Reserves room for `count` stubs (writable until [`Self::seal`]).
    pub(crate) fn new(count: usize) -> Result<Self> {
        let len = (count.max(1) * THUNK_SIZE).next_multiple_of(4096);
        let base = memory::alloc_rw(len);
        if base.is_null() {
            bail!(
                "allocating import stubs failed: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(ThunkBlock { base, len, used: 0 })
    }

    /// Emits a stub for `site` and returns its address.
    pub(crate) fn emit(&mut self, site: &'static ImportSite) -> Result<usize> {
        if self.used + THUNK_SIZE > self.len {
            bail!("import stub block is full");
        }
        let mut code = [0xCC_u8; THUNK_SIZE]; // int3 padding
        code[0..2].copy_from_slice(&[0x48, 0xB9]);
        code[2..10].copy_from_slice(&(site as *const ImportSite as u64).to_le_bytes());
        code[10..12].copy_from_slice(&[0x48, 0xB8]);
        let handler = unimplemented_import as extern "win64" fn(&'static ImportSite) -> u64;
        code[12..20].copy_from_slice(&(handler as *const () as u64).to_le_bytes());
        code[20..22].copy_from_slice(&[0xFF, 0xE0]);
        // SAFETY: `used + THUNK_SIZE <= len`, and the block is still writable.
        let at = unsafe {
            let at = self.base.add(self.used);
            std::ptr::copy_nonoverlapping(code.as_ptr(), at, THUNK_SIZE);
            at
        };
        self.used += THUNK_SIZE;
        Ok(at as usize)
    }

    /// Makes the block read+execute. The block is intentionally leaked:
    /// title code may hold these addresses for the life of the process.
    pub(crate) fn seal(self) -> Result<()> {
        // SAFETY: `base..base+len` is the allocation made in `new`.
        if !unsafe { memory::seal_rx(self.base, self.len) } {
            bail!(
                "protecting import stubs failed: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }
}

/// Resolves every import of `info` (delay-load slots too, so the title's own
/// delay-load helper, which would ask the host, never runs). Whatever
/// `resolver` cannot resolve gets a stub that behaves according to `policy`.
/// Returns each IAT slot's RVA with its address, for the caller to write.
pub(crate) fn resolve_imports(
    info: &ImageInfo,
    resolver: &dyn ImportResolver,
    policy: UnimplementedPolicy,
) -> Result<(Vec<(u32, usize)>, BindReport)> {
    let mut report = BindReport::default();
    let mut slots: Vec<(u32, usize)> = Vec::with_capacity(info.import_count());
    let mut unresolved: Vec<(u32, &str, &ImportSymbol)> = Vec::new();
    for module in info.imports.iter().chain(&info.delay_imports) {
        for entry in &module.entries {
            match resolver.resolve(&module.dll, &entry.symbol) {
                Resolution::Found {
                    address,
                    provenance,
                } => {
                    match provenance {
                        Provenance::Hle => report.hle += 1,
                        Provenance::Host => report.host += 1,
                        Provenance::Title => report.title += 1,
                    }
                    slots.push((entry.iat_slot_rva, address));
                }
                Resolution::Missing => {
                    unresolved.push((entry.iat_slot_rva, &module.dll, &entry.symbol));
                }
            }
        }
    }

    if !unresolved.is_empty() {
        let mut block = ThunkBlock::new(unresolved.len())?;
        for (slot, dll, symbol) in unresolved {
            let site: &'static ImportSite = Box::leak(Box::new(ImportSite {
                dll: dll.to_owned(),
                symbol: symbol.to_string(),
                policy,
                calls: AtomicU64::new(0),
            }));
            slots.push((slot, block.emit(site)?));
            report.missing.push(MissingImport {
                dll: site.dll.clone(),
                symbol: site.symbol.clone(),
            });
        }
        block.seal()?;
    }
    Ok((slots, report))
}

#[cfg(windows)]
mod memory {
    use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READ, PAGE_READWRITE, VirtualAlloc, VirtualProtect,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// Fresh read+write memory, or null.
    pub(super) fn alloc_rw(len: usize) -> *mut u8 {
        // SAFETY: fresh anonymous allocation; no existing memory is touched.
        unsafe { VirtualAlloc(std::ptr::null(), len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) }
            .cast()
    }

    /// Makes `base..base+len` read+execute.
    ///
    /// # Safety
    /// `base..base+len` is an allocation from [`alloc_rw`].
    pub(super) unsafe fn seal_rx(base: *mut u8, len: usize) -> bool {
        let mut old = 0;
        // SAFETY: the caller's allocation.
        if unsafe { VirtualProtect(base.cast(), len, PAGE_EXECUTE_READ, &mut old) } == 0 {
            return false;
        }
        // SAFETY: flushing our own process's instruction cache for our own block.
        unsafe { FlushInstructionCache(GetCurrentProcess(), base.cast(), len) };
        true
    }
}

#[cfg(not(windows))]
mod memory {
    /// Fresh read+write memory, or null.
    pub(super) fn alloc_rw(len: usize) -> *mut u8 {
        let prot = libc::PROT_READ | libc::PROT_WRITE;
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
        // SAFETY: fresh anonymous mapping; no existing memory is touched.
        let at = unsafe { libc::mmap(std::ptr::null_mut(), len, prot, flags, -1, 0) };
        if at == libc::MAP_FAILED {
            std::ptr::null_mut()
        } else {
            at.cast()
        }
    }

    /// Makes `base..base+len` read+execute. On x86-64 the instruction cache
    /// needs no flush, and Rosetta notices the protection change itself.
    ///
    /// # Safety
    /// `base..base+len` is an allocation from [`alloc_rw`].
    pub(super) unsafe fn seal_rx(base: *mut u8, len: usize) -> bool {
        // SAFETY: the caller's mapping.
        unsafe { libc::mprotect(base.cast(), len, libc::PROT_READ | libc::PROT_EXEC) == 0 }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// In a child process of the next test: calls an unimplemented import
    /// that traps, with the crash log where `LEONIDA_TRAP_LOG` says.
    #[test]
    fn trap_child() {
        let Some(log) = std::env::var_os("LEONIDA_TRAP_LOG") else {
            return;
        };
        crate::log_crashes_to(log.into());
        let site = Box::leak(Box::new(ImportSite {
            dll: "probe.dll".into(),
            symbol: "Missing".into(),
            policy: UnimplementedPolicy::Trap,
            calls: AtomicU64::new(0),
        }));
        unimplemented_import(site);
    }

    #[test]
    fn a_trapped_import_is_in_the_crash_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("crashes.log");
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "thunks::tests::trap_child", "--nocapture"])
            .env("LEONIDA_TRAP_LOG", &log)
            .output()
            .unwrap();
        assert!(!out.status.success(), "the trap ends the process");
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let named = text.contains("Leonida Legacy: unimplemented import probe.dll!Missing called");
        assert!(named, "crash log: {text:?}");
    }
}
