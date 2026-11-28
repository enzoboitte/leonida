//! `leonida run --execute` on macOS, end to end, on stand-in titles of our
//! own (tests/fixtures/macos-probe): an exe and the DLL beside it.
#![cfg(all(target_os = "macos", target_arch = "x86_64"))]

use std::path::Path;
use std::process::Command;

#[test]
fn an_exe_and_its_title_dll_run() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("macos-probe");
    let built = Command::new(env!("CARGO"))
        .args([
            "build",
            "--quiet",
            "--release",
            "--target",
            "x86_64-pc-windows-msvc",
        ])
        .arg("--manifest-path")
        .arg(root.join("tests/fixtures/macos-probe/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .env("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER", "rust-lld")
        .status()
        .expect("running cargo");
    assert!(built.success(), "building macos-probe failed");
    let out = target.join("x86_64-pc-windows-msvc/release");

    let dir = tempfile::tempdir().unwrap();
    let title = dir.path().join("title");
    std::fs::create_dir(&title).unwrap();
    std::fs::copy(out.join("probe_exe.exe"), title.join("probe_exe.exe")).unwrap();
    std::fs::copy(out.join("macos_probe.dll"), title.join("MACOS_PROBE.DLL")).unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_leonida"))
        .args(["run", "--execute", "--unimplemented", "zero"])
        .arg(title.join("probe_exe.exe"))
        .env("LEONIDA_DATA_DIR", dir.path().join("data"))
        .env("LEONIDA_CACHE_DIR", dir.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        stdout.contains("title dll      macos_probe.dll"),
        "{stdout}"
    );
    assert!(
        stdout.contains("bound          probe_exe.exe: 0 hle, 1 title, 0 missing"),
        "{stdout}"
    );
    assert!(stdout.contains("title exited with 142"), "{stdout}");
}
