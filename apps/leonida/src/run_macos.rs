//! `leonida run` on macOS: the title's image mapped by our own loader
//! (`leonida_loader::MappedImage`, under Rosetta 2), its imports bound to
//! `leonida-hle`, whose `win32` module stands in for Windows' own DLLs.
//! There is no host pass-through here: whatever we do not implement is a
//! missing import.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use leonida_hle::{HleResolver, HostPolicy};
use leonida_loader::{
    BindReport, ImportResolver, MappedImage, Provenance, Resolution, UnimplementedPolicy,
};
use leonida_pe::{ImageInfo, ImportSymbol};

use crate::{Console, Unimplemented};

/// The resolver, then the exports of the title DLLs mapped so far.
struct WithDlls<'a> {
    inner: &'a HleResolver,
    dlls: &'a [(String, MappedImage)],
}

impl ImportResolver for WithDlls<'_> {
    fn resolve(&self, dll: &str, symbol: &ImportSymbol) -> Resolution {
        // A real C runtime, when one was supplied, gives every function it has:
        // two runtimes must not share a heap or an exit table.
        let target = api_set(dll);
        if target.eq_ignore_ascii_case("ucrtbase.dll") {
            let found = self.title(target, symbol);
            if found != Resolution::Missing {
                return found;
            }
        }
        let found = self.inner.resolve(dll, symbol);
        if found != Resolution::Missing {
            return found;
        }
        self.title(target, symbol)
    }
}

impl WithDlls<'_> {
    /// `symbol` from the title DLL `dll`, if it is mapped and has it.
    fn title(&self, dll: &str, symbol: &ImportSymbol) -> Resolution {
        let Some((_, image)) = self.dlls.iter().find(|(n, _)| n.eq_ignore_ascii_case(dll)) else {
            return Resolution::Missing;
        };
        let address = match symbol {
            ImportSymbol::Name { name, .. } => image.export(name),
            ImportSymbol::Ordinal(o) => {
                let e = image.info().exports.iter().find(|e| e.ordinal == *o);
                e.and_then(|e| e.rva).map(|rva| image.base() + rva as usize)
            }
        };
        address.map_or(Resolution::Missing, |address| Resolution::Found {
            address,
            provenance: Provenance::Title,
        })
    }
}

/// The DLL behind an API set Windows redirects to one: the C runtime's
/// (`api-ms-win-crt-*`) is `ucrtbase.dll`. Others are ours or named as is.
fn api_set(dll: &str) -> &str {
    if dll.to_ascii_lowercase().starts_with("api-ms-win-crt-") {
        "ucrtbase.dll"
    } else {
        dll
    }
}

/// A DLL from the first of `dirs` that has it (any case), unless we serve it.
fn title_dll(dirs: &[&Path], dll: &str) -> Option<PathBuf> {
    if leonida_hle::registry().iter().any(|m| m.serves(dll)) {
        return None;
    }
    dirs.iter().find_map(|dir| {
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(dll))
            .map(|e| e.path())
    })
}

/// Maps the title DLLs `info` imports, each after the ones it imports, into
/// `dlls` (load order: dependencies first).
fn map_dlls(
    info: &ImageInfo,
    dirs: &[&Path],
    resolver: &HleResolver,
    policy: UnimplementedPolicy,
    seen: &mut HashSet<String>,
    dlls: &mut Vec<(String, MappedImage)>,
    show: &dyn Fn(&str, &BindReport),
) -> Result<()> {
    for module in info.imports.iter().chain(&info.delay_imports) {
        let dll = api_set(&module.dll);
        if !seen.insert(dll.to_ascii_lowercase()) {
            continue;
        }
        let Some(path) = title_dll(dirs, dll) else {
            continue;
        };
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let dll_info = ImageInfo::parse(&bytes)?;
        map_dlls(&dll_info, dirs, resolver, policy, seen, dlls, show)?;
        let with = WithDlls {
            inner: resolver,
            dlls,
        };
        let (image, report) = MappedImage::load(&bytes, &with, policy)
            .with_context(|| format!("mapping {}", path.display()))?;
        println!(
            "title dll      {dll} at {:#x} ({})",
            image.base(),
            path.display()
        );
        leonida_hle::runtime::set_image_path(image.base(), &path);
        show(dll, &report);
        dlls.push((dll.to_string(), image));
    }
    Ok(())
}

pub fn run(
    exe: &Path,
    unimplemented: Unimplemented,
    _strict: bool,
    execute: bool,
    console: Console,
) -> Result<()> {
    // No AVX2 check: Rosetta 2 runs AVX2 but its CPUID hides AVX, AVX2, FMA,
    // BMI2 and OSXSAVE (measured on an M5, macOS 26.6), so asking would refuse
    // every Mac. A title that checks CPUID itself takes its paths without them.
    let cfg = leonida_core::Config::load()?;
    let original = std::fs::read(exe).with_context(|| format!("reading {}", exe.display()))?;
    let hash = leonida_loader::prepare(&original)?.original_hash.to_hex();
    match leonida_game::identify(&hash) {
        Some(build) => println!("build          {} ({hash})", build.label),
        None => println!("build          unknown ({hash})"),
    }

    let title_dir = exe.parent().unwrap_or(Path::new("."));
    let manifest =
        std::fs::read_to_string(title_dir.join("MicrosoftGame.config")).unwrap_or_default();
    let title = leonida_hle::runtime::Title::from_manifest(&manifest);
    let storage = cfg.data_subdir(&format!("{:08X}/storage", title.id))?;
    let temp = cfg.data_subdir(&format!("{:08X}/temp", title.id))?;
    println!(
        "title          {:08X}, storage {}",
        title.id,
        storage.display()
    );
    // The console's fixed drives: the package as `G:`, temporary files `T:`.
    leonida_hle::modules::files::mount_at('G', title_dir);
    leonida_hle::modules::files::mount_at('T', &temp);
    leonida_hle::runtime::set_title(leonida_hle::runtime::Title { storage, ..title });
    leonida_hle::console::set(match console {
        Console::SeriesX => leonida_hle::console::Console::SeriesX,
        Console::SeriesS => leonida_hle::console::Console::SeriesS,
    });
    let exe_name = exe
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    leonida_hle::modules::win32::set_command_line(&format!("G:\\{exe_name}"));

    let policy = match unimplemented {
        Unimplemented::Trap => UnimplementedPolicy::Trap,
        Unimplemented::Zero => UnimplementedPolicy::ReturnZero,
    };
    let resolver = HleResolver::new(HostPolicy::none());
    let show = |name: &str, report: &BindReport| {
        println!(
            "bound          {name}: {} hle, {} title, {} missing",
            report.hle,
            report.title,
            report.missing.len()
        );
        for m in &report.missing {
            println!("  missing      {}!{}", m.dll, m.symbol);
        }
    };
    // The C runtime DLLs (Microsoft's, which the title gets from Windows on a
    // PC) come from this folder when the title's does not have them.
    let runtime = cfg.data_subdir("runtime")?;
    println!("runtime dlls   {}", runtime.display());
    let info = ImageInfo::parse(&original)?;
    let mut dlls = Vec::new();
    let mut seen = HashSet::new();
    map_dlls(
        &info,
        &[title_dir, &runtime],
        &resolver,
        policy,
        &mut seen,
        &mut dlls,
        &show,
    )?;
    let with = WithDlls {
        inner: &resolver,
        dlls: &dlls,
    };
    let (image, report) = MappedImage::load(&original, &with, policy)?;
    drop(original);
    leonida_hle::runtime::set_main_image(image.base());
    // The title's own allocator, if its exe exports one (see `modules::xmem`).
    let hooks = image.export("__XMemAllocationHooks__").unwrap_or(0);
    leonida_hle::modules::win32::xmem::set_allocation_hooks(hooks);
    leonida_hle::runtime::set_image_path(image.base(), exe);
    println!(
        "mapped at      {:#x} (preferred {:#x})",
        image.base(),
        image.info().image_base
    );
    show(&exe_name, &report);
    if !execute {
        println!("not executing (pass --execute to start the title)");
        return Ok(());
    }
    let dlls = dlls.into_iter().map(|(_, dll)| dll).collect();
    // SAFETY: imports are bound; from here on we run title code.
    let main = unsafe { image.run_main(dlls)? };
    let code = main
        .join()
        .map_err(|_| anyhow::anyhow!("title main thread panicked"))??;
    println!("title exited with {code}");
    Ok(())
}
