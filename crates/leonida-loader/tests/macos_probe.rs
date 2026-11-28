//! End-to-end on macOS: map, relocate, bind and call into a stand-in title
//! (tests/fixtures/macos-probe), without any game data. Runs in an x86-64
//! process: `cargo test -p leonida-loader --target x86_64-apple-darwin`
//! (under Rosetta 2 on Apple Silicon).
#![cfg(all(target_os = "macos", target_arch = "x86_64"))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use leonida_loader::{
    ImportResolver, MappedImage, Provenance, Resolution, UnimplementedPolicy, disable_thread_calls,
    enter_thread, seh_export, spawn_thread, unimplemented_call_count,
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
        let name = symbol.to_string();
        if let Some(address) = seh_export(dll, &name) {
            let provenance = Provenance::Hle;
            return Resolution::Found {
                address,
                provenance,
            };
        }
        match (dll.to_ascii_lowercase().as_str(), name.as_str()) {
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
        assert_eq!(
            report.hle, 3,
            "XGameRuntimeInitialize, RaiseException, RtlUnwindEx"
        );
        assert_ne!(image.base() as u64, preferred, "mapped elsewhere");
        enter_thread();
        // SAFETY: imports are bound, and this thread has a TEB.
        unsafe { image.attach() }.unwrap();
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
    probe();
    let teb: extern "win64" fn() -> u64 = export("probe_teb");
    let bump: extern "win64" fn() -> u32 = export("probe_tls_bump");

    let here = enter_thread();
    assert_eq!(enter_thread(), here, "once per thread");
    assert_eq!(teb(), here as u64);
    assert_eq!(bump(), 6, "a fresh copy of the template (5)");
    assert_eq!(bump(), 7);

    let there = std::thread::spawn(move || {
        let there = enter_thread();
        assert_eq!(teb(), there as u64);
        assert_eq!(bump(), 6, "not this thread's neighbour's block");
        there
    })
    .join()
    .unwrap();
    assert_ne!(here, there);
    assert_eq!(bump(), 8);
}

#[repr(C)]
struct ExceptionRecord {
    code: u32,
    flags: u32,
    record: u64,
    address: u64,
    parameters: u32,
    info: [u64; 15],
}

#[repr(C)]
struct ExceptionPointers {
    record: *mut ExceptionRecord,
    context: *mut u8,
}

/// The fault `probe_fault_read` makes on purpose, and nothing else.
const VECTORED_TARGET: u64 = 0x20;

/// Steps over the faulting `mov rax, [rcx]` (3 bytes) with rax = 0x77.
extern "win64" fn step_over_read(pointers: *mut ExceptionPointers) -> i32 {
    // SAFETY: dispatch passes a record and a CONTEXT.
    unsafe {
        let record = &*(*pointers).record;
        if record.code != 0xC000_0005 || record.info[1] != VECTORED_TARGET {
            return 0; // EXCEPTION_CONTINUE_SEARCH
        }
        let ctx = (*pointers).context;
        *(ctx.add(0xF8) as *mut u64) += 3; // Rip
        *(ctx.add(0x78) as *mut u64) = 0x77; // Rax
    }
    -1 // EXCEPTION_CONTINUE_EXECUTION
}

#[test]
fn a_vectored_handler_resumes_after_a_fault() {
    let add = seh_export("kernel32.dll", "AddVectoredExceptionHandler").unwrap();
    // SAFETY: our AddVectoredExceptionHandler.
    let add: extern "win64" fn(u32, extern "win64" fn(*mut ExceptionPointers) -> i32) -> usize =
        unsafe { std::mem::transmute(add) };
    add(1, step_over_read);
    let read: extern "win64" fn(u64) -> u64 = export("probe_fault_read");
    assert_eq!(read(VECTORED_TARGET), 0x77);
}

#[test]
fn a_frame_handler_catches_a_fault_and_raised_exceptions_and_unwinds() {
    let guarded: extern "win64" fn(u64) -> u64 = export("probe_seh");
    let seen: extern "win64" fn() -> u64 = export("probe_seh_seen");
    // The landing computes rbx + 2: 0x1113 only with the outer frame's rbx.
    assert_eq!(guarded(0x10), 0x1113, "an access violation");
    assert_eq!(seen(), 1 << 32 | 0xC000_0005, "one inner frame unwound");
    assert_eq!(guarded(0), 0x1113, "RaiseException");
    assert_eq!(seen(), 2 << 32 | 0xE000_1234);
}

/// In a child process of the next test: a fault nothing handles.
#[test]
fn unhandled_child() {
    if std::env::var_os("LEONIDA_UNHANDLED_CHILD").is_none() {
        return;
    }
    let read: extern "win64" fn(u64) -> u64 = export("probe_fault_read");
    read(0x30);
}

#[test]
fn an_unhandled_fault_is_named_and_ends_the_process() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "unhandled_child", "--nocapture"])
        .env("LEONIDA_UNHANDLED_CHILD", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    let named = text.contains("unhandled exception 0xc0000005 at title+")
        && text.contains("accessing 0x30");
    assert!(named, "stderr: {text}");
}

/// Counts of the fixture's notices: `source` 0 is its TLS callback, 1 its
/// `DllMain`.
fn notices(source: u32, reason: u32) -> u32 {
    let count: extern "win64" fn(u32, u32) -> u32 = export("probe_notices");
    count(source, reason)
}

const PROCESS_ATTACH: u32 = 1;
const THREAD_ATTACH: u32 = 2;
const THREAD_DETACH: u32 = 3;

#[test]
fn an_attached_dll_hears_of_its_attach_and_of_title_threads() {
    let image = &probe().image;
    assert_eq!(notices(0, PROCESS_ATTACH), 1, "TLS callback");
    assert_eq!(notices(1, PROCESS_ATTACH), 1, "DllMain");

    let thread = image.export("probe_thread").unwrap();
    let before = [0, 1].map(|s| [THREAD_ATTACH, THREAD_DETACH].map(|r| notices(s, r)));
    // SAFETY: `probe_thread` is a thread procedure.
    let joined = unsafe { spawn_thread(thread, 100, 0) }.unwrap().join();
    assert_eq!(
        joined.unwrap(),
        100 + 6,
        "its parameter, and a fresh TLS block"
    );
    for (s, [attach, detach]) in [0, 1].into_iter().zip(before) {
        assert_eq!(notices(s, THREAD_ATTACH), attach + 1, "source {s}");
        assert_eq!(notices(s, THREAD_DETACH), detach + 1, "source {s}");
    }

    assert!(disable_thread_calls(image.base()));
    let dll_main = notices(1, THREAD_ATTACH);
    let callback = notices(0, THREAD_ATTACH);
    // SAFETY: as above.
    unsafe { spawn_thread(thread, 0, 0) }
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(notices(1, THREAD_ATTACH), dll_main, "DllMain is quiet now");
    assert_eq!(
        notices(0, THREAD_ATTACH),
        callback + 1,
        "TLS callbacks still hear"
    );
}

#[test]
fn an_exe_runs_with_its_static_tls_in_slot_0() {
    probe(); // the DLL first, so it takes slot 1
    let exe = build_probe().with_file_name("probe_exe.exe");
    let bytes = std::fs::read(exe).unwrap();
    let (exe, _) =
        MappedImage::load(&bytes, &TestResolver, UnimplementedPolicy::ReturnZero).unwrap();
    assert!(!exe.info().is_dll());
    // SAFETY: the exe imports nothing.
    let main = unsafe { exe.run_main(Vec::new()) }.unwrap();
    assert_eq!(main.join().unwrap().unwrap(), 40 + 2);
    // The DLL, mapped first, still reads its own slot.
    let bump: extern "win64" fn() -> u32 = export("probe_tls_bump");
    std::thread::spawn(move || {
        enter_thread();
        assert_eq!(bump(), 6);
    })
    .join()
    .unwrap();
}
