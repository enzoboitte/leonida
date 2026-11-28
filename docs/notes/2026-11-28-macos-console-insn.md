# Console-only instructions on macOS

The console's Zen 2 has instructions the CPU Rosetta 2 presents lacks (`clzero`, `rdpru`,
`monitorx`/`mwaitx`, SSE4a's `extrq`/`insertq`, `rdpid`, `clwb`). On Windows the crash reporter's
vectored handler emulates them (`crates/leonida-loader/src/console_insn.rs`, from XCL). On macOS
the exception dispatcher (`seh.rs`) now asks the same emulation first for every illegal
instruction in title code, before the title's own handlers.

`console_insn.rs` reaches the host through a small `host` module: on Windows the calls it made
before (`VirtualQuery`, `VirtualProtect`, `QueryPerformanceCounter`, `GetCurrentThreadId`,
`GetCurrentProcessorNumber`, `CONTEXT`); on macOS `mach_vm_region`, `mprotect`,
`mach_absolute_time`, `pthread_threadid_np`, and a `CONTEXT` with Windows' field names laid out as
`unwind::Context` (checked at compile time). Nothing changes on Windows.

`shortcut:` macOS does not say which processor a thread runs on, so `rdpid` reads 0 there.

The emulation's own tests use Windows' APIs and run on Windows only.

## Tested

Apple M5, macOS 26.6, Rosetta 2, Rust 1.98.1, 2026-11-28. A fixture function runs `clzero`,
`monitorx`, `mwaitx` and `rdpru` under Rosetta: the 64-byte line is zeroed and only it, and `rdpru`
reads a counter. With the dispatcher's call to the emulation removed, the same test ends with
an unhandled exception (SIGABRT), so the instructions do fault under Rosetta and are emulated.

`cargo test -p leonida-loader --target x86_64-apple-darwin --locked --test macos_probe`: 10 passed.
Clippy with `-D warnings` is clean for `x86_64-apple-darwin`, `aarch64-apple-darwin` and, from the
Mac with `CARGO_FEATURE_PURE=1`, `x86_64-pc-windows-msvc` (`--all-features`). Not run on Windows.
