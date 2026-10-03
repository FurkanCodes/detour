//! Native macOS window chrome: the standard traffic-light buttons over
//! Detour's own title bar, and a minimize button that tucks the app away in
//! the menu bar (no Dock icon) instead of the Dock.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSButton, NSToolbar, NSView,
    NSWindowButton, NSWindowToolbarStyle,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this type has no Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct MinimizeTarget;

    unsafe impl NSObjectProtocol for MinimizeTarget {}

    impl MinimizeTarget {
        // SAFETY: the signature matches a control action.
        #[unsafe(method(hideToMenuBar:))]
        fn hide_to_menu_bar(&self, sender: &NSButton) {
            if let Some(window) = sender.window() {
                window.orderOut(None);
            }
            show_in_dock(false);
        }
    }
);

/// Hooks up the native window chrome; call once the window exists. With
/// `minimize_to_menu_bar`, the yellow button hides Detour to the menu bar
/// instead of the Dock.
pub fn setup(window: &impl HasWindowHandle, minimize_to_menu_bar: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: winit hands out a valid NSView for the lifetime of the window.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    let Some(window) = view.window() else {
        return;
    };

    // An empty compact toolbar makes the title bar tall enough that the
    // traffic lights sit centred in Detour's own title row.
    let toolbar = NSToolbar::new(mtm);
    window.setToolbar(Some(&toolbar));
    window.setToolbarStyle(NSWindowToolbarStyle::UnifiedCompact);

    if !minimize_to_menu_bar {
        return;
    }
    if let Some(button) = window.standardWindowButton(NSWindowButton::MiniaturizeButton) {
        let target: Retained<MinimizeTarget> =
            unsafe { msg_send![MinimizeTarget::alloc(mtm), init] };
        // SAFETY: the target is kept alive for the rest of the process below,
        // and `hideToMenuBar:` takes the sender as its only argument.
        unsafe {
            button.setTarget(Some(&target as &AnyObject));
            button.setAction(Some(sel!(hideToMenuBar:)));
        }
        // Controls do not retain their target.
        std::mem::forget(target);
    }
}

/// Shows or removes Detour's Dock icon (and app menu). Without it Detour
/// lives only in the menu bar.
pub fn show_in_dock(show: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let policy = if show {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    };
    if app.activationPolicy() != policy {
        app.setActivationPolicy(policy);
    }
    if show {
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}
