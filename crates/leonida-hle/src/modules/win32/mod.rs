//! The Windows API a title gets from the host's own DLLs on Windows
//! (`kernel32`, `kernelbase`, `ntdll`, their API sets), implemented for
//! macOS, where there is no such host: memory, threads and waits, locks in
//! title memory, TLS, clocks, the environment, code pages. A runtime the
//! title brings or loads (the C runtime DLLs) sits on top of this.
//!
//! Every function keeps Windows' contract: its results, its last error
//! (in the TEB, where title code may read it), its structure layouts.
// As in `files.rs` on Windows: the exports take the title's pointers by contract.
#![allow(clippy::not_unsafe_ptr_arg_deref, clippy::too_many_arguments)]

pub mod files;
mod memory;
mod sync;
mod system;
mod thread;

pub use system::set_command_line;

use thread::current_thread_id;

use crate::{HleExport, HleModule, hle_export};

/// `TEB.LastErrorValue`.
const TEB_LAST_ERROR: usize = 0x68;

/// This thread's TEB, made the first time.
fn teb() -> *mut u8 {
    leonida_loader::enter_thread() as *mut u8
}

fn set_last_error(code: u32) {
    // SAFETY: inside this thread's TEB.
    unsafe { teb().add(TEB_LAST_ERROR).cast::<u32>().write(code) };
}

#[allow(non_snake_case)]
extern "win64" fn GetLastError() -> u32 {
    // SAFETY: inside this thread's TEB.
    unsafe { teb().add(TEB_LAST_ERROR).cast::<u32>().read() }
}

#[allow(non_snake_case)]
extern "win64" fn SetLastError(code: u32) {
    set_last_error(code);
}

/// The exception functions live in the loader, beside the dispatcher.
const SEH: [&str; 10] = [
    "RtlCaptureContext",
    "RtlRestoreContext",
    "RtlUnwindEx",
    "RtlUnwind",
    "RaiseException",
    "RtlRaiseException",
    "RtlLookupFunctionEntry",
    "RtlVirtualUnwind",
    "AddVectoredExceptionHandler",
    "RemoveVectoredExceptionHandler",
];

pub fn module() -> HleModule {
    use files::*;
    use memory::*;
    use sync::*;
    use system::*;
    use thread::*;
    let mut exports = vec![
        hle_export!(GetLastError),
        hle_export!(SetLastError),
        // memory
        hle_export!(VirtualAlloc),
        hle_export!(VirtualFree),
        hle_export!(VirtualProtect),
        hle_export!(VirtualQuery),
        hle_export!(GetProcessHeap),
        hle_export!(HeapCreate),
        hle_export!(HeapDestroy),
        hle_export!(HeapAlloc),
        hle_export!(HeapFree),
        hle_export!(HeapSize),
        hle_export!(HeapReAlloc),
        // handles and waits
        hle_export!(WaitForSingleObject),
        hle_export!(WaitForSingleObjectEx),
        hle_export!(WaitForMultipleObjects),
        hle_export!(WaitForMultipleObjectsEx),
        hle_export!(CloseHandle),
        hle_export!(CreateEventW),
        hle_export!(CreateEventA),
        hle_export!(CreateEventExW),
        hle_export!(SetEvent),
        hle_export!(ResetEvent),
        hle_export!(CreateMutexW),
        hle_export!(CreateMutexA),
        hle_export!(CreateMutexExW),
        hle_export!(ReleaseMutex),
        hle_export!(CreateSemaphoreW),
        hle_export!(CreateSemaphoreExW),
        hle_export!(ReleaseSemaphore),
        // locks in title memory
        hle_export!(InitializeCriticalSection),
        hle_export!(InitializeCriticalSectionAndSpinCount),
        hle_export!(InitializeCriticalSectionEx),
        hle_export!(DeleteCriticalSection),
        hle_export!(EnterCriticalSection),
        hle_export!(TryEnterCriticalSection),
        hle_export!(LeaveCriticalSection),
        hle_export!(InitializeSRWLock),
        hle_export!(AcquireSRWLockExclusive),
        hle_export!(TryAcquireSRWLockExclusive),
        hle_export!(ReleaseSRWLockExclusive),
        hle_export!(AcquireSRWLockShared),
        hle_export!(TryAcquireSRWLockShared),
        hle_export!(ReleaseSRWLockShared),
        hle_export!(InitializeConditionVariable),
        hle_export!(WakeConditionVariable),
        hle_export!(WakeAllConditionVariable),
        hle_export!(SleepConditionVariableSRW),
        hle_export!(SleepConditionVariableCS),
        hle_export!(WaitOnAddress),
        hle_export!(WakeByAddressSingle),
        hle_export!(WakeByAddressAll),
        hle_export!(InitOnceExecuteOnce),
        hle_export!(InitOnceInitialize),
        // threads
        hle_export!(GetCurrentThreadId),
        hle_export!(GetCurrentProcessId),
        hle_export!(GetCurrentThread),
        hle_export!(GetCurrentProcess),
        hle_export!(CreateThread),
        hle_export!(ResumeThread),
        hle_export!(SuspendThread),
        hle_export!(ExitThread),
        hle_export!(TerminateThread),
        hle_export!(GetExitCodeThread),
        hle_export!(GetThreadId),
        hle_export!(SetThreadPriority),
        hle_export!(GetThreadPriority),
        hle_export!(SetThreadAffinityMask),
        hle_export!(SetThreadIdealProcessor),
        hle_export!(SetThreadDescription),
        hle_export!(Sleep),
        hle_export!(SleepEx),
        hle_export!(SwitchToThread),
        hle_export!(ExitProcess),
        hle_export!(TlsAlloc),
        hle_export!(TlsFree),
        hle_export!(TlsGetValue),
        hle_export!(TlsSetValue),
        hle_export!(FlsAlloc),
        hle_export!(FlsFree),
        hle_export!(FlsGetValue),
        hle_export!(FlsSetValue),
        // files
        hle_export!(GetStdHandle),
        hle_export!(GetFileType),
        hle_export!(CreateFileW),
        hle_export!(CreateFileA),
        hle_export!(CreateFile2),
        hle_export!(ReadFile),
        hle_export!(WriteFile),
        hle_export!(WriteConsoleW),
        hle_export!(GetOverlappedResult),
        hle_export!(SetFilePointer),
        hle_export!(SetFilePointerEx),
        hle_export!(GetFileSize),
        hle_export!(GetFileSizeEx),
        hle_export!(GetFileInformationByHandle),
        hle_export!(GetFileInformationByHandleEx),
        hle_export!(FlushFileBuffers),
        hle_export!(SetEndOfFile),
        hle_export!(GetFileAttributesW),
        hle_export!(GetFileAttributesA),
        hle_export!(GetFileAttributesExW),
        hle_export!(SetFileAttributesW),
        hle_export!(CreateDirectoryW),
        hle_export!(CreateDirectoryA),
        hle_export!(RemoveDirectoryW),
        hle_export!(DeleteFileW),
        hle_export!(DeleteFileA),
        hle_export!(MoveFileExW),
        hle_export!(GetFullPathNameW),
        hle_export!(GetCurrentDirectoryW),
        hle_export!(SetCurrentDirectoryW),
        hle_export!(GetTempPathW),
        hle_export!(GetDiskFreeSpaceExW),
        hle_export!(FindFirstFileW),
        hle_export!(FindFirstFileExW),
        hle_export!(FindNextFileW),
        hle_export!(FindClose),
        hle_export!(GetLogicalDrives),
        // system
        hle_export!(QueryPerformanceCounter),
        hle_export!(QueryPerformanceFrequency),
        hle_export!(GetTickCount),
        hle_export!(GetTickCount64),
        hle_export!(GetSystemTimeAsFileTime),
        hle_export!(GetSystemTimePreciseAsFileTime),
        hle_export!(GetSystemInfo),
        hle_export!(GetNativeSystemInfo),
        hle_export!(IsProcessorFeaturePresent),
        hle_export!(IsDebuggerPresent),
        hle_export!(GetCommandLineW),
        hle_export!(GetCommandLineA),
        hle_export!(GetEnvironmentVariableW),
        hle_export!(GetEnvironmentVariableA),
        hle_export!(SetEnvironmentVariableW),
        hle_export!(GetEnvironmentStringsW),
        hle_export!(FreeEnvironmentStringsW),
        hle_export!(GetStartupInfoW),
        hle_export!(GetACP),
        hle_export!(GetOEMCP),
        hle_export!(IsValidCodePage),
        hle_export!(GetCPInfo),
        hle_export!(MultiByteToWideChar),
        hle_export!(WideCharToMultiByte),
        hle_export!(InitializeSListHead),
        hle_export!(InterlockedPushEntrySList),
        hle_export!(InterlockedPopEntrySList),
        hle_export!(InterlockedFlushSList),
        hle_export!(QueryDepthSList),
    ];
    for name in SEH {
        if let Some(address) = leonida_loader::seh_export("kernel32.dll", name) {
            exports.push(HleExport { name, address });
        }
    }
    HleModule {
        name: "win32",
        dlls: &[
            "kernel32.dll",
            "kernelbase.dll",
            "ntdll.dll",
            "api-ms-win-core-*",
        ],
        exports,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_error_is_per_thread_and_in_the_teb() {
        SetLastError(1234);
        assert_eq!(GetLastError(), 1234);
        // SAFETY: the field title code reads itself.
        assert_eq!(
            unsafe { teb().add(TEB_LAST_ERROR).cast::<u32>().read() },
            1234
        );
        assert_eq!(std::thread::spawn(|| GetLastError()).join().unwrap(), 0);
    }

    #[test]
    fn the_module_serves_kernel32_and_its_api_sets() {
        let m = module();
        assert!(m.serves("KERNEL32.dll") && m.serves("api-ms-win-core-synch-l1-2-0.dll"));
        let names: Vec<_> = m.exports.iter().map(|e| e.name).collect();
        for name in [
            "CreateThread",
            "VirtualAlloc",
            "RaiseException",
            "WaitOnAddress",
        ] {
            assert!(names.contains(&name), "{name}");
        }
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "no name twice");
    }
}
