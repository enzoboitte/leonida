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

## Console memory (`xmem.dll`)

`modules::win32::xmem` (macOS) serves `xmem.dll` with the contracts of `modules::xmem`:
`XMemAlloc`/`XMemFree` through the title's `__XMemAllocationHooks__` (the runner passes the
export's address) or the default allocator with the attributes' alignment; 32-bit allocations
below 4 GiB; `XMemVirtualAlloc` with 2 MiB alignment and the address ranges of the flags (from
4 GiB up to 40 bits by default, as decision 0001 keeps them); `XMemVirtualQuery`; physical pages
as 64 KiB slices of host shared-memory objects (`shm_open`, unlinked at once), mapped with
`mmap(MAP_SHARED | MAP_FIXED)` so that a page mapped at two addresses shows the same bytes, and
unmapped back to no access; working set statistics and memory transfers.

It is a separate implementation, not Windows' made portable: Windows' is tied to the GPU side
(write-combined memory, write watches, remapped texture views, `map_alias`), which has no macOS
back end, and it is the measured memory core of the Windows build, which cannot be run from a
Mac. Merge the two once a Metal back end needs the same hooks. `shortcut:` "used" in the working
set is the process's peak resident size.

**Measured: memory below 4 GiB.** A 64-bit macOS executable reserves its first 4 GiB
(`__PAGEZERO`), so no allocation can land there, and the title's 32-bit graphics memory failed.
`apps/leonida` and `leonida-hle` now link with `-pagezero_size 0x10000` on macOS (their
`build.rs`), as Wine does; under Rosetta 2 on macOS 26.6 a 32-bit allocation then lands below
4 GiB.

Three tests: alignment, 32-bit and 40-bit ranges, 2 MiB alignment and the query's flags; two
physical pages mapped at two reservations, a byte written through one address read through the
other, unmapped, freed pages still readable where mapped; the title's hooks taking over
`XMemAlloc`. With the rest, `--target x86_64-apple-darwin --locked`: 167 passed, 5 ignored.

## Network and device type

`modules::win32::net` (macOS) serves `ws2_32.dll` and `winhttp.dll` as a console with its cable
out: `WSAStartup` succeeds, creating a socket fails with `WSAENETDOWN` (so every call on one fails
with `WSAENOTSOCK`), name lookups fail with `WSAHOST_NOT_FOUND`, a WinHTTP session opens but
`WinHttpConnect` fails with `ERROR_WINHTTP_CANNOT_CONNECT`, proxy detection fails. Winsock's
well-known ordinals are served too. Nothing can reach a network, which keeps "offline by design".
`shortcut:` no loopback sockets either (Windows' `modules::net` allows loopback).

`modules::xsystem`'s registry part (the console model titles read from
`HKLM\...\OEM\ConsoleMode`) now builds on macOS: keys and values that are not ours are missing
there, where Windows hands them to the host's registry. Its test of that hand-over is ignored
off Windows.

## Thread pool and the rest the game's DLLs import

Run on the game's own DLLs (`leonida census --strict` over a user's Xbox Series copy, read where
it lies; only import names are written here), 17 images, 625 functions: 204 were served on macOS
before this, 266 after. Added (`modules::win32::pool` and `misc`): Windows' thread pool (work
items, timers with periods and cancelling, waits on handles, `QueueUserWorkItem`) on loader
threads, `EncodePointer`/`DecodePointer`, `UnhandledExceptionFilter`, `RtlPcToFileHeader` (from
the loader's image list, `leonida_loader::image_base`), `LocalAlloc`/`LocalFree`/`GlobalFree`,
`FormatMessageW/A` (`shortcut:` "Error <n>" for system messages), `DebugBreak`,
`RaiseFailFastException`, strings and locale (`lstrlenW`, `lstrcmpiW`, `CompareStringEx`,
`GetStringType(Ex)W`, `LCMapStringEx`, `GetLocaleInfoEx` for the name, `shortcut:` en-US and
ordinal comparisons), ETW providers as a console with no tracing session, and app state
notification registration (`shortcut:` nothing sends a notice yet).

What is left of the 359 missing: the C runtime's API sets (from `ucrtbase.dll`, which the game
does not ship), `msvcp140`/`vcruntime140` (the game ships them; `leonida run` maps them as title
DLLs, which the census does not count), WinRT and COM, `crypt32`, `oleaut32`, `xaudio2_9`, more
of WinHTTP, classic ETW, and a few file and process functions.

**Mapped from the game's folder**, without running any of it (`leonida run` on `msvcp140.dll`,
no `--execute`): `vcruntime140.dll` and `vcruntime140_1.dll` mapped first, `msvcp140.dll` away from
its preferred base (so its relocations applied), 134 imports bound to us, 22 between the three,
106 missing: 101 of them the C runtime's, 5 rare file functions. The game's executable was not in
that copy, so the game itself was not started.

7 more tests (work items, timers, waits; strings; messages and local blocks).
