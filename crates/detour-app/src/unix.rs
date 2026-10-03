//! macOS (and generic Unix) helpers: single instance, "show window" signal,
//! autostart. Same functions as `win.rs`.

use std::io::Write;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Loopback port that doubles as the single-instance lock and the channel a
/// second launch uses to ask the first one to show its window.
const SIGNAL_PORT: u16 = 47813;
const AGENT_LABEL: &str = "com.detour.app";

static LISTENER: OnceLock<TcpListener> = OnceLock::new();
static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);

/// False if another Detour is already running.
pub fn claim_single_instance() -> bool {
    match TcpListener::bind((Ipv4Addr::LOCALHOST, SIGNAL_PORT)) {
        Ok(listener) => {
            let _ = LISTENER.set(listener);
            true
        }
        Err(_) => false,
    }
}

/// Asks the running instance to show its window.
pub fn signal_existing_instance() {
    if let Ok(mut s) = TcpStream::connect((Ipv4Addr::LOCALHOST, SIGNAL_PORT)) {
        let _ = s.write_all(b"show");
    }
}

/// Calls `on_request` (from a helper thread) whenever another instance asks
/// this one to show its window.
pub fn listen_for_show_requests(on_request: impl Fn() + Send + 'static) {
    let Some(listener) = LISTENER.get().and_then(|l| l.try_clone().ok()) else {
        return;
    };
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if conn.is_ok() {
                request_show();
                on_request();
            }
        }
    });
}

pub fn request_show() {
    SHOW_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn take_show_request() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::SeqCst)
}

pub fn error_box(text: &str) {
    let quoted = text.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display dialog \"{quoted}\" with title \"Detour\" buttons {{\"OK\"}} default button \"OK\" with icon stop"
    );
    let _ = Command::new("osascript").args(["-e", &script]).status();
}

/// Local time as `HH:MM:SS`.
pub fn local_hms() -> String {
    // SAFETY: `tm` is plain data; `localtime_r` fills it from the given time.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(target_os = "macos")]
#[path = "macos_window.rs"]
mod window;
#[cfg(target_os = "macos")]
pub use window::{setup as setup_window, show_in_dock};

#[cfg(not(target_os = "macos"))]
pub fn setup_window(_window: &eframe::CreationContext, _minimize_to_menu_bar: bool) {}

#[cfg(not(target_os = "macos"))]
pub fn show_in_dock(_show: bool) {}

/// Windows clips its borderless window to rounded corners itself; macOS
/// composites the transparent window correctly, so nothing to do here.
pub fn round_window(_radius_px: i32, _rounded: bool) {}

/// Where Detour keeps runtime files (the saved system-proxy settings).
pub fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library").join("Application Support").join("Detour"))
}

fn agent_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| {
        PathBuf::from(h)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{AGENT_LABEL}.plist"))
    })
}

pub fn autostart_enabled() -> bool {
    agent_path().is_some_and(|p| p.exists())
}

/// Starts Detour hidden in the menu bar at login, via a LaunchAgent.
pub fn set_autostart(on: bool, exe: &Path) -> Result<(), String> {
    let path = agent_path().ok_or("cannot find your home folder")?;
    if !on {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let exe = exe.display().to_string().replace('&', "&amp;").replace('<', "&lt;");
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>Label</key><string>{AGENT_LABEL}</string>\n\
         <key>ProgramArguments</key><array><string>{exe}</string><string>--tray</string></array>\n\
         <key>RunAtLoad</key><true/>\n\
         </dict></plist>\n"
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, plist).map_err(|e| format!("Could not write {}: {e}", path.display()))
}
