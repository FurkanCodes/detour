//! Small Windows helpers: single instance, "show window" signal, autostart.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, RECT};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
    INFINITE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowRect, MessageBoxW, MB_ICONERROR, MB_OK,
};

const TASK_NAME: &str = "Detour";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MUTEX_NAME: &str = "Global\\DetourSingleInstance";
const SHOW_EVENT: &str = "Global\\DetourShowWindow";

static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// False if another Detour is already running.
pub fn claim_single_instance() -> bool {
    let name = wide(MUTEX_NAME);
    // The handle is intentionally kept for the life of the process.
    unsafe {
        CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// Asks the running instance to show its window.
pub fn signal_existing_instance() {
    let name = wide(SHOW_EVENT);
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if !event.is_null() {
            SetEvent(event);
        }
    }
}

/// Calls `on_request` (from a helper thread) whenever another instance asks
/// this one to show its window.
pub fn listen_for_show_requests(on_request: impl Fn() + Send + 'static) {
    let name = wide(SHOW_EVENT);
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) } as usize;
    if event == 0 {
        return;
    }
    std::thread::spawn(move || loop {
        let event = event as *mut std::ffi::c_void;
        if unsafe { WaitForSingleObject(event, INFINITE) } != 0 {
            return;
        }
        request_show();
        on_request();
    });
}

static LAST_SHAPE: AtomicU64 = AtomicU64::new(0);
static STYLED: AtomicBool = AtomicBool::new(false);

/// Clips the borderless window to a rounded rectangle (or removes the clip
/// when `rounded` is false, e.g. maximized). Transparent windows draw the
/// area outside the corners black on some systems; a window region cuts the
/// corners off for real. Cheap to call every frame: it only acts when the
/// size, radius or mode changed.
pub fn round_window(radius_px: i32, rounded: bool) {
    let title = wide("Detour");
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd.is_null() {
            return;
        }
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut r) == 0 {
            return;
        }
        // Windows 11 draws its own accent-coloured border (and may round the
        // corners again) around borderless windows. It clashes with the
        // border the app paints, so switch both off once.
        if !STYLED.swap(true, Ordering::Relaxed) {
            let no_border: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
            let do_not_round: i32 = 1; // DWMWCP_DONOTROUND
            DwmSetWindowAttribute(hwnd, 34, (&no_border as *const u32).cast(), 4); // DWMWA_BORDER_COLOR
            DwmSetWindowAttribute(hwnd, 33, (&do_not_round as *const i32).cast(), 4); // DWMWA_WINDOW_CORNER_PREFERENCE
        }
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let key = ((w as u64) << 44) ^ ((h as u64) << 24) ^ ((radius_px as u64) << 2) ^ u64::from(rounded) ^ 0x8000_0000_0000_0000;
        if LAST_SHAPE.swap(key, Ordering::Relaxed) == key {
            return;
        }
        if rounded {
            // The first row of a borderless window is not covered by the
            // app's drawing (it shows the window's light-gray background), so
            // the region starts one row down. The system owns the region
            // after SetWindowRgn succeeds.
            let region = CreateRoundRectRgn(0, 1, w + 1, h + 1, radius_px * 2, radius_px * 2);
            SetWindowRgn(hwnd, region, 1);
        } else {
            SetWindowRgn(hwnd, std::ptr::null_mut(), 1);
        }
    }
}

/// Where Detour keeps runtime files (the extracted WinDivert driver).
pub fn runtime_dir() -> Option<std::path::PathBuf> {
    crate::driver::default_dir()
}

pub fn request_show() {
    SHOW_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn take_show_request() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::SeqCst)
}

pub fn error_box(text: &str) {
    let (title, text) = (wide("Detour"), wide(text));
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

/// Local time as `HH:MM:SS`.
pub fn local_hms() -> String {
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
}

fn schtasks() -> Command {
    let mut c = Command::new("schtasks");
    c.creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

pub fn autostart_enabled() -> bool {
    schtasks()
        .args(["/Query", "/TN", TASK_NAME])
        .status()
        .is_ok_and(|s| s.success())
}

/// Starts Detour hidden in the tray at logon, elevated, without a UAC prompt.
pub fn set_autostart(on: bool, exe: &Path) -> Result<(), String> {
    let status = if on {
        let command = format!("\"{}\" --tray", exe.display());
        schtasks()
            .args(["/Create", "/TN", TASK_NAME, "/TR", &command])
            .args(["/SC", "ONLOGON", "/RL", "HIGHEST", "/F"])
            .status()
    } else {
        schtasks()
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .status()
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) if !on => Ok(()),
        Ok(s) => Err(format!("Could not change the startup task ({s})")),
        Err(e) => Err(format!("Could not run schtasks: {e}")),
    }
}
