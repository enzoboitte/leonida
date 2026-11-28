//! Opt-in device regressions through real heaps, recording and submission.

use super::*;
use crate::host::Host;
use crate::object::{S_OK, guid, slot};
use std::sync::Mutex;

static TEST_LOCK: Mutex<()> = Mutex::new(());

const READ: &str = r#"
#define RS "DescriptorTable(SRV(t0)), SRV(t1), UAV(u0)"
StructuredBuffer<uint> input : register(t0);
ByteAddressBuffer strides : register(t1);
RWByteAddressBuffer output : register(u0);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() { output.Store2(0, uint2(input[0], strides.Load(0))); }
"#;

const READ_STRIDE: &str = r#"
#define RS "SRV(t0), UAV(u0), RootConstants(num32BitConstants=1,b0)"
ByteAddressBuffer strides : register(t0);
RWByteAddressBuffer output : register(u0);
cbuffer Position : register(b0) { uint offset; };
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() { output.Store(offset, strides.Load(0)); }
"#;

fn enabled() -> bool {
    let adapter = std::env::var("LEONIDA_TEST_HEAP_HARDWARE").as_deref() == Ok("1")
        || std::env::var("LEONIDA_D3D12_WARP").as_deref() == Ok("1");
    let on = std::env::var("LEONIDA_TEST_HEAP_READINESS").as_deref() == Ok("1");
    if !on || !adapter {
        eprintln!("heap readiness device test skipped: explicit test and adapter opt-ins required");
        return false;
    }
    assert_eq!(
        crate::inplace::level(),
        0,
        "test requires an actual shadow copy"
    );
    assert!(
        !crate::shadow::cards(),
        "set LEONIDA_CARD_BUFFERS=0 for this fixture"
    );
    true
}

struct TitleBuffer(Box<[u32]>);

impl TitleBuffer {
    fn new(value: u32) -> Self {
        let words = vec![value; 16 * 1024].into_boxed_slice();
        let start = words.as_ptr() as u64;
        crate::shadow::register(start as usize, start, 64 * 1024);
        Self(words)
    }

    fn address(&self) -> u64 {
        self.0.as_ptr() as u64
    }
}

impl Drop for TitleBuffer {
    fn drop(&mut self) {
        crate::shadow::unregister(self.address() as usize, self.address());
    }
}

struct TitleHeap(This);

impl TitleHeap {
    fn new(buffer: &TitleBuffer, stride: u32) -> Self {
        // The device API reads the placement after its ordinary description.
        #[repr(C)]
        struct Extended {
            description: [u32; 4],
            placement: [u64; 2],
        }
        let description = Extended {
            description: [0, 1, 1, 0],
            placement: [0, 0],
        };
        let iid = guid(
            0x8efb471d,
            0x616c,
            0x4f49,
            [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51],
        );
        let mut object = std::ptr::null();
        assert_eq!(
            crate::descriptor::create_heap(
                std::ptr::null(),
                std::ptr::from_ref(&description).cast(),
                &iid,
                &mut object,
            ),
            S_OK,
        );
        let owner = Self(object);
        let mut resource = [0u32; 14];
        resource[0] = 1; // Buffer.
        resource[4] = 64 * 1024;
        let description = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT(0),
            ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_SRV {
                    FirstElement: 0,
                    NumElements: 1,
                    StructureByteStride: stride,
                    Flags: D3D12_BUFFER_SRV_FLAG_NONE,
                },
            },
        };
        let mut words = [0u32; 16];
        // SAFETY: a 40-byte SRV description; the first ten dwords hold it.
        let view_words = unsafe {
            std::ptr::from_ref(&description)
                .cast::<[u32; 10]>()
                .read_unaligned()
        };
        words[..10].copy_from_slice(&view_words);
        words[10] = buffer.address() as u32;
        words[11] = (buffer.address() >> 32) as u32;
        crate::descriptor::create_placed_srv(
            std::ptr::null(),
            &resource,
            &words,
            owner.heap().start,
        );
        let host = owner.heap().host().expect("actual mirrored title heap");
        assert_eq!(host.buffer_slots(), vec![0]);
        owner
    }

    fn heap(&self) -> &crate::descriptor::Heap {
        crate::descriptor::heap(self.0)
    }

    fn state(&self) -> State {
        let mut state = State::default();
        state.set_heaps(&[self.0]);
        state
    }
}

impl Drop for TitleHeap {
    fn drop(&mut self) {
        slot::<extern "win64" fn(This) -> u32>(self.0, 16)(self.0);
    }
}

struct Program {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

impl Program {
    fn new(host: &Host, source: &str) -> Self {
        let code = crate::host::compile(
            source,
            windows::core::s!("main"),
            windows::core::s!("cs_5_1"),
        )
        .expect("authored one-thread shader");
        // SAFETY: the authored bytecode includes its own matching root signature.
        unsafe {
            let root: ID3D12RootSignature =
                host.device.CreateRootSignature(0, &code).expect("root");
            let pso = host
                .device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: code.as_ptr().cast(),
                        BytecodeLength: code.len(),
                    },
                    ..Default::default()
                })
                .expect("pipeline");
            Self { root, pso }
        }
    }
}

struct Output {
    buffer: ID3D12Resource,
    readback: ID3D12Resource,
    fence: ID3D12Fence,
}

impl Output {
    fn new(host: &Host) -> Self {
        Self {
            buffer: host.uav_buffer(8).expect("output"),
            readback: host.read_back_buffer(8).expect("readback"),
            // SAFETY: a private completion fence, made before any submission.
            fence: unsafe {
                host.device
                    .CreateFence(0, D3D12_FENCE_FLAG_NONE)
                    .expect("fence")
            },
        }
    }

    fn begin(&self, list: &HostList) {
        // SAFETY: this private output has not been used before.
        unsafe {
            list.list.ResourceBarrier(&[crate::host::transition(
                &self.buffer,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            )]);
        }
    }

    fn end(&self, list: &HostList) {
        // SAFETY: the output was only written as UAV, and readback starts COPY_DEST.
        unsafe {
            list.list.ResourceBarrier(&[crate::host::transition(
                &self.buffer,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            list.list
                .CopyBufferRegion(&self.readback, 0, &self.buffer, 0, 8);
        }
        list.close();
    }

    fn submit(&self, host: &Host, list: HostList, late: bool) -> [u32; 2] {
        // Every owner stays in this scope (or the production late-flight owner)
        // until an actual completion fence. Post-submit failures cannot unwind.
        let mut held = Some(list);
        if late {
            host.execute_late(
                vec![crate::list::Detached::for_test(
                    held.take().expect("one list"),
                )],
                vec![0],
            );
        } else {
            host.execute(&[held.as_ref().expect("one list")]);
        }
        // SAFETY: the same native queue, after the actual production submission.
        if let Err(error) = unsafe { host.queue().Signal(&self.fence, 1) } {
            eprintln!("heap readiness completion Signal failed: {error}");
            std::process::abort();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            // SAFETY: read-only query of the retained private fence.
            let completed = unsafe { self.fence.GetCompletedValue() };
            if completed == u64::MAX || std::time::Instant::now() >= deadline {
                eprintln!("heap readiness completion missing: {completed}");
                std::process::abort();
            }
            if completed >= 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        crate::host::settle_for_fence();
        let mut data = std::ptr::null_mut();
        // SAFETY: the copy completed, and exactly eight readback bytes are read.
        unsafe {
            self.readback
                .Map(0, Some(&D3D12_RANGE { Begin: 0, End: 8 }), Some(&mut data))
                .expect("readback mapping");
            assert!(!data.is_null(), "successful readback Map returned null");
            let actual = data.cast::<[u32; 2]>().read_unaligned();
            self.readback.Unmap(0, Some(&D3D12_RANGE::default()));
            actual
        }
    }
}

fn record_read(host: &Host, program: &Program, heap: &TitleHeap) -> (HostList, Output) {
    let list = host.new_list().expect("recording");
    let state = heap.state();
    // The production path under test: discovery/cache, per-recording readiness,
    // actual Step::Refresh and stride ownership. No direct refresh is inserted.
    let strides = state.heap_buffers(&list).expect("heap buffers bound");
    let native = heap.heap().host().expect("native heap");
    let output = Output::new(host);
    output.begin(&list);
    // SAFETY: the actual mirrored structured-uint SRV matches READ; all owners
    // remain live until submission completes (or this unsubmitted list is dropped).
    unsafe {
        list.list.SetDescriptorHeaps(&[Some(native.heap.clone())]);
        list.list.SetComputeRootSignature(&program.root);
        list.list.SetPipelineState(&program.pso);
        list.list
            .SetComputeRootDescriptorTable(0, native.gpu_handle(0));
        list.list.SetComputeRootShaderResourceView(1, strides);
        list.list
            .SetComputeRootUnorderedAccessView(2, output.buffer.GetGPUVirtualAddress());
        list.list.Dispatch(1, 1, 1);
    }
    output.end(&list);
    (list, output)
}

#[derive(Clone, Copy, Debug)]
enum Delivery {
    OnlySecond,
    AbandonFirst,
    FirstControl,
}

fn next_frame() -> u64 {
    let before = crate::frame::current();
    let mut token = 0;
    crate::frame::wait(std::ptr::null(), 0, 0, std::ptr::null(), 2, &mut token);
    let frame = crate::frame::current();
    assert!(frame > before && token == frame);
    frame
}

fn reads_title_write(delivery: Delivery, late: bool) {
    if !enabled() {
        return;
    }
    if late {
        assert!(
            crate::host::late(),
            "late case requires LEONIDA_LATE_WAIT=3"
        );
    }
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let host = crate::host::host().expect("explicitly requested device");
    crate::host::settle_for_fence();
    let program = Program::new(host, READ);
    let mut title = TitleBuffer::new(13);
    let heap = TitleHeap::new(&title, 4);
    // Separate cases can run in one process; a previous case's global frame
    // decision must not consume this initializer's legacy refresh.
    next_frame();
    let (seed, seed_output) = record_read(host, &program, &heap);
    assert_eq!(
        seed_output.submit(host, seed, false),
        [13, 4],
        "initial GPU shadow"
    );
    // Seeding consumed the legacy global frame gate. Advance with the actual
    // title API, then keep A and B on exactly that one frame.
    let frame = next_frame();
    title.0[0] = 37;
    let (a, a_output) = record_read(host, &program, &heap);
    let (b, b_output) = record_read(host, &program, &heap);
    assert_eq!(crate::frame::current(), frame, "A and B share a frame");
    let actual = match delivery {
        Delivery::OnlySecond => b_output.submit(host, b, late),
        Delivery::AbandonFirst => {
            drop(a);
            drop(a_output);
            b_output.submit(host, b, late)
        }
        Delivery::FirstControl => a_output.submit(host, a, late),
    };
    eprintln!(
        "heap readiness {:?} late={late} enabled={} actual={actual:?}",
        delivery,
        crate::heap_readiness::enabled(),
    );
    assert_eq!(
        actual,
        [37, 4],
        "submitted recording must refresh the title write"
    );
}

#[test]
fn second_recording_refreshes_when_only_it_is_submitted() {
    reads_title_write(Delivery::OnlySecond, false);
}

#[test]
fn late_second_recording_refreshes_when_only_it_is_submitted() {
    reads_title_write(Delivery::OnlySecond, true);
}

#[test]
fn abandoning_first_recording_does_not_consume_refresh() {
    reads_title_write(Delivery::AbandonFirst, true);
}

#[test]
fn first_submitted_recording_is_a_positive_control() {
    reads_title_write(Delivery::FirstControl, false);
}

#[test]
fn switching_heaps_retains_both_stride_tables_until_completion() {
    if !enabled() {
        return;
    }
    assert!(
        crate::host::late(),
        "late case requires LEONIDA_LATE_WAIT=3"
    );
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let host = crate::host::host().expect("explicitly requested device");
    crate::host::settle_for_fence();
    let program = Program::new(host, READ_STRIDE);
    let title = TitleBuffer::new(37);
    let heaps = [TitleHeap::new(&title, 4), TitleHeap::new(&title, 12)];
    let list = host.new_list().expect("two-heap recording");
    let output = Output::new(host);
    output.begin(&list);
    let mut state = State::default();
    for (i, heap) in heaps.iter().enumerate() {
        state.set_heaps(&[heap.0]);
        let gpu = state.heap_buffers(&list).expect("actual heap readiness");
        let native = heap.heap().host().expect("native heap");
        let strides = native.strides_buffer().expect("stride resource");
        // Fail before releasing owners or submitting in the broken baseline:
        // never deliberately dispatch a dangling GPU address to prove retention.
        assert!(
            list.keeps_for_test(strides),
            "heap {i} stride resource is not retained"
        );
        // SAFETY: valid root buffer SRV, exactly its first dword. This shader
        // does not read either differently structured descriptor as a raw view.
        unsafe {
            if i != 0 {
                list.list.ResourceBarrier(&[D3D12_RESOURCE_BARRIER {
                    Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
                    Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                    Anonymous: D3D12_RESOURCE_BARRIER_0 {
                        UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                            pResource: std::mem::ManuallyDrop::new(None),
                        }),
                    },
                }]);
            }
            list.list.SetComputeRootSignature(&program.root);
            list.list.SetPipelineState(&program.pso);
            list.list.SetComputeRootShaderResourceView(0, gpu);
            list.list
                .SetComputeRootUnorderedAccessView(1, output.buffer.GetGPUVirtualAddress());
            list.list.SetComputeRoot32BitConstant(2, i as u32 * 4, 0);
            list.list.Dispatch(1, 1, 1);
        }
    }
    output.end(&list);
    drop(state);
    drop(heaps);
    // Only the recording now owns the two stride resources. Its retained
    // shadow spans also outlive the title heaps until this submission lands.
    let actual = output.submit(host, list, true);
    eprintln!("heap readiness retained strides actual={actual:?}");
    assert_eq!(actual, [4, 12]);
}
