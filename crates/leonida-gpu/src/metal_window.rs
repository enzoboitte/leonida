//! The title's window on macOS: an AppKit window whose view shows a
//! [`Surface`]'s Metal layer. AppKit runs on the main thread only, so the
//! runner keeps that thread here while the title runs on its own.

use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventMask, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};

use crate::metal::Surface;

/// Shows `surface` in a window titled `title` and handles the window's
/// events while `running()` holds and the window is open. Returns whether
/// the window was closed (the user closed the title, as on a console).
pub fn show(
    title: &str,
    surface: &Surface,
    mut running: impl FnMut() -> bool,
) -> Result<bool, String> {
    let mtm = MainThreadMarker::new().ok_or("the window needs the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let (w, h) = surface.size();
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    // SAFETY: a fresh window on the main thread, kept by `window` for the loop.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: `window` keeps the window; AppKit must not free it on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    let view = window.contentView().ok_or("the window has no view")?;
    view.setWantsLayer(true);
    view.setLayer(Some(surface.layer()));
    window.center();
    window.makeKeyAndOrderFront(None);
    app.finishLaunching();
    app.activate();
    while running() {
        if !window.isVisible() {
            return Ok(true);
        }
        let until = NSDate::dateWithTimeIntervalSinceNow(0.01);
        // SAFETY: the default run loop mode, a static of AppKit's.
        let mode = unsafe { NSDefaultRunLoopMode };
        while let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&until),
            mode,
            true,
        ) {
            app.sendEvent(&event);
        }
    }
    window.close();
    Ok(false)
}
