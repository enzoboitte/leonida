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

mod com;
pub mod files;
mod memory;
mod misc;
pub mod more;
pub mod net;
mod pool;
mod sync;
mod system;
mod thread;
pub mod xmem;

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
    use com::*;
    use files::*;
    use memory::*;
    use misc::*;
    use more::*;
    use pool::*;
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
        hle_export!(SetFileTime),
        hle_export!(CopyFile2),
        hle_export!(CreateHardLinkW),
        hle_export!(CreateSymbolicLinkW),
        hle_export!(DuplicateHandle),
        hle_export!(GetModuleHandleExW),
        // COM and WinRT
        hle_export!(CoTaskMemAlloc),
        hle_export!(CoTaskMemFree),
        hle_export!(CoCreateGuid),
        hle_export!(CoCreateInstance),
        hle_export!(CoGetApartmentType),
        hle_export!(CoGetContextToken),
        hle_export!(CoGetObjectContext),
        hle_export!(CoCreateFreeThreadedMarshaler),
        hle_export!(CoMarshalInterThreadInterfaceInStream),
        hle_export!(CoGetInterfaceAndReleaseStream),
        hle_export!(CoAddRefServerProcess),
        hle_export!(CoReleaseServerProcess),
        hle_export!(PropVariantClear),
        hle_export!(RoInitialize),
        hle_export!(RoUninitialize),
        hle_export!(RoGetActivationFactory),
        hle_export!(RoRegisterActivationFactories),
        hle_export!(RoRevokeActivationFactories),
        hle_export!(RoGetApartmentIdentifier),
        hle_export!(RoRegisterForApartmentShutdown),
        hle_export!(RoUnregisterForApartmentShutdown),
        hle_export!(RoOriginateError),
        hle_export!(RoTransformError),
        hle_export!(GetRestrictedErrorInfo),
        hle_export!(SetRestrictedErrorInfo),
        hle_export!(WindowsCreateString),
        hle_export!(WindowsCreateStringReference),
        hle_export!(WindowsDeleteString),
        hle_export!(WindowsDuplicateString),
        hle_export!(WindowsGetStringLen),
        hle_export!(WindowsGetStringRawBuffer),
        hle_export!(WindowsIsStringEmpty),
        hle_export!(WindowsStringHasEmbeddedNull),
        hle_export!(WindowsCompareStringOrdinal),
        // certificates, classic tracing, resources, threads, processors
        hle_export!(CertOpenStore),
        hle_export!(CertOpenSystemStoreW),
        hle_export!(CertCloseStore),
        hle_export!(CertFindCertificateInStore),
        hle_export!(CertFindChainInStore),
        hle_export!(CertFreeCertificateContext),
        hle_export!(CertFreeCertificateChain),
        hle_export!(CryptStringToBinaryW),
        hle_export!(RegisterTraceGuidsW),
        hle_export!(UnregisterTraceGuids),
        hle_export!(GetTraceLoggerHandle),
        hle_export!(GetTraceEnableFlags),
        hle_export!(GetTraceEnableLevel),
        hle_export!(TraceMessage),
        hle_export!(FindResourceExW),
        hle_export!(LoadResource),
        hle_export!(GetConsoleScreenBufferInfo),
        hle_export!(InitializeProcThreadAttributeList),
        hle_export!(UpdateProcThreadAttribute),
        hle_export!(DeleteProcThreadAttributeList),
        hle_export!(CreateRemoteThreadEx),
        hle_export!(GetLogicalProcessorInformationEx),
        // what the executable imports beyond that
        hle_export!(GetSystemTime),
        hle_export!(GetLocalTime),
        hle_export!(FileTimeToSystemTime),
        hle_export!(SystemTimeToFileTime),
        hle_export!(FileTimeToLocalFileTime),
        hle_export!(LocalFileTimeToFileTime),
        hle_export!(GetFileTime),
        hle_export!(GetDateFormatEx),
        hle_export!(GetTimeFormatEx),
        hle_export!(GetUserDefaultLocaleName),
        hle_export!(GetUserDefaultGeoName),
        hle_export!(LocaleNameToLCID),
        hle_export!(lstrcmpW),
        hle_export!(GlobalMemoryStatusEx),
        hle_export!(GetLogicalProcessorInformation),
        hle_export!(VirtualQueryEx),
        hle_export!(GetProcessId),
        hle_export!(OpenThread),
        hle_export!(GetCurrentThreadStackLimits),
        hle_export!(SetProcessPriorityBoost),
        hle_export!(GetThreadGroupAffinity),
        hle_export!(SetThreadGroupAffinity),
        hle_export!(WaitNamedPipeW),
        hle_export!(SetNamedPipeHandleState),
        hle_export!(RegisterAppConstrainedChangeNotification),
        hle_export!(UnregisterAppConstrainedChangeNotification),
        hle_export!(WerRegisterFile),
        hle_export!(CoInitializeEx),
        hle_export!(CoUninitialize),
        hle_export!(StringFromGUID2),
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
        // thread pool
        hle_export!(CreateThreadpoolWork),
        hle_export!(SubmitThreadpoolWork),
        hle_export!(WaitForThreadpoolWorkCallbacks),
        hle_export!(CloseThreadpoolWork),
        hle_export!(CreateThreadpoolTimer),
        hle_export!(SetThreadpoolTimer),
        hle_export!(WaitForThreadpoolTimerCallbacks),
        hle_export!(CloseThreadpoolTimer),
        hle_export!(CreateThreadpoolWait),
        hle_export!(SetThreadpoolWait),
        hle_export!(WaitForThreadpoolWaitCallbacks),
        hle_export!(CloseThreadpoolWait),
        hle_export!(QueueUserWorkItem),
        hle_export!(CallbackMayRunLong),
        hle_export!(DisassociateCurrentThreadFromCallback),
        hle_export!(FreeLibraryWhenCallbackReturns),
        // the rest
        hle_export!(EncodePointer),
        hle_export!(DecodePointer),
        hle_export!(UnhandledExceptionFilter),
        hle_export!(RtlPcToFileHeader),
        hle_export!(RtlCaptureStackBackTrace),
        hle_export!(DebugBreak),
        hle_export!(RaiseFailFastException),
        hle_export!(FlushProcessWriteBuffers),
        hle_export!(LocalAlloc),
        hle_export!(LocalFree),
        hle_export!(GlobalFree),
        hle_export!(FormatMessageW),
        hle_export!(FormatMessageA),
        hle_export!(lstrlenW),
        hle_export!(lstrcmpiW),
        hle_export!(CompareStringEx),
        hle_export!(GetStringTypeW),
        hle_export!(GetStringTypeExW),
        hle_export!(LCMapStringEx),
        hle_export!(GetLocaleInfoEx),
        hle_export!(GetUserDefaultUILanguage),
        hle_export!(SetEnvironmentVariableA),
        hle_export!(SetThreadIdealProcessorEx),
        hle_export!(GetSystemTimeAdjustment),
        hle_export!(GetConsoleWindow),
        hle_export!(EventRegister),
        hle_export!(EventUnregister),
        hle_export!(EventSetInformation),
        hle_export!(EventWriteTransfer),
        hle_export!(RegisterAppStateChangeNotification),
        hle_export!(UnregisterAppStateChangeNotification),
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
            "advapi32.dll",
            "crypt32.dll",
            "api-ms-win-core-*",
            "api-ms-win-eventing-*",
        ],
        exports,
    }
}

/// `xaudio2_9.dll`: `XAudio2Create`, by name and as ordinal 1.
pub fn xaudio2() -> HleModule {
    let create = HleExport {
        name: "XAudio2Create",
        address: misc::XAudio2Create as *const () as usize,
    };
    HleModule {
        name: "xaudio2",
        dlls: &["xaudio2_9.dll"],
        exports: vec![
            create,
            HleExport {
                name: "#1",
                ..create
            },
        ],
    }
}

pub use com::oleaut32;

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
