# macOS: the core of kernel32 for title code

On Windows a title gets most of `kernel32`, `kernelbase` and `ntdll` from the PC itself (host
pass-through, `crates/leonida-hle/src/host.rs`). macOS has no such DLLs, so
`crates/leonida-hle/src/modules/win32/` (macOS only) implements their core for title code, with
Windows' contracts: results, last errors, structure layouts.

| Area | Functions |
|------|-----------|
| memory | `VirtualAlloc/Free/Protect/Query` (64 KiB aligned reservations, commit, decommit to fresh zero pages, release with size 0, per-page state), `GetProcessHeap`, `HeapAlloc/ReAlloc/Free/Size/Create/Destroy` |
| handles and waits | events, mutexes (recursive, owned by a thread), semaphores, thread handles; `WaitForSingleObject(Ex)`, `WaitForMultipleObjects(Ex)` (any or all), `CloseHandle` |
| locks in title memory | critical sections (recursive, laid out as `RTL_CRITICAL_SECTION`), SRW locks, condition variables (SRW and CS), `WaitOnAddress`/`WakeByAddress*`, `InitOnceExecuteOnce` |
| threads | `CreateThread` (suspended or not, through the loader's `spawn_thread`, so DLLs hear of it), `ResumeThread`, `ExitThread`, `GetExitCodeThread`, ids, `Sleep(Ex)`, `SwitchToThread`, `ExitProcess` |
| TLS and FLS | `TlsAlloc/Free/GetValue/SetValue` in the TEB's 64 slots (`0x1480`) and its 1024 expansion slots (`0x1780`), `Fls*` |
| last error | `GetLastError`/`SetLastError` in the TEB (`0x68`), where title code may read it itself |
| system | `QueryPerformanceCounter/Frequency` (10 MHz), tick counts, file times, `GetSystemInfo` (x64, 4 KiB pages, 64 KiB granularity), `IsProcessorFeaturePresent`, command line, environment, `GetStartupInfoW` |
| text | `GetACP`, `GetCPInfo`, `MultiByteToWideChar`, `WideCharToMultiByte` |
| lists | `InitializeSListHead`, `InterlockedPush/Pop/FlushSList`, `QueryDepthSList` |
| exceptions | the loader's (`seh.rs`), served under these DLLs' names |
| standard handles | `GetStdHandle`, `GetFileType`, `WriteFile` and `WriteConsoleW` to the host's stdout and stderr |

The `shortcut:`s, each marked where it is: one lock and one wake-up for every handle object;
locks in title memory park on one condition variable; object names are ignored; a running thread
cannot be suspended or terminated, and `ExitThread` parks the thread; priorities and affinities
are accepted, not applied; FLS callbacks do not run; heaps are one allocator; the ANSI code page is
UTF-8 and other single-byte ones read as Latin-1; SLists take a lock; files other than the
standard handles need the macOS port of `files.rs`.

Not done: files and paths (`files.rs`), the console memory API (`xmem.rs`), and the C runtime. For
the C runtime the plan is to map Microsoft's own DLLs (`ucrtbase`, `vcruntime140`, `msvcp140`,
which the user supplies from the redistributable) as title DLLs: they then need only what this
module gives.

`HleResolver`'s strict-policy test now asks for `CreateWaitableTimerW`: `Sleep`, which it asked
for before, is served by this module on macOS.

## Tested

Apple M5, macOS 26.6, Rosetta 2, Rust 1.98.1, 2026-11-28. 18 tests of the module, each through
the functions as title code calls them: reservations, commit, decommit and release, with
`VirtualQuery` between; heap blocks; events (auto and manual), any and all waits, semaphores,
recursive mutexes; four threads incrementing one counter under nested critical sections and an
SRW lock (8,000 increments, none lost); condition variables with a timeout and a wake;
`WaitOnAddress`; one-time initialisation run once by three calls; suspended threads resumed,
with their exit codes; TLS values per thread, seen in the TEB, including an expansion slot; FLS;
the last error per thread in the TEB; conversions and their buffer-size conventions; SLists.

`cargo test -p leonida-hle -p leonida-gpu -p leonida-loader --target x86_64-apple-darwin --locked`:
154 passed, 6 ignored. Clippy with `-D warnings` is clean for `x86_64-apple-darwin`,
`aarch64-apple-darwin` and, from the Mac with `CARGO_FEATURE_PURE=1`, `x86_64-pc-windows-msvc`
(`--all-features`). Not run on Windows; nothing of it is built there.

## Files and paths

`modules::win32::files` (and `modules::files` on macOS, the same names as Windows' module):
virtual drives over host folders (`mount`, `mount_at`; `G:` makes `G:\` the current directory),
title paths turned into host paths inside their drive's folder (`\\?\`, `/`, `.`, `..`, trailing
dots and spaces as Windows treats them; `..` stops at the drive's root), and files as host
descriptors behind handles: `CreateFileW/A`, `CreateFile2` (every disposition, delete on close,
`ERROR_ALREADY_EXISTS` where Windows sets it), `ReadFile`/`WriteFile` (overlapped ones complete at
once, at their offset, with their event set, as Windows allows), file pointers, sizes,
`GetFileInformationByHandle(Ex)` (basic and standard), flush, end of file, attributes, folders,
delete, move, `FindFirstFile(Ex)W`/`FindNextFileW` with Windows' wildcards (and `.` and `..`
below a drive's root), full paths, the current directory, `GetTempPathW` (`T:\`), free space.

`shortcut:`s: a path on no mounted drive fails (`C:\` does not exist here); names compare as the
host volume does; attributes beyond read-only are not kept; no I/O completion ports.

`storage.rs` now mounts its folder on macOS too, and its test runs there. Four tests of the files
module (paths kept inside their drive, a file created, written, read with and without overlap,
found, moved and deleted, wildcards, full paths and the current directory): 159 passed, 5 ignored
with the loader and GPU crates (`--target x86_64-apple-darwin --locked`).
