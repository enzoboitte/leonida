# A loader for macOS: map, relocate, bind and run a stand-in title under Rosetta

Follows `2026-11-27-macos-rosetta-probe.md`. `leonida-loader` now has a mapper for x86-64 macOS
(`crates/leonida-loader/src/macos.rs`, under Rosetta 2 on Apple Silicon), where there is no
Windows loader to do that work:

- `MappedImage::load` maps the image (at its preferred base when that is free), copies the
  sections, applies the base relocations, sets the TLS index to 0, binds every import and
  delay-load import, then gives each section the protection it asks for. Binding goes through the
  same `ImportResolver` and unimplemented-import stubs as on Windows: the resolution loop moved
  from `windows.rs` into `thunks::resolve_imports`, shared by both.
- `enter_thread` gives the calling thread a TEB and a copy of the image's static TLS block,
  reached as Win64 code reaches them: `gs:0x30` (TEB `Self`) and `gs:0x58` (the TLS array). On
  macOS those are pthread TSD slots 6 and 11, as Wine uses them.
- The unimplemented-import handler is `extern "win64"` instead of `extern "system"`. Title code
  calls it with the Windows convention; on Windows x64 the two are the same, on macOS `"system"`
  is the System V one. Every function `leonida-hle` hands to title code will need the same change
  before it can serve a title on macOS.

Not done yet: Windows exceptions (a fault in title code is a plain signal), TLS callbacks and the
entry point (nothing calls them; the test calls exports), and the other TEB fields (marked
`shortcut:` in `enter_thread`).

## Tested

Apple M5, macOS 26.6, Rosetta 2, Rust 1.98.1, 2026-11-27.

`cargo test -p leonida-loader --target x86_64-apple-darwin --locked` builds a stand-in title,
`tests/fixtures/macos-probe` (our own `no_std` code, linked for Windows with `rust-lld` and
`raw-dylib` imports, so no Windows SDK is needed), and runs it: 3 tests pass. With its preferred
base taken first, the image is mapped elsewhere and relocated; it reaches a console import we
answer and a stub for a missing one; two threads each see their own TEB through `gs` and their own
copy of a static TLS variable, read the way MSVC code reads it. With the relocation step removed,
the test ends with a SIGSEGV, so it does check it.

Three `prepare` tests read the test binary as a PE image and are ignored off Windows, like the
`leonida-pe` ones.

Not run on Windows. Checked from the Mac instead:
`CARGO_FEATURE_PURE=1 cargo clippy --workspace --target x86_64-pc-windows-msvc --all-targets
[--all-features] --locked -- -D warnings` is clean (`CARGO_FEATURE_PURE` stops blake3 from
building its MSVC assembly, which needs `ml64.exe`). The Windows tests themselves still need CI or
a Windows PC.
