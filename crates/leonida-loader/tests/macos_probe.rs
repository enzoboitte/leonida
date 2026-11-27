//! End-to-end on macOS: map, relocate, bind and call into a stand-in title
//! (tests/fixtures/macos-probe), without any game data. Runs in an x86-64
//! process: `cargo test -p leonida-loader --target x86_64-apple-darwin`
//! (under Rosetta 2 on Apple Silicon).
#![cfg(all(target_os = "macos", target_arch = "x86_64"))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use leonida_loader::{
    ImportResolver, MappedImage, Provenance, Resolution, UnimplementedPolicy, enter_thread,
    unimplemented_call_count,
};
use leonida_pe::ImportSymbol;

/// Builds the fixture for Windows with rust-lld (it needs no Windows SDK),
/// into the outer build's scratch folder.
fn build_probe() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = root.join("tests/fixtures/macos-probe/Cargo.toml");
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("macos-probe");
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--quiet",
            "--release",
            "--target",
            "x86_64-pc-windows-msvc",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target)
        .env("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER", "rust-lld")
        .status()
        .expect("running cargo");
    assert!(status.success(), "building macos-probe failed");
    target.join("x86_64-pc-windows-msvc/release/macos_probe.dll")
}

extern "win64" fn fake_xgameruntime_initialize() -> i32 {
    0x1234
}

struct TestResolver;

impl ImportResolver for TestResolver {
    fn resolve(&self, dll: &str, symbol: &ImportSymbol) -> Resolution {
        match (
            dll.to_ascii_lowercase().as_str(),
            symbol.to_string().as_str(),
        ) {
            ("xgameruntime.dll", "XGameRuntimeInitialize") => Resolution::Found {
                address: fake_xgameruntime_initialize as *const () as usize,
                provenance: Provenance::Hle,
            },
            _ => Resolution::Missing,
        }
    }
}

struct Probe {
    image: MappedImage,
    missing: Vec<String>,
}

/// Loaded once: the image stays mapped for the process's life.
fn probe() -> &'static Probe {
    static PROBE: OnceLock<Probe> = OnceLock::new();
    PROBE.get_or_init(|| {
        let bytes = std::fs::read(build_probe()).unwrap();
        // Take the preferred base first, so the loader has to relocate.
        let preferred = leonida_pe::ImageInfo::parse(&bytes).unwrap().image_base;
        let (prot, flags) = (
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
        );
        // SAFETY: nothing of ours lives at the image's preferred base.
        let taken = unsafe { libc::mmap(preferred as *mut _, 0x1000, prot, flags, -1, 0) };
        assert_eq!(taken as u64, preferred);
        let (image, report) =
            MappedImage::load(&bytes, &TestResolver, UnimplementedPolicy::ReturnZero).unwrap();
        assert_eq!(report.hle, 1);
        assert_ne!(image.base() as u64, preferred, "mapped elsewhere");
        let missing = report
            .missing
            .iter()
            .map(|m| format!("{}!{}", m.dll, m.symbol))
            .collect();
        Probe { image, missing }
    })
}

fn export<T: Copy>(name: &str) -> T {
    let address = probe()
        .image
        .export(name)
        .unwrap_or_else(|| panic!("no export {name}"));
    assert_eq!(size_of::<T>(), size_of::<usize>());
    // SAFETY: `T` is the export's function pointer type, as the fixture declares it.
    unsafe { std::mem::transmute_copy(&address) }
}

#[test]
fn the_image_is_relocated_and_has_static_tls() {
    let probe = probe();
    assert!(probe.image.info().has_relocations);
    assert!(
        probe.image.info().tls.is_some(),
        "the fixture's _tls_used is its TLS directory"
    );
    let relocated: extern "win64" fn() -> usize = export("probe_relocated");
    assert_eq!(relocated(), 3 + 5 + 5);
}

#[test]
fn imports_reach_our_functions_and_stubs() {
    assert_eq!(
        probe().missing,
        ["leonida-probe-missing.dll!LeonidaProbeNotImplemented"]
    );
    let init: extern "win64" fn() -> i32 = export("probe_xgameruntime_init");
    assert_eq!(init(), 0x1234);
    let before = unimplemented_call_count();
    let missing: extern "win64" fn() -> u64 = export("probe_missing");
    assert_eq!(missing(), 0, "ReturnZero");
    assert!(unimplemented_call_count() > before);
}

#[test]
fn each_thread_gets_its_own_teb_and_tls_block() {
    let image = &probe().image;
    let teb: extern "win64" fn() -> u64 = export("probe_teb");
    let bump: extern "win64" fn() -> u32 = export("probe_tls_bump");

    let here = enter_thread(image);
    assert_eq!(enter_thread(image), here, "once per thread");
    assert_eq!(teb(), here as u64);
    assert_eq!(bump(), 6, "a fresh copy of the template (5)");
    assert_eq!(bump(), 7);

    let there = std::thread::spawn(move || {
        let there = enter_thread(&probe().image);
        assert_eq!(teb(), there as u64);
        assert_eq!(bump(), 6, "not this thread's neighbour's block");
        there
    })
    .join()
    .unwrap();
    assert_ne!(here, there);
    assert_eq!(bump(), 8);
}
