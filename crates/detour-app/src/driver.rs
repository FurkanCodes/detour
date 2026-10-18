//! Unpacks the embedded WinDivert files so the DLL can be loaded.

use crate::assets::{WINDIVERT_DLL, WINDIVERT_SYS};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
    SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_STATUS, SERVICE_STOP,
    SERVICE_STOPPED,
};

pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Detour").join("driver"))
}

/// Writes the DLL and driver into `dir` (skipping files that already have
/// the right content) and returns the DLL path.
pub fn extract(dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for (name, bytes) in [
        ("WinDivert.dll", WINDIVERT_DLL),
        ("WinDivert64.sys", WINDIVERT_SYS),
    ] {
        let path = dir.join(name);
        if std::fs::read(&path).is_ok_and(|existing| existing == bytes) {
            continue;
        }
        std::fs::write(&path, bytes).map_err(|e| {
            std::io::Error::new(e.kind(), format!("cannot write {}: {e}", path.display()))
        })?;
    }
    Ok(dir.join("WinDivert.dll"))
}

/// Unloads the WinDivert driver. Otherwise it stays loaded until Windows
/// restarts, long after Detour quits, and some games' anti-cheat refuses to
/// run or log in while it is. WinDivert marks its service for deletion when
/// it installs it, so stopping also removes it; a program still using the
/// driver keeps it until it lets go.
pub fn unload() {
    let name: Vec<u16> = "WinDivert".encode_utf16().chain([0]).collect();
    unsafe {
        let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return;
        }
        let service = OpenServiceW(manager, name.as_ptr(), SERVICE_STOP | SERVICE_QUERY_STATUS);
        if !service.is_null() {
            let mut status: SERVICE_STATUS = std::mem::zeroed();
            if ControlService(service, SERVICE_CONTROL_STOP, &mut status) != 0 {
                // Wait until it is gone, so a reconnect right after (a
                // changed setting) can install it afresh.
                let deadline = Instant::now() + Duration::from_secs(3);
                while status.dwCurrentState != SERVICE_STOPPED && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                    if QueryServiceStatus(service, &mut status) == 0 {
                        break;
                    }
                }
            }
            CloseServiceHandle(service);
        }
        CloseServiceHandle(manager);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_once_and_repairs_damage() {
        let dir = std::env::temp_dir().join(format!("detour-driver-{}", std::process::id()));
        let dll = extract(&dir).unwrap();
        assert_eq!(std::fs::read(&dll).unwrap(), WINDIVERT_DLL);
        assert_eq!(
            std::fs::read(dir.join("WinDivert64.sys")).unwrap(),
            WINDIVERT_SYS
        );

        std::fs::write(&dll, b"corrupt").unwrap();
        extract(&dir).unwrap();
        assert_eq!(std::fs::read(&dll).unwrap(), WINDIVERT_DLL);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
