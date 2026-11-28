//! Implementations, one module per area of the console API.
//!
//! Adding an API: write an `extern "win64"` function named exactly like
//! the import, then list it with `hle_export!` in the module's `module()`.
//! Title code calls it with the Windows x64 convention: on Windows that is
//! also "system", on macOS "system" is the System V one. Callbacks given to
//! the host's own APIs stay "system".
//! Run `leonida census` over your titles to see what is still missing.
//!
//! Planned areas (driven by census results, not by completeness):
//! `file` (package paths), `thread` (TLS callbacks for title threads),
//! `xaudio2`, `xstore` stubs.

#[cfg(windows)]
pub mod apu;
pub mod audio;
pub mod d3d12x;
pub mod display;
pub mod dsp;
#[cfg(any(windows, target_os = "macos"))]
pub mod dstorage;
#[cfg(windows)]
pub mod files;
pub mod gameinput;
pub mod kernel;
#[cfg(windows)]
pub mod lifecycle;
#[cfg(windows)]
pub mod media;
#[cfg(windows)]
pub mod net;
pub mod speech;
pub mod storage;
pub mod xasync;
pub mod xg;
pub mod xgame;
pub mod xgameruntime;
pub mod xgamesave;
pub mod xgameui;
#[cfg(windows)]
pub mod xmem;
pub mod xnetworking;
pub mod xpackage;
pub mod xstore;
pub mod xsystem;
pub mod xuser;
// What Windows' own DLLs give a title, for hosts without them.
#[cfg(target_os = "macos")]
pub mod win32;
#[cfg(target_os = "macos")]
pub use win32::files;
#[cfg(target_os = "macos")]
pub use win32::xmem;

use crate::HleModule;

pub(crate) fn all() -> Vec<HleModule> {
    let mut all = vec![
        kernel::module(),
        xgameruntime::module(),
        gameinput::module(),
        d3d12x::module(),
        audio::module(),
        dsp::module(),
        xg::module(),
    ];
    #[cfg(windows)]
    all.push(apu::module());
    #[cfg(windows)]
    all.push(media::module());
    #[cfg(any(windows, target_os = "macos"))]
    all.push(dstorage::module());
    #[cfg(windows)]
    all.push(files::module());
    #[cfg(windows)]
    all.push(lifecycle::module());
    #[cfg(windows)]
    all.push(net::module());
    #[cfg(windows)]
    all.push(xmem::module());
    #[cfg(any(windows, target_os = "macos"))]
    all.push(xsystem::module());
    #[cfg(target_os = "macos")]
    all.push(win32::module());
    #[cfg(target_os = "macos")]
    all.push(win32::xmem::module());
    #[cfg(target_os = "macos")]
    all.push(win32::net::module());
    #[cfg(target_os = "macos")]
    all.push(win32::oleaut32());
    #[cfg(target_os = "macos")]
    all.push(win32::crt::module());
    #[cfg(target_os = "macos")]
    all.push(win32::xaudio2());
    #[cfg(target_os = "macos")]
    all.extend(win32::more::modules());
    all
}
