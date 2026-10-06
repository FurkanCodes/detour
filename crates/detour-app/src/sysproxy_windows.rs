//! Windows: the WinINet proxy settings that browsers read, kept in
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`.

use super::{
    backup_path, decode_wininet, decode_winhttp, encode_wininet, encode_winhttp, runonce_command,
    windows_bypass_list, winhttp_backup_path, winhttp_bypass_list, WinInetSaved,
};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use windows_sys::Win32::Networking::WinInet::{
    InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, KEY_WRITE,
    REG_BINARY, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const KEY_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";
const RUNONCE_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce";
const RUNONCE_NAME: &str = "DetourRestoreProxy";
/// Where `netsh winhttp` keeps the WinHTTP proxy, in both registry views.
const WINHTTP_PATH: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Internet Settings\\Connections";
const WINHTTP_VALUE: &str = "WinHttpSettings";
const WINHTTP_VIEWS: [u32; 2] = [KEY_WOW64_64KEY, KEY_WOW64_32KEY];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// The Internet Settings key, opened (or created) for reading and writing.
pub struct Key(HKEY);

impl Key {
    pub fn open() -> Result<Key, String> {
        Key::open_at(KEY_PATH)
    }

    fn open_at(path: &str) -> Result<Key, String> {
        Key::open_in(HKEY_CURRENT_USER, path, 0)
    }

    /// `view` is 0 or a `KEY_WOW64_*` flag.
    fn open_in(root: HKEY, path: &str, view: u32) -> Result<Key, String> {
        let mut key: HKEY = std::ptr::null_mut();
        let path = wide(path);
        let status = unsafe {
            RegCreateKeyExW(
                root,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE | view,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if status == 0 {
            Ok(Key(key))
        } else {
            Err(format!("cannot open the Windows registry (error {status})"))
        }
    }

    fn read(&self, name: &str) -> Option<(u32, Vec<u8>)> {
        let name = wide(name);
        let (mut kind, mut size) = (0u32, 0u32);
        let probe = unsafe {
            RegQueryValueExW(
                self.0,
                name.as_ptr(),
                std::ptr::null(),
                &mut kind,
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if probe != 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                name.as_ptr(),
                std::ptr::null(),
                &mut kind,
                buf.as_mut_ptr(),
                &mut size,
            )
        };
        buf.truncate(size as usize);
        (status == 0).then_some((kind, buf))
    }

    fn read_dword(&self, name: &str) -> Option<u32> {
        match self.read(name)? {
            (REG_DWORD, b) if b.len() >= 4 => Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            _ => None,
        }
    }

    fn read_string(&self, name: &str) -> Option<String> {
        let (kind, bytes) = self.read(name)?;
        if kind != REG_SZ {
            return None;
        }
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        Some(String::from_utf16_lossy(&units).trim_end_matches('\0').to_owned())
    }

    fn set(&self, name: &str, kind: u32, data: &[u8]) -> Result<(), String> {
        let wname = wide(name);
        let status = unsafe {
            RegSetValueExW(self.0, wname.as_ptr(), 0, kind, data.as_ptr(), data.len() as u32)
        };
        if status == 0 {
            Ok(())
        } else {
            Err(format!("cannot write {name} (error {status})"))
        }
    }

    fn set_dword(&self, name: &str, v: u32) -> Result<(), String> {
        self.set(name, REG_DWORD, &v.to_le_bytes())
    }

    fn set_string(&self, name: &str, v: &str) -> Result<(), String> {
        let bytes: Vec<u8> = wide(v).iter().flat_map(|u| u.to_le_bytes()).collect();
        self.set(name, REG_SZ, &bytes)
    }

    fn delete(&self, name: &str) {
        let wname = wide(name);
        unsafe { RegDeleteValueW(self.0, wname.as_ptr()) };
    }

    pub fn load(&self) -> WinInetSaved {
        WinInetSaved {
            enable: self.read_dword("ProxyEnable"),
            server: self.read_string("ProxyServer"),
            bypass: self.read_string("ProxyOverride"),
            pac_url: self.read_string("AutoConfigURL"),
        }
    }

    fn apply(&self, s: &WinInetSaved) -> Result<(), String> {
        match &s.server {
            Some(v) => self.set_string("ProxyServer", v)?,
            None => self.delete("ProxyServer"),
        }
        match &s.bypass {
            Some(v) => self.set_string("ProxyOverride", v)?,
            None => self.delete("ProxyOverride"),
        }
        match &s.pac_url {
            Some(v) => self.set_string("AutoConfigURL", v)?,
            None => self.delete("AutoConfigURL"),
        }
        match s.enable {
            Some(v) => self.set_dword("ProxyEnable", v),
            None => {
                self.delete("ProxyEnable");
                Ok(())
            }
        }
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

/// Tells running programs the proxy settings changed, so browsers pick them
/// up without a restart.
fn notify() {
    unsafe {
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
    }
}

/// Points the system proxy at `127.0.0.1:port`, after saving the user's own
/// settings in `dir`.
pub fn enable(dir: &Path, port: u16) -> Result<(), String> {
    let key = Key::open()?;
    let backup = backup_path(dir);
    // Keep the very first backup if a previous run did not clean up.
    if !backup.exists() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(&backup, encode_wininet(&key.load()))
            .map_err(|e| format!("cannot save the current proxy settings: {e}"))?;
    }
    // A script URL would take precedence over the fixed proxy, so it goes.
    let ours = WinInetSaved {
        enable: Some(1),
        server: Some(format!("127.0.0.1:{port}")),
        bypass: Some(windows_bypass_list()),
        pac_url: None,
    };
    if let Err(e) = key.apply(&ours) {
        let _ = restore(dir);
        return Err(e);
    }
    arm_logon_safety_net(dir, port);
    notify();
    if let Err(e) = enable_winhttp(dir, port) {
        // Browsers already use the proxy; only native programs miss out.
        let _ = restore_winhttp(dir);
        eprintln!("WinHTTP proxy not set: {e}");
    }
    Ok(())
}

/// Native programs and system services read the WinHTTP proxy, not the
/// browser one. BypaxDPI sets it too ("game mode"); the user's own value is
/// saved first.
fn enable_winhttp(dir: &Path, port: u16) -> Result<(), String> {
    let backup = winhttp_backup_path(dir);
    if !backup.exists() {
        let saved = WINHTTP_VIEWS
            .iter()
            .map(|&view| {
                let key = Key::open_in(HKEY_LOCAL_MACHINE, WINHTTP_PATH, view)?;
                Ok(key.read(WINHTTP_VALUE).map(|(_, bytes)| bytes))
            })
            .collect::<Result<Vec<_>, String>>()?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(&backup, encode_winhttp(&saved))
            .map_err(|e| format!("cannot save the WinHTTP proxy: {e}"))?;
    }
    netsh(&[
        "winhttp",
        "set",
        "proxy",
        &format!("proxy-server=127.0.0.1:{port}"),
        &format!("bypass-list={}", winhttp_bypass_list()),
    ])
}

/// Puts the saved WinHTTP proxy back: the exact registry value of each view,
/// or a direct connection if there was none.
fn restore_winhttp(dir: &Path) -> Result<(), String> {
    let backup = winhttp_backup_path(dir);
    let Ok(text) = std::fs::read_to_string(&backup) else {
        return Ok(());
    };
    match decode_winhttp(&text) {
        Ok(views) if views.len() == WINHTTP_VIEWS.len() && views.iter().any(Option::is_some) => {
            for (view, value) in WINHTTP_VIEWS.iter().zip(&views) {
                let key = Key::open_in(HKEY_LOCAL_MACHINE, WINHTTP_PATH, *view)?;
                match value {
                    Some(v) => key.set(WINHTTP_VALUE, REG_BINARY, v)?,
                    None => key.delete(WINHTTP_VALUE),
                }
            }
        }
        // Nothing was set, or the backup is damaged: a direct connection is
        // the safe guess.
        _ => netsh(&["winhttp", "reset", "proxy"])?,
    }
    let _ = std::fs::remove_file(backup);
    Ok(())
}

fn netsh(args: &[&str]) -> Result<(), String> {
    let status = Command::new("netsh")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run netsh: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("netsh {} failed ({status})", args.join(" ")))
    }
}

/// If Windows shuts down or Detour is killed while connected, the system
/// proxy would keep pointing at a port nobody listens on and every browser
/// would stop working. A one-shot logon entry puts the saved settings back.
fn arm_logon_safety_net(dir: &Path, port: u16) {
    let saved = std::fs::read_to_string(backup_path(dir)).map(|t| decode_wininet(&t));
    let command = saved.ok().and_then(|s| runonce_command(&s, port));
    if let (Some(command), Ok(run_once)) = (command, Key::open_at(RUNONCE_PATH)) {
        let _ = run_once.set_string(RUNONCE_NAME, &command);
    }
}

fn disarm_logon_safety_net() {
    if let Ok(run_once) = Key::open_at(RUNONCE_PATH) {
        run_once.delete(RUNONCE_NAME);
    }
}

/// Puts the saved settings back. A no-op when there is nothing saved.
pub fn restore(dir: &Path) -> Result<(), String> {
    let winhttp = restore_winhttp(dir);
    let backup = backup_path(dir);
    let Ok(text) = std::fs::read_to_string(&backup) else {
        return winhttp;
    };
    let key = Key::open()?;
    key.apply(&decode_wininet(&text))?;
    disarm_logon_safety_net();
    let _ = std::fs::remove_file(backup);
    // Answers cached while Detour was on (or poisoned ones cached while it
    // was off) should not outlive the switch.
    detour_engine::doh::flush_cache();
    notify();
    winhttp
}

/// Store apps run in an AppContainer that may not talk to 127.0.0.1, so with
/// the system proxy pointing there they would lose their network. This
/// exempts the installed ones from loopback isolation (once; in the
/// background, since it runs one command per package).
pub fn exempt_store_apps(dir: &Path) {
    let marker = dir.join("store-apps-loopback.done");
    if marker.exists() {
        return;
    }
    let script = concat!(
        "$ok = $true; ",
        "Get-AppxPackage -ErrorAction SilentlyContinue | ForEach-Object { ",
        "if ($_.PackageFamilyName) { ",
        "CheckNetIsolation.exe LoopbackExempt -a \"-n=$($_.PackageFamilyName)\" | Out-Null; ",
        "if ($LASTEXITCODE -ne 0) { $ok = $false } } }; ",
        "if (-not $ok) { exit 1 }"
    );
    std::thread::spawn(move || {
        let done = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if done {
            let _ = std::fs::write(marker, b"done");
        }
    });
}
