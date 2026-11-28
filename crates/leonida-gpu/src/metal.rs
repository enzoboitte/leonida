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
use objc2_core_foundation::CGSize;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder,
    MTLCommandQueue, MTLComputeCommandEncoder, MTLComputePipelineState,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLDrawable, MTLGPUFamily, MTLLibrary, MTLLoadAction,
    MTLOrigin, MTLPixelFormat, MTLRenderPassDescriptor, MTLResourceOptions, MTLSize,
    MTLStoreAction, MTLTexture,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

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

/// A buffer over title memory, with no copy: on unified memory the GPU
/// reads and writes the title's own pages, as the console's does. `at` and
/// `len` must lie on the pages the process sees (4 KiB under Rosetta 2, 16
/// KiB natively on Apple Silicon); the memory must outlive the buffer.
///
/// # Safety
/// `at..at+len` is mapped read+write memory that stays mapped while the
/// buffer lives.
pub unsafe fn wrap(device: &Device, at: usize, len: usize) -> Option<Buffer> {
    let page = page_size();
    if !at.is_multiple_of(page) || !len.is_multiple_of(page) || len == 0 {
        return None;
    }
    // SAFETY: the caller's memory; no deallocator, the title owns it.
    unsafe {
        device.newBufferWithBytesNoCopy_length_options_deallocator(
            std::ptr::NonNull::new(at as *mut _)?,
            len,
            MTLResourceOptions::StorageModeShared,
            None,
        )
    }
}

pub type Buffer = Retained<ProtocolObject<dyn MTLBuffer>>;

/// The host's page size, which no-copy buffers must keep to.
pub fn page_size() -> usize {
    // SAFETY: no preconditions.
    (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }) as usize
}

/// Runs kernel `name` of `source` over `count` threads with `buffer` at
/// index 0.
pub fn dispatch(
    device: &Device,
    source: &str,
    name: &str,
    buffer: &Buffer,
    count: usize,
) -> Result<(), String> {
    let library = device
        .newLibraryWithSource_options_error(&NSString::from_str(source), None)
        .map_err(|e| format!("compiling: {}", e.localizedDescription()))?;
    let function = library
        .newFunctionWithName(&NSString::from_str(name))
        .ok_or_else(|| format!("no kernel {name}"))?;
    let pipeline = device
        .newComputePipelineStateWithFunction_error(&function)
        .map_err(|e| format!("pipeline: {}", e.localizedDescription()))?;
    let queue = device.newCommandQueue().ok_or("no command queue")?;
    let commands = queue.commandBuffer().ok_or("no command buffer")?;
    let encoder = commands.computeCommandEncoder().ok_or("no encoder")?;
    encoder.setComputePipelineState(&pipeline);
    // SAFETY: the buffer is index 0 of the kernel.
    unsafe { encoder.setBuffer_offset_atIndex(Some(buffer), 0, 0) };
    let width = pipeline.maxTotalThreadsPerThreadgroup().min(count);
    encoder.dispatchThreads_threadsPerThreadgroup(
        MTLSize {
            width: count,
            height: 1,
            depth: 1,
        },
        MTLSize {
            width,
            height: 1,
            depth: 1,
        },
    );
    encoder.endEncoding();
    commands.commit();
    commands.waitUntilCompleted();
    match commands.error() {
        Some(e) => Err(format!("running: {}", e.localizedDescription())),
        None => Ok(()),
    }
}

/// Where frames are shown: a Metal layer of the size the title renders at.
/// Attached to a window (by the runner), it shows on screen; alone, its
/// images are made and presented all the same, which the tests use.
pub struct Surface {
    layer: Retained<CAMetalLayer>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    size: (usize, usize),
}

impl Surface {
    pub fn new(device: &Device, width: usize, height: usize) -> Option<Self> {
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        // Read back by the tests (and by frame shots later).
        layer.setFramebufferOnly(false);
        layer.setDrawableSize(CGSize {
            width: width as f64,
            height: height as f64,
        });
        let queue = device.newCommandQueue()?;
        Some(Surface {
            layer,
            queue,
            size: (width, height),
        })
    }

    pub fn layer(&self) -> &CAMetalLayer {
        &self.layer
    }

    /// Clears the next image to `rgba` and presents it. With `read`, also
    /// gives its pixels (BGRA, 8 bits each, row by row).
    pub fn present_clear(&self, rgba: [f64; 4], read: bool) -> Result<Option<Vec<u8>>, String> {
        let drawable = self.layer.nextDrawable().ok_or("no drawable")?;
        let texture = drawable.texture();
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: attachment 0 always exists.
        let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(&texture));
        color.setLoadAction(MTLLoadAction::Clear);
        color.setStoreAction(MTLStoreAction::Store);
        let [red, green, blue, alpha] = rgba;
        color.setClearColor(MTLClearColor {
            red,
            green,
            blue,
            alpha,
        });
        let commands = self.queue.commandBuffer().ok_or("no command buffer")?;
        commands
            .renderCommandEncoderWithDescriptor(&pass)
            .ok_or("no render encoder")?
            .endEncoding();
        let (w, h) = self.size;
        let readback = if read {
            let device = self.layer.device().ok_or("no device")?;
            let buffer = device
                .newBufferWithLength_options(w * h * 4, MTLResourceOptions::StorageModeShared)
                .ok_or("no buffer")?;
            let blit = commands.blitCommandEncoder().ok_or("no blit encoder")?;
            // SAFETY: the whole texture, into a buffer of its size.
            unsafe {
                blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &texture,
                    0,
                    0,
                    MTLOrigin { x: 0, y: 0, z: 0 },
                    MTLSize { width: w, height: h, depth: 1 },
                    &buffer,
                    0,
                    w * 4,
                    w * h * 4,
                );
            }
            blit.endEncoding();
            Some(buffer)
        } else {
            None
        };
        commands.presentDrawable(ProtocolObject::from_ref(&*drawable));
        commands.commit();
        commands.waitUntilCompleted();
        if let Some(e) = commands.error() {
            return Err(format!("presenting: {}", e.localizedDescription()));
        }
        Ok(readback.map(|b| {
            // SAFETY: a shared buffer of w * h * 4 bytes, the GPU done with it.
            unsafe { std::slice::from_raw_parts(b.contents().as_ptr().cast::<u8>(), w * h * 4) }
                .to_vec()
        }))
    }
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

    #[test]
    fn a_surface_presents_images_of_its_size_and_colour() {
        let Some(device) = device() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let surface = Surface::new(&device, 64, 32).expect("a surface");
        for _ in 0..3 {
            surface.present_clear([0.0, 0.0, 0.0, 1.0], false).unwrap();
        }
        let pixels = surface
            .present_clear([1.0, 0.5, 0.0, 1.0], true)
            .unwrap()
            .unwrap();
        assert_eq!(pixels.len(), 64 * 32 * 4);
        // BGRA: blue 0, green half, red full, alpha full.
        for p in pixels.chunks_exact(4) {
            assert_eq!((p[0], p[2], p[3]), (0, 255, 255));
            assert!((127..=128).contains(&p[1]), "{p:?}");
        }
    }

    #[test]
    fn the_gpu_writes_title_memory_in_place() {
        let Some(device) = device() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let page = page_size();
        let len = 4 * page;
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
        let rw = libc::PROT_READ | libc::PROT_WRITE;
        // SAFETY: a fresh anonymous mapping, ours for the test.
        let at = unsafe { libc::mmap(std::ptr::null_mut(), len, rw, flags, -1, 0) } as usize;
        // SAFETY: as above.
        let words = unsafe { std::slice::from_raw_parts_mut(at as *mut u32, len / 4) };
        words
            .iter_mut()
            .enumerate()
            .for_each(|(i, w)| *w = i as u32);
        assert!(
            unsafe { wrap(&device, at + 4, len) }.is_none(),
            "off a page"
        );
        // SAFETY: mapped until the end of the test, after the buffer.
        let buffer = unsafe { wrap(&device, at, len) }.expect("a no-copy buffer");
        let source = "kernel void negate(device uint *v [[buffer(0)]],\n\
                      uint i [[thread_position_in_grid]]) { v[i] = ~v[i]; }";
        dispatch(&device, source, "negate", &buffer, len / 4).unwrap();
        assert!(
            words.iter().enumerate().all(|(i, &w)| w == !(i as u32)),
            "seen by the CPU in place"
        );
        drop(buffer);
        // SAFETY: the mapping made above, no longer used.
        unsafe { libc::munmap(at as *mut _, len) };
    }
}
