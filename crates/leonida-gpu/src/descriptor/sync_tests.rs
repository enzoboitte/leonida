//! Descriptor-creation regressions: no shader, command list or GPU submission.

use super::*;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
};

static TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug)]
enum Scenario {
    ApiRestore,
    NullRestore,
    ApiClear,
    QueuedClear,
}

struct Owned {
    heap: This,
    memory: *mut std::ffi::c_void,
}
impl Drop for Owned {
    fn drop(&mut self) {
        object::slot::<extern "win64" fn(This) -> u32>(self.heap, 0x10)(self.heap);
        // SAFETY: heap released; these tests never submit any native work.
        unsafe { VirtualFree(self.memory, 0, MEM_RELEASE) };
    }
}

fn run(scenario: Scenario, page: bool) {
    let _one = TEST.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: private writable watched allocation, retained until the heap is released.
    let memory = unsafe {
        VirtualAlloc(
            std::ptr::null(),
            crate::watch::PAGE,
            MEM_RESERVE | MEM_COMMIT | 0x20_0000,
            PAGE_READWRITE,
        )
    };
    assert!(!memory.is_null());
    #[repr(C)]
    struct Extended(HeapDesc, Placement);
    let desc = Extended(
        HeapDesc {
            kind: SAMPLERS,
            count: 64,
            flags: SHADER_VISIBLE,
            nodes: 1,
        },
        Placement {
            address: memory as u64,
            size: 64 * 16,
        },
    );
    let mut h = std::ptr::null();
    assert_eq!(
        create_heap(std::ptr::null(), &desc.0, &HEAP_IIDS[0], &mut h),
        object::S_OK
    );
    let _owned = Owned { heap: h, memory };
    let at = heap(h).handle(5).unwrap();
    let mut a = [0_u32; 4];
    let mut b = [0_u32; 4];
    let mut sampler = [
        21, // Linear minification, magnification and mip filtering.
        3, 3, 3, // Clamp each axis.
        0, // MipLODBias.
        1, // MaxAnisotropy.
        1, // ComparisonFunc: NEVER, also valid for non-comparison filtering.
        0, 0, 0, 0, // BorderColor.
        0, // MinLOD.
        4.0_f32.to_bits(), // MaxLOD.
    ];
    create_sampler(std::ptr::null(), &sampler, a.as_mut_ptr() as u64);
    if matches!(scenario, Scenario::QueuedClear) {
        create_sampler(std::ptr::null(), &sampler, at);
        assert!(heap(h).host.get().is_none());
        assert!(heap(h).pending.lock().unwrap().contains(&5));
    }
    sampler[0] = 0; // A second valid sampler: point instead of linear filtering.
    create_sampler(std::ptr::null(), &sampler, b.as_mut_ptr() as u64);
    let key_a = u64::from(a[2]) << 32 | u64::from(a[1]);
    let key_b = u64::from(b[2]) << 32 | u64::from(b[1]);
    assert_ne!(key_a, key_b);
    let Some(host) = heap(h).host() else {
        eprintln!("descriptor-record-sync skipped: no D3D12 device available");
        return;
    };
    let raw = |record: &[u32; 4]| {
        // SAFETY: both authored sources and this private heap slot hold sixteen bytes.
        unsafe { std::ptr::copy_nonoverlapping(record.as_ptr().cast::<u8>(), at as *mut u8, 16) };
    };
    let expected = match scenario {
        Scenario::ApiRestore | Scenario::NullRestore => {
            raw(&a);
            sync_heaps_with_page(page);
            assert_eq!(host.made_key(5), Some(key_a));
            let replacement = if matches!(scenario, Scenario::ApiRestore) {
                b
            } else {
                [0; 4]
            };
            copy_simple(
                std::ptr::null(),
                1,
                at,
                replacement.as_ptr() as u64,
                SAMPLERS,
            );
            assert_eq!(
                host.made_key(5),
                Some(if matches!(scenario, Scenario::ApiRestore) {
                    key_b
                } else {
                    0
                })
            );
            raw(&a);
            key_a
        }
        Scenario::ApiClear => {
            create_sampler(std::ptr::null(), &sampler, at);
            assert_eq!(host.made_key(5), Some(key_b));
            raw(&[0; 4]);
            0
        }
        Scenario::QueuedClear => {
            assert_eq!(host.made_key(5), Some(key_a));
            raw(&[0; 4]);
            0
        }
    };
    sync_heaps_with_page(page);
    let actual = host.made_key(5);
    println!(
        "descriptor-record-sync {scenario:?} page={page}: actual={actual:?} expected={expected}"
    );
    assert_eq!(actual, Some(expected), "{scenario:?} page={page}");
}

#[test]
fn api_restore_scalar() {
    run(Scenario::ApiRestore, false);
}
#[test]
fn api_restore_page() {
    run(Scenario::ApiRestore, true);
}
#[test]
fn null_restore_scalar() {
    run(Scenario::NullRestore, false);
}
#[test]
fn null_restore_page() {
    run(Scenario::NullRestore, true);
}
#[test]
fn api_clear_scalar() {
    run(Scenario::ApiClear, false);
}
#[test]
fn api_clear_page() {
    run(Scenario::ApiClear, true);
}
#[test]
fn queued_clear_scalar() {
    run(Scenario::QueuedClear, false);
}
#[test]
fn queued_clear_page() {
    run(Scenario::QueuedClear, true);
}
