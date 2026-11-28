#[cfg(any(not(target_os = "macos"), test))]
use std::path::Path;

#[cfg(windows)]
use anyhow::Context;
#[cfg(any(not(target_os = "macos"), test))]
use anyhow::Result;

#[cfg(not(target_os = "macos"))]
use crate::{Console, Unimplemented};

#[cfg(target_os = "macos")]
pub use crate::run_macos::run;

#[cfg(not(any(windows, target_os = "macos")))]
pub fn run(_: &Path, _: Unimplemented, _: bool, _: bool, _: Console) -> Result<()> {
    anyhow::bail!("running titles requires Windows or an x86-64 Mac (Rosetta 2)")
}

#[cfg(windows)]
pub fn run(
    exe: &Path,
    unimplemented: Unimplemented,
    strict: bool,
    execute: bool,
    console: Console,
) -> Result<()> {
    run_title(exe, unimplemented, strict, execute, console).map_err(explain_commit_limit)
}

#[cfg(windows)]
fn run_title(
    exe: &Path,
    unimplemented: Unimplemented,
    strict: bool,
    execute: bool,
    console: Console,
) -> Result<()> {
    use std::sync::Arc;
    use leonida_hle::{HleResolver, HostPolicy};
    use leonida_loader::{BindReport, UnimplementedPolicy, WithTitleDlls};

    if execute {
        check_avx2(has_avx2())?;
    }
    // A title that relaunched itself: the old process must be gone first.
    leonida_hle::modules::lifecycle::wait_for_previous();
    let cfg = leonida_core::Config::load()?;
    crate::graphics::initialize(&cfg)?;
    let original = std::fs::read(exe).with_context(|| format!("reading {}", exe.display()))?;
    let prepared = leonida_loader::prepare(&original)?;
    let hash = prepared.original_hash.to_hex();
    match leonida_game::identify(&hash) {
        Some(build) => println!("build          {} ({hash})", build.label),
        None => println!("build          unknown ({hash})"),
    }

    let images = cfg.cache_subdir("images")?;
    leonida_hle::modules::d3d12x::use_shader_cache(cfg.cache_subdir("shaders")?);
    let image = leonida_loader::load(&prepared, &images)?;
    // The program is mapped from the copy `load` wrote: the file's bytes
    // and their patched copy are not read again, and were held for the
    // whole run (two blocks of 114.7 MB each for the title, in
    // physical memory: run 65's memory map).
    drop(prepared);
    drop(original);
    leonida_hle::runtime::set_main_image(image.base());
    leonida_hle::runtime::set_image_path(image.base(), exe);
    println!(
        "mapped at      {:#x} (preferred {:#x})",
        image.base(),
        image.info().image_base
    );
    let resolver = HleResolver::new(if strict {
        HostPolicy::none()
    } else {
        HostPolicy::default()
    });
    let wanted = |dll: &str| title_dll(&resolver.host, dll);
    let title_dir = exe.parent().unwrap_or(Path::new("."));
    // A missing manifest leaves id 0 and no storage quota.
    let manifest =
        std::fs::read_to_string(title_dir.join("MicrosoftGame.config")).unwrap_or_default();
    let title = leonida_hle::runtime::Title::from_manifest(&manifest);
    let storage = cfg.data_subdir(&format!("{:08X}/storage", title.id))?;
    println!(
        "title          {:08X}, storage {} ({} MB)",
        title.id,
        storage.display(),
        title.storage_bytes >> 20
    );
    // The console's fixed drives: the package as `G:`, temporary files `T:`.
    leonida_hle::modules::files::mount_at('G', title_dir);
    let temp = cfg.data_subdir(&format!("{:08X}/temp", title.id))?;
    leonida_hle::modules::files::mount_at('T', &temp);
    leonida_hle::runtime::set_title(leonida_hle::runtime::Title { storage, ..title });
    leonida_hle::console::set(match console {
        Console::SeriesX => leonida_hle::console::Console::SeriesX,
        Console::SeriesS => leonida_hle::console::Console::SeriesS,
    });
    println!("console        {}", leonida_hle::console::current().name());
    let dlls = leonida_loader::load_title_dlls(image.info(), title_dir, &images, &wanted)?;
    for (name, dll) in &dlls {
        println!("title dll      {name} at {:#x}", dll.base());
        leonida_hle::runtime::set_image_path(dll.base(), &title_dir.join(name));
    }
    let imported: Vec<&str> = image
        .info()
        .imports
        .iter()
        .map(|m| m.dll.as_str())
        .collect();
    let mapped: Vec<_> = dlls
        .iter()
        .map(|(name, dll)| {
            let imports = dll.info().imports.iter().map(|m| m.dll.as_str()).collect();
            (name.as_str(), imports)
        })
        .collect();
    let absent = absent_title_dlls(&imported, &mapped, &wanted);
    if !absent.is_empty() {
        let message = missing_dlls_message(&absent, title_dir);
        println!("missing dlls   {message}");
        // None of their imports can be bound, so the title would stop at its
        // first call into one with nothing saying why. Stubs only if asked for.
        if execute && matches!(unimplemented, Unimplemented::Trap) {
            anyhow::bail!("{message}");
        }
    }

    let policy = match unimplemented {
        Unimplemented::Trap => UnimplementedPolicy::Trap,
        Unimplemented::Zero => UnimplementedPolicy::ReturnZero,
    };
    let with_dlls = WithTitleDlls {
        inner: &resolver,
        dlls: &dlls,
    };
    let show = |name: &str, report: BindReport| {
        println!(
            "bound          {name}: {} hle, {} host, {} title, {} missing",
            report.hle,
            report.host,
            report.title,
            report.missing.len()
        );
        for m in &report.missing {
            println!("  missing      {}!{}", m.dll, m.symbol);
        }
    };
    for (name, dll) in &dlls {
        show(name, dll.bind_imports(&with_dlls, policy)?);
    }
    let exe_name = exe.file_name().unwrap_or_default().to_string_lossy();
    show(&exe_name, image.bind_imports(&with_dlls, policy)?);
    if !execute {
        println!("not executing (pass --execute to start the title)");
        return Ok(());
    }
    leonida_loader::report_crashes();
    leonida_loader::log_crashes_to(temp.with_file_name("crashes.log"));
    leonida_hle::runtime::use_console_timers();
    #[cfg(feature = "diagnostics")]
    diagnostics(&image, &cfg);
    let late = LateDlls {
        host: resolver.host.clone(),
        title_dir: title_dir.to_owned(),
        cache_dir: images,
        policy,
        mapped: std::sync::Mutex::new(dlls.clone()),
        lock: LoaderLock::default(),
    };
    leonida_hle::runtime::set_title_dll_loader(Box::new(move |file| {
        late.load(file, show).map_err(|e| format!("{e:#}"))
    }));
    // SAFETY: imports are bound; from here on we run title code.
    let main = unsafe { Arc::new(image).spawn_main(dlls)? };
    let code = main
        .join()
        .map_err(|_| anyhow::anyhow!("title main thread panicked"))??;
    println!("title exited with {code}");
    Ok(())
}

/// The debugging aids (README): scheduled stack dumps, watched values and
/// memory saved on a crash.
#[cfg(all(windows, feature = "diagnostics"))]
fn diagnostics(image: &leonida_loader::LoadedImage, cfg: &leonida_core::Config) {
    leonida_loader::dump_stacks_on_schedule();
    leonida_hle::modules::audio::capture::start();
    watch(image.base(), image.info().size_of_image as usize);
    // Debugging aid: `LEONIDA_CRASH_SAVE=<rva>:<len>,...` (hex) writes those
    // title ranges to `crash-<rva>.bin` in the cache dir on a crash.
    let size = image.info().size_of_image as usize;
    let ranges = std::env::var("LEONIDA_CRASH_SAVE").unwrap_or_default();
    let hex = |s: &str| usize::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
    leonida_loader::save_on_crash(
        ranges
            .split(',')
            .filter_map(|r| r.split_once(':'))
            .filter_map(|(rva, len)| Some((hex(rva)?, hex(len)?)))
            .filter(|&(rva, len)| rva.checked_add(len).is_some_and(|end| end <= size))
            .map(|(rva, len)| {
                let file = cfg.cache_dir.join(format!("crash-{rva:x}.bin"));
                (image.base() + rva, len, file)
            })
            .collect(),
    );
}

/// Debugging aid: `LEONIDA_WATCH=<rva>,...` (hex) logs the u32 at each
/// title+rva whenever it changes, polled every 5 ms (an engine's state, say).
#[cfg(all(windows, feature = "diagnostics"))]
fn watch(base: usize, size: usize) {
    let Ok(list) = std::env::var("LEONIDA_WATCH") else {
        return;
    };
    let rvas: Vec<usize> = list
        .split(',')
        .filter_map(|r| usize::from_str_radix(r.trim().trim_start_matches("0x"), 16).ok())
        .filter(|&rva| rva + 4 <= size)
        .collect();
    std::thread::spawn(move || {
        let mut seen = vec![None; rvas.len()];
        loop {
            for (&rva, seen) in rvas.iter().zip(&mut seen) {
                // SAFETY: four bytes inside the mapped image, checked above.
                let value = unsafe { ((base + rva) as *const u32).read_volatile() };
                if *seen != Some(value) {
                    eprintln!("Leonida Legacy: title+{rva:#x} = {value:#x}");
                    *seen = Some(value);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });
}

/// DLLs the host or our runtime provide are never taken from the title.
#[cfg(windows)]
fn title_dll(host: &leonida_hle::HostPolicy, dll: &str) -> bool {
    !host.allows(dll) && !leonida_hle::registry().iter().any(|m| m.serves(dll))
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn has_avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

#[cfg(all(windows, not(target_arch = "x86_64")))]
fn has_avx2() -> bool {
    true
}

/// Xbox Series code is built for Zen 2 and uses AVX2 throughout: without it
/// the title dies on its first such instruction with nothing saying why.
#[cfg(any(windows, test))]
fn check_avx2(has_avx2: bool) -> Result<()> {
    anyhow::ensure!(
        has_avx2,
        "this processor does not support AVX2, which the game needs: it cannot run on this PC. \
         Leonida Legacy needs an Intel processor from 2013 (Haswell) or later, or an AMD \
         processor from 2015 (Excavator) or later."
    );
    Ok(())
}

/// Missing title DLLs reachable through the main image's static imports.
/// `mapped` includes delay-loaded DLLs too, but only static edges from a
/// required image are followed. A delay-only dependency stays optional.
/// Host/HLE modules are supplied elsewhere; cycles and case aliases are
/// visited once. Direct missing imports keep their original order.
#[cfg(any(windows, test))]
fn absent_title_dlls(
    imported: &[&str],
    mapped: &[(&str, Vec<&str>)],
    wanted: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    use std::collections::{HashSet, VecDeque};

    let mut absent = Vec::new();
    let mut visited = HashSet::new();
    let mut pending: VecDeque<_> = imported.iter().copied().collect();
    while let Some(dll) = pending.pop_front() {
        if !wanted(dll) || !visited.insert(dll.to_ascii_lowercase()) {
            continue;
        }
        match mapped.iter().find(|(name, _)| name.eq_ignore_ascii_case(dll)) {
            Some((_, imports)) => pending.extend(imports.iter().copied()),
            None => absent.push(dll.to_owned()),
        }
    }
    absent
}

#[cfg(any(windows, test))]
fn missing_dlls_message(absent: &[String], title_dir: &Path) -> String {
    format!(
        "the game's own {} {} missing from the game folder {}. Copy every file of your copy of \
         the game into that folder, or point LEONIDA_GAME_DIR (or game_dir in leonida.toml) at \
         the folder that has them. `--unimplemented zero` runs it anyway.",
        absent.join(", "),
        if absent.len() == 1 { "is" } else { "are" },
        title_dir.display(),
    )
}

/// Windows error 1455 (`ERROR_COMMITMENT_LIMIT`): the paging file is too
/// small for what the title reserves. The loader's errors carry it as an
/// `io::Error` or only in their text (`os error 1455`).
#[cfg(any(windows, test))]
fn explain_commit_limit(error: anyhow::Error) -> anyhow::Error {
    const ERROR_COMMITMENT_LIMIT: i32 = 1455;
    let hit = error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.raw_os_error() == Some(ERROR_COMMITMENT_LIMIT))
            || cause.to_string().contains("os error 1455")
    });
    if !hit {
        return error;
    }
    error.context(
        "Windows ran out of memory to give the game (error 1455: the paging file is too small). \
         Close other programs, then make the paging file bigger: System > About > Advanced \
         system settings > Performance Settings > Advanced > Virtual memory > Change, and pick \
         'System managed size' or a custom size of at least 32 GB. Restart Windows and try again.",
    )
}

/// Title DLLs the title loads itself (`LoadLibrary`) that nothing imports,
/// e.g. The title's upscaler behind its FidelityFX loader.
#[cfg(windows)]
struct LateDlls {
    host: leonida_hle::HostPolicy,
    title_dir: std::path::PathBuf,
    cache_dir: std::path::PathBuf,
    policy: leonida_loader::UnimplementedPolicy,
    /// Every title DLL mapped so far, to bind new ones against.
    mapped: std::sync::Mutex<Vec<(String, leonida_loader::LoadedImage)>>,
    lock: LoaderLock,
}

#[cfg(windows)]
impl LateDlls {
    /// Every `LoadLibrary` of the title comes here first. Returns a title
    /// image called `file`, mapping it and the title DLLs it needs first,
    /// binding them and running their `DllMain` on the calling thread, as
    /// the Windows loader would; `None` if `file` is no title image.
    fn load(
        &self,
        file: &str,
        show: impl Fn(&str, leonida_loader::BindReport),
    ) -> Result<Option<usize>> {
        use leonida_hle::runtime::{forget_image, image_named, set_image_path};
        // Waits while another thread initialises a DLL; a `DllMain` that
        // loads another DLL re-enters.
        self.lock.hold(|| {
            if let Some(base) = image_named(file) {
                return Ok(Some(base));
            }
            let new = {
                let mut mapped = self.mapped.lock().unwrap_or_else(|e| e.into_inner());
                let wanted = |dll: &str| title_dll(&self.host, dll) && image_named(dll).is_none();
                let new = leonida_loader::load_title_dll(
                    file,
                    &self.title_dir,
                    &self.cache_dir,
                    &wanted,
                )?;
                let hle = leonida_hle::HleResolver::new(self.host.clone());
                let old = leonida_loader::WithTitleDlls {
                    inner: &hle,
                    dlls: &mapped,
                };
                let resolver = leonida_loader::WithTitleDlls {
                    inner: &old,
                    dlls: &new,
                };
                for (name, dll) in &new {
                    println!(
                        "title dll      {name} at {:#x} (loaded by the title)",
                        dll.base()
                    );
                    show(name, dll.bind_imports(&resolver, self.policy)?);
                    set_image_path(dll.base(), &self.title_dir.join(name));
                }
                // Visible before their `DllMain`s run, which may load more.
                mapped.extend(new.iter().cloned());
                new
            };
            let failed = new.iter().enumerate().find_map(|(i, (name, dll))| {
                // SAFETY: bound above, dependencies attached first.
                let attached = unsafe { dll.attach() };
                attached
                    .err()
                    .map(|e| (i, e.context(format!("attaching {name}"))))
            });
            let Some((i, error)) = failed else {
                return Ok(new.last().map(|(_, dll)| dll.base()));
            };
            // As `LoadLibrary` does when a `DllMain` fails: detach what this
            // load attached (the failed one too) and unmap all of it.
            let bases: Vec<_> = new.iter().map(|(_, d)| d.base()).collect();
            self.mapped
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|(_, d)| !bases.contains(&d.base()));
            for (_, done) in new[..=i].iter().rev() {
                // SAFETY: attached (or failing to) on this thread just now.
                unsafe { done.detach() };
            }
            for (_, dll) in new {
                forget_image(dll.base());
                // SAFETY: detached, and no title code holds it yet.
                unsafe { dll.unload() };
            }
            Err(error)
        })
    }
}

/// The Windows loader lock's shape: one thread at a time, re-entered by
/// the thread that holds it.
#[cfg(windows)]
#[derive(Default)]
struct LoaderLock {
    owner: std::sync::Mutex<(Option<std::thread::ThreadId>, usize)>,
    free: std::sync::Condvar,
}

#[cfg(windows)]
impl LoaderLock {
    fn hold<T>(&self, f: impl FnOnce() -> T) -> T {
        let me = std::thread::current().id();
        let lock = || self.owner.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut owner = lock();
            while owner.0.is_some_and(|o| o != me) {
                owner = self.free.wait(owner).unwrap_or_else(|e| e.into_inner());
            }
            *owner = (Some(me), owner.1 + 1);
        }
        let result = f();
        let mut owner = lock();
        owner.1 -= 1;
        if owner.1 == 0 {
            owner.0 = None;
            self.free.notify_one();
        }
        result
    }
}

#[cfg(test)]
mod message_tests {
    use std::path::Path;

    use super::{absent_title_dlls, check_avx2, explain_commit_limit, missing_dlls_message};

    #[test]
    fn no_avx2_says_the_pc_cannot_run_it() {
        assert!(check_avx2(true).is_ok());
        let message = check_avx2(false).unwrap_err().to_string();
        assert!(message.contains("AVX2"), "{message}");
        assert!(message.contains("Haswell"), "{message}");
    }

    #[test]
    fn title_dlls_missing_from_the_folder_are_named_once() {
        let wanted = |dll: &str| {
            !["KERNEL32.dll", "d3d12_xs.dll"]
                .iter()
                .any(|h| h.eq_ignore_ascii_case(dll))
        };
        let imported = [
            "KERNEL32.dll",
            "d3d12_xs.dll",
            "One.dll",
            "two.dll",
            "Three.dll",
            "one.dll",
        ];
        let absent = absent_title_dlls(&imported, &[("TWO.DLL", vec![])], &wanted);
        assert_eq!(absent, ["One.dll", "Three.dll"]);
        let message = missing_dlls_message(&absent, Path::new("D:\\Game"));
        assert!(
            message.contains("One.dll, Three.dll are missing"),
            "{message}"
        );
        assert!(message.contains("D:\\Game"), "{message}");
        assert!(message.contains("LEONIDA_GAME_DIR"), "{message}");
        assert!(absent_title_dlls(&["One.dll"], &[("one.dll", vec![])], &wanted).is_empty());
    }

    #[test]
    fn error_1455_explains_the_paging_file() {
        let io = std::io::Error::from_raw_os_error(1455);
        let typed = explain_commit_limit(anyhow::Error::new(io).context("reserving"));
        let text = explain_commit_limit(anyhow::anyhow!("reserving: x (os error 1455)"));
        for error in [typed, text] {
            let message = format!("{error:#}");
            assert!(
                message.starts_with("Windows ran out of memory"),
                "{message}"
            );
            assert!(message.contains("paging file"), "{message}");
        }
        let other = explain_commit_limit(anyhow::anyhow!("reading x (os error 2)"));
        assert_eq!(other.to_string(), "reading x (os error 2)");
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::sync::{Arc, Mutex};

    #[test]
    fn loader_lock_is_reentrant_and_exclusive() {
        let lock = Arc::new(super::LoaderLock::default());
        assert_eq!(lock.hold(|| lock.hold(|| 7)), 7);
        let order = Arc::new(Mutex::new(Vec::new()));
        let other = lock.hold(|| {
            let (theirs, their_order) = (lock.clone(), order.clone());
            let other = std::thread::spawn(move || {
                theirs.hold(|| their_order.lock().unwrap().push(2));
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            order.lock().unwrap().push(1);
            other
        });
        other.join().unwrap();
        assert_eq!(*order.lock().unwrap(), [1, 2]);
    }
}
