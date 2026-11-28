# `leonida run` on macOS

`apps/leonida/src/run_macos.rs`: on an x86-64 Mac build (Rosetta 2 on Apple Silicon),
`leonida run [--execute] <exe>` reads the title's executable, mounts its folder as `G:` and a
temporary folder as `T:`, maps the title DLLs it imports (from its folder, any case, each after
the ones it imports, unless `leonida-hle` serves that DLL) and the exe with
`leonida_loader::MappedImage`, binds their imports to `leonida-hle` and to each other, prints
what is missing, and with `--execute` runs the exe's entry point (`run_main`). There is no host
pass-through on macOS: `HostPolicy::none()`. The title's command line is `G:\<exe>`.

`translate` builds on macOS too: its DXIL validation and pipeline checks need Windows and say so
when asked for.

The README has the steps for a Mac, marked experimental: there is no picture without a Metal back
end.

## Measured: Rosetta's CPUID

Apple M5, macOS 26.6, Rosetta 2, 2026-11-28, an x86-64 program of ours: CPUID leaf 1 reports no
AVX, FMA or OSXSAVE, leaf 7 no AVX2 or BMI2, and XCR0 reads 0, so `is_x86_feature_detected!`
says none of them; yet a `vpaddd ymm` runs and gives the right lanes. So the AVX2 check of the
Windows run (`check_avx2`) would refuse every Mac; the macOS run has none. A title that reads
CPUID before using AVX takes its paths without it (inferred, not seen in a title).

## Tested

`apps/leonida/tests/macos_run.rs` runs the built `leonida` on our stand-in exe and its DLL (the
DLL copied under an upper-case name): the DLL is found and mapped, the exe's import from it is
bound (`1 title, 0 missing`), and the exe returns 142 (40, its TLS variable 2, and 100 because
its call into the DLL returned 13). The loader's own test still sees 42 for that exe, with the
import left to a stub.

`cargo test -p leonida -p leonida-hle -p leonida-gpu -p leonida-loader --target
x86_64-apple-darwin --locked`: 164 passed, 5 ignored. Clippy with `-D warnings` is clean for
`x86_64-apple-darwin`, `aarch64-apple-darwin` and, from the Mac with `CARGO_FEATURE_PURE=1`,
`x86_64-pc-windows-msvc` (`--all-features`). Not run on Windows.
