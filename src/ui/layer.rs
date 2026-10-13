//! Minimal bindings to gtk4-layer-shell, which turns a GTK window into a
//! wlr-layer-shell surface: a desktop widget on Hyprland, Sway, KDE Plasma
//! and other wlroots-style compositors.
//!
//! The library must be linked (not loaded at runtime) so that it precedes
//! libwayland-client; it is behind the default `layer-shell` cargo feature.

use gtk::prelude::*;
use toktop::config::Widget;

#[cfg(feature = "layer-shell")]
mod ffi {
    use gtk::{gdk, glib};

    /// Surface namespace, used by compositor rules such as Hyprland's
    /// `layerrule = …, match:namespace toktop`.
    pub const NAMESPACE: &std::ffi::CStr = c"toktop";

    pub const EDGE_LEFT: i32 = 0;
    pub const EDGE_RIGHT: i32 = 1;
    pub const EDGE_TOP: i32 = 2;
    pub const EDGE_BOTTOM: i32 = 3;
    pub const KEYBOARD_ON_DEMAND: i32 = 2;

    #[link(name = "gtk4-layer-shell")]
    unsafe extern "C" {
        pub fn gtk_layer_is_supported() -> glib::ffi::gboolean;
        pub fn gtk_layer_init_for_window(window: *mut gtk::ffi::GtkWindow);
        pub fn gtk_layer_set_namespace(
            window: *mut gtk::ffi::GtkWindow,
            namespace: *const std::ffi::c_char,
        );
        pub fn gtk_layer_set_layer(window: *mut gtk::ffi::GtkWindow, layer: i32);
        pub fn gtk_layer_set_monitor(
            window: *mut gtk::ffi::GtkWindow,
            monitor: *mut gdk::ffi::GdkMonitor,
        );
        pub fn gtk_layer_set_anchor(
            window: *mut gtk::ffi::GtkWindow,
            edge: i32,
            anchor: glib::ffi::gboolean,
        );
        pub fn gtk_layer_set_margin(window: *mut gtk::ffi::GtkWindow, edge: i32, margin: i32);
        pub fn gtk_layer_set_keyboard_mode(window: *mut gtk::ffi::GtkWindow, mode: i32);
    }
}

/// Whether the compositor supports layer-shell. Requires an open display.
pub fn is_supported() -> bool {
    #[cfg(feature = "layer-shell")]
    {
        // SAFETY: no preconditions beyond GTK being initialized.
        unsafe { ffi::gtk_layer_is_supported() != 0 }
    }
    #[cfg(not(feature = "layer-shell"))]
    false
}

/// Makes `window` a layer surface placed as `config` describes, on
/// `monitor` if given. Must be called before the window is first shown, and
/// only when [`is_supported`].
#[cfg_attr(not(feature = "layer-shell"), allow(unused_variables))]
pub fn place(window: &impl IsA<gtk::Window>, config: &Widget, monitor: Option<&gtk::gdk::Monitor>) {
    #[cfg(feature = "layer-shell")]
    {
        use gtk::glib::translate::ToGlibPtr;
        use toktop::config::Layer;

        let layer = match config.layer {
            Layer::Background => 0,
            Layer::Bottom => 1,
            Layer::Top => 2,
            Layer::Overlay | Layer::Window => 3,
        };
        let window: *mut gtk::ffi::GtkWindow = window.as_ref().to_glib_none().0;
        let (top, bottom, left, right) = config.anchor.edges();
        // SAFETY: `window` is a live, unrealized GtkWindow; the namespace and
        // monitor pointers are valid for the duration of the calls.
        unsafe {
            ffi::gtk_layer_init_for_window(window);
            ffi::gtk_layer_set_namespace(window, ffi::NAMESPACE.as_ptr());
            ffi::gtk_layer_set_layer(window, layer);
            if let Some(monitor) = monitor {
                ffi::gtk_layer_set_monitor(window, monitor.to_glib_none().0);
            }
            for (edge, anchored) in [
                (ffi::EDGE_TOP, top),
                (ffi::EDGE_BOTTOM, bottom),
                (ffi::EDGE_LEFT, left),
                (ffi::EDGE_RIGHT, right),
            ] {
                ffi::gtk_layer_set_anchor(window, edge, anchored.into());
                ffi::gtk_layer_set_margin(window, edge, config.margin);
            }
            // Clicks always work; the keyboard is taken only when clicked.
            ffi::gtk_layer_set_keyboard_mode(window, ffi::KEYBOARD_ON_DEMAND);
        }
    }
}

/// Monitor whose connector is `name` (such as `DP-1`).
pub fn find_monitor(name: &str) -> Option<gtk::gdk::Monitor> {
    if name.is_empty() {
        return None;
    }
    let monitors = gtk::gdk::Display::default()?.monitors();
    let found = (0..monitors.n_items())
        .filter_map(|index| monitors.item(index)?.downcast::<gtk::gdk::Monitor>().ok())
        .find(|monitor| monitor.connector().as_deref() == Some(name));
    if found.is_none() {
        eprintln!("toktop: no monitor named {name}; letting the compositor choose");
    }
    found
}
