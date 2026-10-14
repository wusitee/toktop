//! Screen space reserved by panels, read from Hyprland's IPC socket.
//!
//! Layer-shell clients cannot query the area left free by bars such as
//! waybar, so the widget would grow under them. Other compositors fall back
//! to the full monitor height.

use serde::Deserialize;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Deserialize)]
struct Monitor {
    name: String,
    focused: bool,
    /// Left, top, right and bottom, in logical pixels.
    reserved: [i32; 4],
}

/// A monitor's connector name and the space reserved at its top and bottom.
pub struct Reserved {
    pub connector: String,
    pub top: i32,
    pub bottom: i32,
}

/// Reserved space on the monitor named `connector`, or on the focused
/// monitor, where the compositor places a widget without one.
pub fn reserved(connector: Option<&str>) -> Option<Reserved> {
    let monitors: Vec<Monitor> = serde_json::from_slice(&request(b"j/monitors")?).ok()?;
    let monitor = monitors.into_iter().find(|monitor| match connector {
        Some(name) => monitor.name == name,
        None => monitor.focused,
    })?;
    Some(Reserved {
        connector: monitor.name,
        top: monitor.reserved[1].max(0),
        bottom: monitor.reserved[3].max(0),
    })
}

fn request(command: &[u8]) -> Option<Vec<u8>> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let path = PathBuf::from(runtime)
        .join("hypr")
        .join(signature)
        .join(".socket.sock");
    let mut socket = UnixStream::connect(path).ok()?;
    // Never stall the UI on an unresponsive compositor.
    let timeout = Some(Duration::from_millis(200));
    socket.set_read_timeout(timeout).ok()?;
    socket.set_write_timeout(timeout).ok()?;
    socket.write_all(command).ok()?;
    let mut reply = Vec::new();
    socket.read_to_end(&mut reply).ok()?;
    Some(reply)
}
