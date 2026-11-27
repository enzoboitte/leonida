# A Windows x64 image runs inside an x86_64 macOS process under Rosetta

Research for a native macOS port, not part of the build. Can the loader's model (map the title's
image into our process, bind its imports to our functions, jump to its entry point) work on Apple
Silicon, with Rosetta 2 translating the whole x86_64 process? A prototype answered the questions
about the CPU side. No game data was used: the "title" is a `no_std` Windows image built from our own
code.

## Set-up

Apple M5, macOS 26.6, Rosetta 2, Rust 1.98.1, 2026-11-27.

- **Title:** `#![no_std] #![no_main]`, target `x86_64-pc-windows-msvc`, linked with `rust-lld`
  (`-C linker-flavor=lld-link /ENTRY:start /NODEFAULTLIB /FIXED`). Its `kernel32` imports
  (`GetStdHandle`, `WriteFile`, `ExitProcess`) are declared `kind = "raw-dylib"`, so no Windows
  import library or SDK is needed to build it on a Mac.
- **Host:** a Rust program for `x86_64-apple-darwin`. It parses the image with `leonida-pe`, maps it
  at its preferred base with `mmap`, copies the sections, writes our `extern "win64"` functions into
  the import address table, sets each section's protection with `mprotect`, and calls the entry point.

## Measured

| Question | Result |
|----|----|
| Does Win64 code run when mapped by hand in a Rosetta process? | yes |
| Can it reach its TEB through `gs`? | yes. macOS keeps the pthread TSD array at `gs`, so `gs:0x30` (TEB `Self`) and `gs:0x58` (TLS array) are TSD slots 6 and 11; the host writes its TEB and TLS pointers there, as Wine does on macOS. The title read both back correctly |
| Do calls through the IAT into the host work? | yes, with the host's functions declared `extern "win64"` |
| Does AVX2 work? | yes: a `vpaddd ymm` in the title gave the right lanes |
| Can a faulting instruction be caught and stepped over, as the Windows loader does with exceptions? | yes: `monitorx` raises `SIGILL` under Rosetta; a `SA_SIGINFO` handler read the bytes at `__rip`, added 3 and the title went on |
| Metal | the M5 reports Metal 4. Apple's Metal Shader Converter (DXIL to Metal) is not installed here, so nothing was tested on the GPU |

## Still open

Each of these is its own piece of work, and none was tried:

- **Relocations, TLS callbacks, static TLS slots** for a real image (the prototype's has none, so it
  loads at its preferred base).
- **Windows exceptions (SEH):** a signal has to become an `EXCEPTION_RECORD` and `CONTEXT`, go through
  vectored handlers, then unwind with the image's `.pdata`. The title's own `try`/`catch` and
  `__fastfail` depend on it.
- **The host pass-through:** on Windows many of the title's imports go to the PC's own `kernel32`,
  CRT and `ntdll`. On macOS every one of them needs an implementation (files, threads with a TEB each,
  waits, virtual memory with Windows' reserve and commit rules). The prototype's `mprotect` calls
  on 4 KiB boundaries succeeded under Rosetta although Apple Silicon pages are 16 KiB; how far that
  holds was not tested.
- **The GPU:** `leonida-gpu` calls host D3D12 directly (44 files). A Metal back end is the largest
  part. Whether Metal Shader Converter accepts the DXIL `leonida-shader` makes (mesh shaders, wave
  ops, ray queries) is unknown.
- **Memory:** the Windows build uses about 24 GB in play (TODO P-8); a 32 GB Mac shares that with the
  GPU.
