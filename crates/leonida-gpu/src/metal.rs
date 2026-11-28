//! Metal, the host GPU on macOS: the start of the back end that takes the
//! place of `host` (Direct3D 12) there. The console objects above it
//! (devices, lists, queues, resources) stay as they are; what they hand to
//! D3D12 on Windows goes to Metal here.
//!
//! Done: the device and what it can do, and compute work run and read back.
//! Not done: everything a frame needs (resources, descriptors, pipelines
//! from translated shaders, command lists, presentation). See
//! `docs/notes/2026-11-28-macos-metal.md`.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBuffer, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLComputeCommandEncoder,
    MTLComputePipelineState, MTLCreateSystemDefaultDevice, MTLDevice, MTLGPUFamily, MTLLibrary,
    MTLResourceOptions, MTLSize,
};

pub type Device = Retained<ProtocolObject<dyn MTLDevice>>;

/// The host GPU: Metal's default device.
pub fn device() -> Option<Device> {
    MTLCreateSystemDefaultDevice()
}

/// What the back end needs to know of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub name: String,
    /// The memory Metal recommends a process keep resident, in bytes.
    pub working_set: u64,
    /// CPU and GPU share memory, as on the console.
    pub unified: bool,
    /// Metal 3: mesh and object shaders (the title draws with mesh shaders).
    pub metal3: bool,
    /// The highest Apple GPU family, 0 for none.
    pub apple_family: u32,
    /// Ray-tracing acceleration structures (the title traces rays).
    pub ray_tracing: bool,
}

pub fn capabilities(device: &Device) -> Capabilities {
    let families = [
        (MTLGPUFamily::Apple9, 9),
        (MTLGPUFamily::Apple8, 8),
        (MTLGPUFamily::Apple7, 7),
        (MTLGPUFamily::Apple6, 6),
    ];
    Capabilities {
        name: device.name().to_string(),
        working_set: device.recommendedMaxWorkingSetSize(),
        unified: device.hasUnifiedMemory(),
        metal3: device.supportsFamily(MTLGPUFamily::Metal3),
        apple_family: families
            .iter()
            .find(|(f, _)| device.supportsFamily(*f))
            .map_or(0, |&(_, n)| n),
        ray_tracing: device.supportsRaytracing(),
    }
}

/// Compiles the Metal Shading Language `source` and runs its kernel `name`
/// over `count` threads, one `u32` of a shared buffer each, that starts as
/// `input`; returns the buffer afterwards. A way to check the whole path
/// (compile, pipeline, encode, submit, wait, read back) on a device.
pub fn run_kernel(
    device: &Device,
    source: &str,
    name: &str,
    input: &[u32],
) -> Result<Vec<u32>, String> {
    let library = device
        .newLibraryWithSource_options_error(&NSString::from_str(source), None)
        .map_err(|e| format!("compiling: {}", e.localizedDescription()))?;
    let function = library
        .newFunctionWithName(&NSString::from_str(name))
        .ok_or_else(|| format!("no kernel {name}"))?;
    let pipeline = device
        .newComputePipelineStateWithFunction_error(&function)
        .map_err(|e| format!("pipeline: {}", e.localizedDescription()))?;
    let bytes = std::mem::size_of_val(input);
    // SAFETY: `input` is `bytes` readable bytes; the buffer copies them.
    let buffer = unsafe {
        device.newBufferWithBytes_length_options(
            std::ptr::NonNull::new(input.as_ptr().cast_mut().cast()).ok_or("empty input")?,
            bytes,
            MTLResourceOptions::StorageModeShared,
        )
    }
    .ok_or("no buffer")?;
    let queue = device.newCommandQueue().ok_or("no command queue")?;
    let commands = queue.commandBuffer().ok_or("no command buffer")?;
    let encoder = commands.computeCommandEncoder().ok_or("no encoder")?;
    encoder.setComputePipelineState(&pipeline);
    // SAFETY: the buffer is index 0 of the kernel, as its source declares.
    unsafe { encoder.setBuffer_offset_atIndex(Some(&buffer), 0, 0) };
    let threads = MTLSize {
        width: input.len(),
        height: 1,
        depth: 1,
    };
    let group = MTLSize {
        width: pipeline.maxTotalThreadsPerThreadgroup().min(input.len()),
        height: 1,
        depth: 1,
    };
    encoder.dispatchThreads_threadsPerThreadgroup(threads, group);
    encoder.endEncoding();
    commands.commit();
    commands.waitUntilCompleted();
    if let Some(e) = commands.error() {
        return Err(format!("running: {}", e.localizedDescription()));
    }
    // SAFETY: a shared buffer of `input.len()` u32, the GPU done with it.
    let out = unsafe {
        std::slice::from_raw_parts(buffer.contents().as_ptr().cast::<u32>(), input.len())
    };
    Ok(out.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_can_do_what_the_title_needs() {
        let Some(device) = device() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let c = capabilities(&device);
        eprintln!("{c:?}");
        assert!(!c.name.is_empty());
        assert!(c.working_set > 0);
        assert!(c.metal3, "mesh shaders need Metal 3");
    }

    #[test]
    fn a_kernel_runs_and_reads_back() {
        let Some(device) = device() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let source = "kernel void twice(device uint *v [[buffer(0)]],\n\
                      uint i [[thread_position_in_grid]]) { v[i] = v[i] * 2 + 1; }";
        let input: Vec<u32> = (0..1000).collect();
        let out = run_kernel(&device, source, "twice", &input).unwrap();
        assert_eq!(out, input.iter().map(|v| v * 2 + 1).collect::<Vec<_>>());
        assert!(run_kernel(&device, "not metal", "twice", &input).is_err());
    }
}
