//! Dragging files out of the window into other apps (Finder, Mail, a browser, the Dock …).
//!
//! egui's drag and drop only lives inside the window. When a drag of files leaves the
//! window, it is handed over to the system as a native dragging session, so any app that
//! accepts files can take them. Dragging back into the window then arrives as an ordinary
//! file drop (the Drop dialog). Only on macOS; elsewhere drags stay inside the window.

use std::path::PathBuf;

/// Hand `paths` to the system as a native drag. True when the drag was started.
#[cfg(target_os = "macos")]
pub fn start(frame: &eframe::Frame, paths: &[PathBuf]) -> bool { mac::start(frame, paths) }
#[cfg(not(target_os = "macos"))]
pub fn start(_frame: &eframe::Frame, _paths: &[PathBuf]) -> bool { false }

/// A native drag is under way (the app should keep repainting to notice its end).
#[cfg(target_os = "macos")]
pub fn active() -> bool { mac::active() }
#[cfg(not(target_os = "macos"))]
pub fn active() -> bool { false }

/// True once after a native drag has ended. The system keeps the mouse-up for itself,
/// so the caller has to tell egui that the button went up.
#[cfg(target_os = "macos")]
pub fn take_ended() -> bool { mac::take_ended() }
#[cfg(not(target_os = "macos"))]
pub fn take_ended() -> bool { false }

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource,
        NSEvent, NSEventModifierFlags, NSEventType, NSView, NSWorkspace,
    };
    use objc2_foundation::{NSArray, NSPoint, NSProcessInfo, NSRect, NSSize, NSString, NSURL};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    const ICON: f64 = 32.0;
    /// Items beyond this many share the last one's place in the drag image.
    const FANNED: usize = 8;

    thread_local! {
        /// The source of the running drag; AppKit does not promise to keep it alive.
        static SOURCE: RefCell<Option<Retained<DragSource>>> = const { RefCell::new(None) };
        static ENDED: Cell<bool> = const { Cell::new(false) };
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "OxideCommanderDragSource"]
        struct DragSource;

        unsafe impl NSObjectProtocol for DragSource {}

        unsafe impl NSDraggingSource for DragSource {
            /// Let the target choose, like a drag from Finder: same volume moves, another
            /// volume or app copies, and the modifier keys work as in Finder.
            #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
            fn source_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
                NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Generic | NSDragOperation::Link
            }

            #[unsafe(method(draggingSession:endedAtPoint:operation:))]
            fn ended(&self, _session: &NSDraggingSession, _point: NSPoint, _operation: NSDragOperation) {
                ENDED.set(true);
            }
        }
    );

    pub fn start(frame: &eframe::Frame, paths: &[PathBuf]) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        if active() { return false; }
        let Ok(handle) = frame.window_handle() else { return false };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return false };
        // SAFETY: eframe's window owns this view and outlives the frame we were handed.
        let view: &NSView = unsafe { h.ns_view.cast().as_ref() };
        let Some(window) = view.window() else { return false };
        let Some(event) = mouse_event(mtm, &window) else { return false };

        let at = view.convertPoint_fromView(event.locationInWindow(), None);
        let workspace = NSWorkspace::sharedWorkspace();
        let items: Vec<Retained<NSDraggingItem>> = paths.iter()
            .filter_map(|p| p.to_str())
            .enumerate()
            .map(|(i, p)| {
                let path = NSString::from_str(p);
                let url = NSURL::fileURLWithPath(&path);
                let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
                let icon = workspace.iconForFile(&path);
                let off = 4.0 * i.min(FANNED) as f64;
                let rect = NSRect::new(NSPoint::new(at.x - ICON / 2.0 + off, at.y - ICON / 2.0 - off), NSSize::new(ICON, ICON));
                let contents: &AnyObject = &icon;
                // SAFETY: `contents` is an NSImage, which the drag image accepts.
                unsafe { item.setDraggingFrame_contents(rect, Some(contents)) };
                item
            })
            .collect();
        if items.is_empty() { return false; }

        let source: Retained<DragSource> = unsafe { msg_send![DragSource::alloc(mtm), init] };
        ENDED.set(false);
        let session = view.beginDraggingSessionWithItems_event_source(
            &NSArray::from_retained_slice(&items), &event, ProtocolObject::from_ref(&*source));
        session.setAnimatesToStartingPositionsOnCancelOrFail(true);
        SOURCE.with_borrow_mut(|s| *s = Some(source));
        true
    }

    /// The mouse event that drives the drag: the current one when it is a left-button
    /// drag, otherwise one made up at the pointer (AppKit needs a mouse event to start).
    fn mouse_event(mtm: MainThreadMarker, window: &objc2_app_kit::NSWindow) -> Option<Retained<NSEvent>> {
        if let Some(e) = NSApplication::sharedApplication(mtm).currentEvent()
            && matches!(e.r#type(), NSEventType::LeftMouseDragged | NSEventType::LeftMouseDown)
            && e.windowNumber() == window.windowNumber() {
            return Some(e);
        }
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDragged,
            window.mouseLocationOutsideOfEventStream(),
            NSEventModifierFlags::empty(),
            NSProcessInfo::processInfo().systemUptime(),
            window.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
    }

    pub fn active() -> bool { SOURCE.with_borrow(|s| s.is_some()) }

    pub fn take_ended() -> bool {
        if !ENDED.replace(false) { return false; }
        SOURCE.with_borrow_mut(|s| *s = None);
        true
    }
}
