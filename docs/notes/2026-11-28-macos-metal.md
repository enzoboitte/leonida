# Metal: the start of the GPU back end on macOS

On Windows the console graphics objects of `leonida-gpu` (device, lists, queues, resources,
descriptors) hand their work to Direct3D 12 through `host.rs` and the modules around it (`draw`,
`indirect`, `texture`, `queue2`, about 30,000 lines). macOS has no Direct3D 12; its GPU API is
Metal, so the back end there is a Metal one in place of that layer. This is its first part:
`crates/leonida-gpu/src/metal.rs` (macOS only), through the `objc2-metal` bindings
(Zlib/Apache-2.0/MIT).

- `device()`: Metal's default device.
- `capabilities()`: its name, recommended working set, unified memory, Metal 3 (mesh and object
  shaders), Apple GPU family, ray tracing.
- `run_kernel()`: a Metal Shading Language kernel compiled at run time, run over a shared buffer,
  read back: the whole path from compiling to reading results.

## Measured

Apple M5, macOS 26.6, from an x86-64 process under Rosetta 2, 2026-11-28: name "Apple M5",
working set 26,800,603,136 bytes, unified memory, Metal 3, Apple family 9, ray tracing. A kernel
over 1,000 values gave the right results; a source that is not Metal fails to compile with an
error, as it should.

## What is left (the bulk of the port)

1. Resources: buffers and textures over title memory. Buffers are done: `wrap()` makes a
   Metal buffer over title pages with no copy (`newBufferWithBytesNoCopy`, shared storage), as the
   Windows side wraps title memory. Textures are left.
   choices: Apple's Metal Shader Converter (DXIL to Metal IR; a free download that needs an Apple
   developer sign-in, not installed here, its coverage of this DXIL unknown), or a second emitter
   in `leonida-shader` writing MSL from its IR. A decision for the project.
3. Root signatures and descriptor heaps as argument buffers; pipelines; render passes from
   render targets; indirect draws and mesh dispatches; queries; fences as Metal events.
4. Presentation: a window with a `CAMetalLayer`.
5. Ray tracing: acceleration structures and intersection queries (Metal has both).

## Tested

`cargo test -p leonida-gpu --target x86_64-apple-darwin --locked metal`: 2 passed (skipped where
there is no Metal device). With the rest of the macOS crates: 172 passed, 6 ignored. Clippy with
`-D warnings` is clean for `x86_64-apple-darwin`, `aarch64-apple-darwin` (where `leonida-gpu` is
empty) and, from the Mac, `x86_64-pc-windows-msvc` (the new dependencies are macOS only).

## Buffers over title memory

`wrap(device, at, len)` makes a Metal buffer over mapped memory with no copy; `dispatch()` runs a
kernel on one. Measured under Rosetta 2 (M5, macOS 26.6): a kernel inverting 16 KiB of an
anonymous mapping's words left them inverted where the CPU reads them, with no copy back. The
process sees 4 KiB pages under Rosetta (`getconf PAGESIZE`: 4096 under `arch -x86_64`, 16384
natively); buffers must start and end on them. Whether Metal takes a no-copy buffer that is 4 KiB
but not 16 KiB aligned was not tried (the mapping in the test was 16 KiB aligned).
