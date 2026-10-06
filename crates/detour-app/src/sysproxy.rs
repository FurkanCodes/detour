#![allow(dead_code)] // each OS uses only its own half of these helpers
//! Points the system web proxy at Detour's local proxy, and puts the user's
//! previous settings back afterwards: `networksetup` on macOS, the WinINet
//! registry values on Windows.
//!
//! The command building, parsing and backup formats are plain functions so
//! they are tested on any OS; only touching the system is platform-specific.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Setting {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
}

/// One network service's web (HTTP) and secure web (HTTPS) proxy settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceProxy {
    pub service: String,
    pub web: Setting,
    pub secure: Setting,
}

/// Output of `networksetup -getwebproxy <service>`.
pub fn parse_setting(output: &str) -> Setting {
    let mut s = Setting::default();
    for line in output.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match key.trim() {
            "Enabled" => s.enabled = value.eq_ignore_ascii_case("yes"),
            "Server" => s.host = value.to_owned(),
            "Port" => s.port = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    s
}

/// Output of `networksetup -listallnetworkservices`: a note line first, and a
/// leading `*` marks a disabled service.
pub fn parse_services(output: &str) -> Vec<String> {
    output
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('*'))
        .map(str::to_owned)
        .collect()
}

pub fn encode_state(all: &[ServiceProxy]) -> String {
    let mut out = String::new();
    for p in all {
        let f = |s: &Setting| format!("{}\t{}\t{}", u8::from(s.enabled), s.host, s.port);
        out.push_str(&format!("{}\t{}\t{}\n", p.service, f(&p.web), f(&p.secure)));
    }
    out
}

pub fn decode_state(text: &str) -> Vec<ServiceProxy> {
    let setting = |f: &[&str]| Setting {
        enabled: f[0] == "1",
        host: f[1].to_owned(),
        port: f[2].parse().unwrap_or(0),
    };
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            (f.len() == 7 && !f[0].is_empty()).then(|| ServiceProxy {
                service: f[0].to_owned(),
                web: setting(&f[1..4]),
                secure: setting(&f[4..7]),
            })
        })
        .collect()
}

type Command = Vec<String>;

fn cmd(args: &[&str]) -> Command {
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// `networksetup` invocations that route every service through `127.0.0.1:port`.
pub fn enable_commands(services: &[String], port: u16) -> Vec<Command> {
    let port = port.to_string();
    let mut out = Vec::new();
    for s in services {
        for (set, state) in [
            ("-setwebproxy", "-setwebproxystate"),
            ("-setsecurewebproxy", "-setsecurewebproxystate"),
        ] {
            out.push(cmd(&[set, s, "127.0.0.1", &port]));
            out.push(cmd(&[state, s, "on"]));
        }
    }
    out
}

/// `networksetup` invocations that put the saved settings back.
pub fn restore_commands(saved: &[ServiceProxy]) -> Vec<Command> {
    let mut out = Vec::new();
    for p in saved {
        for (set, state, s) in [
            ("-setwebproxy", "-setwebproxystate", &p.web),
            ("-setsecurewebproxy", "-setsecurewebproxystate", &p.secure),
        ] {
            if s.enabled && !s.host.is_empty() {
                out.push(cmd(&[set, &p.service, &s.host, &s.port.to_string()]));
                out.push(cmd(&[state, &p.service, "on"]));
            } else {
                out.push(cmd(&[state, &p.service, "off"]));
            }
        }
    }
    out
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A shell script running every command, as one AppleScript `do shell script`
/// that asks for administrator rights once.
pub fn elevated_applescript(program: &str, commands: &[Command]) -> String {
    let script = commands
        .iter()
        .map(|c| {
            let args: Vec<String> = c.iter().map(|a| shell_quote(a)).collect();
            format!("{} {}", shell_quote(program), args.join(" "))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let escaped = script.replace('\\', "\\\\").replace('"', "\\\"");
    format!("do shell script \"{escaped}\" with administrator privileges")
}

pub fn backup_path(dir: &Path) -> PathBuf {
    dir.join("system-proxy-backup.txt")
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::process::Command as Process;

    const NETWORKSETUP: &str = "/usr/sbin/networksetup";

    fn output(args: &[&str]) -> Result<String, String> {
        let out = Process::new(NETWORKSETUP)
            .args(args)
            .output()
            .map_err(|e| format!("cannot run networksetup: {e}"))?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Runs the commands as the current user; if macOS refuses, asks once
    /// for administrator rights and runs them all that way.
    fn run(commands: &[Command]) -> Result<(), String> {
        let direct = commands.iter().all(|c| {
            Process::new(NETWORKSETUP)
                .args(c)
                .output()
                .is_ok_and(|o| o.status.success())
        });
        if direct {
            return Ok(());
        }
        let script = elevated_applescript(NETWORKSETUP, commands);
        let out = Process::new("/usr/bin/osascript")
            .args(["-e", &script])
            .output()
            .map_err(|e| format!("cannot run osascript: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "could not change the system proxy: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    pub fn enable(dir: &Path, port: u16) -> Result<(), String> {
        let services = parse_services(&output(&["-listallnetworkservices"])?);
        if services.is_empty() {
            return Err("no active network service found".into());
        }
        let backup = backup_path(dir);
        // Keep the very first backup if a previous run did not clean up.
        if !backup.exists() {
            let saved: Vec<ServiceProxy> = services
                .iter()
                .map(|s| ServiceProxy {
                    service: s.clone(),
                    web: parse_setting(&output(&["-getwebproxy", s]).unwrap_or_default()),
                    secure: parse_setting(&output(&["-getsecurewebproxy", s]).unwrap_or_default()),
                })
                .collect();
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            std::fs::write(&backup, encode_state(&saved)).map_err(|e| e.to_string())?;
        }
        run(&enable_commands(&services, port))
    }

    pub fn restore(dir: &Path) -> Result<(), String> {
        let backup = backup_path(dir);
        let Ok(text) = std::fs::read_to_string(&backup) else {
            return Ok(());
        };
        run(&restore_commands(&decode_state(&text)))?;
        let _ = std::fs::remove_file(backup);
        Ok(())
    }
}

/// The WinINet values Detour overwrites. `None` means the value did not
/// exist, so restoring deletes it again.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WinInetSaved {
    pub enable: Option<u32>,
    pub server: Option<String>,
    pub bypass: Option<String>,
    pub pac_url: Option<String>,
}

pub fn encode_wininet(saved: &WinInetSaved) -> String {
    let mut out = String::new();
    let mut line = |name: &str, value: Option<String>| {
        if let Some(v) = value {
            out.push_str(&format!("{name}\t{v}\n"));
        }
    };
    line("ProxyEnable", saved.enable.map(|v| v.to_string()));
    line("ProxyServer", saved.server.clone());
    line("ProxyOverride", saved.bypass.clone());
    line("AutoConfigURL", saved.pac_url.clone());
    out
}

pub fn decode_wininet(text: &str) -> WinInetSaved {
    let mut saved = WinInetSaved::default();
    for l in text.lines() {
        let Some((name, value)) = l.split_once('\t') else { continue };
        match name {
            "ProxyEnable" => saved.enable = value.parse().ok(),
            "ProxyServer" => saved.server = Some(value.to_owned()),
            "ProxyOverride" => saved.bypass = Some(value.to_owned()),
            "AutoConfigURL" => saved.pac_url = Some(value.to_owned()),
            _ => {}
        }
    }
    saved
}

/// Hosts the browser should reach directly (BypaxDPI's list): local and
/// private addresses, connectivity checks (or Windows shows "no internet"),
/// Windows Update, and game launchers and CDNs that need no help and whose
/// HTTP clients may not cope with a split handshake.
pub fn windows_bypass_list() -> String {
    let private_172: Vec<String> = (16..=31).map(|n| format!("172.{n}.*")).collect();
    let mut hosts: Vec<&str> = vec!["<local>", "localhost", "127.*", "10.*", "192.168.*"];
    hosts.extend(private_172.iter().map(String::as_str));
    hosts.extend([
        "*.msftconnecttest.com",
        "*.msftncsi.com",
        "dns.msn.com",
        "ipv6.msftconnecttest.com",
        "connectivitycheck.gstatic.com",
        "connectivitycheck.android.com",
        "clients3.google.com",
        "play.googleapis.com",
        "captive.apple.com",
        "gsp1.apple.com",
        "connectivitycheck.samsung.com",
        "*.windowsupdate.com",
        "*.delivery.mp.microsoft.com",
        "*.steamcontent.com",
        "*.steamstatic.com",
        "clientconfig.akamai.steamstatic.com",
        "*.cm.steampowered.com",
        "*.epicgames.com",
        "*.unrealengine.com",
        "download.epicgames.com",
        "launcher-public-service-prod06.ol.epicgames.com",
        "*.riotgames.com",
        "*.leagueoflegends.com",
        "riotgames-update.akamaized.net",
        "*.ea.com",
        "*.origin.com",
        "*.blizzard.com",
        "*.battle.net",
        "blzddist1-a.akamaihd.net",
        "*.ubisoft.com",
        "*.ubi.com",
        "*.xboxlive.com",
        "*.xbox.com",
        "*.microsoft.com",
        "*.cachefly.net",
    ]);
    hosts.join(";")
}

/// The bypass list BypaxDPI gives the WinHTTP proxy (system services and
/// native programs): shorter than the browser one.
pub fn winhttp_bypass_list() -> String {
    [
        "<local>",
        "127.0.0.1",
        "*.steamcontent.com",
        "*.steamstatic.com",
        "*.cm.steampowered.com",
        "*.epicgames.com",
        "*.unrealengine.com",
        "*.riotgames.com",
        "*.leagueoflegends.com",
        "*.ea.com",
        "*.origin.com",
        "*.blizzard.com",
        "*.battle.net",
        "*.ubisoft.com",
        "*.ubi.com",
        "*.xboxlive.com",
        "*.xbox.com",
        "*.microsoft.com",
        "*.cachefly.net",
        "*.msftconnecttest.com",
        "*.windowsupdate.com",
    ]
    .join(";")
}

pub fn winhttp_backup_path(dir: &Path) -> PathBuf {
    dir.join("winhttp-proxy-backup.txt")
}

/// The saved `WinHttpSettings` value of each registry view (Windows keeps a
/// 64-bit and a 32-bit copy, each with its own change counter), one line
/// each as hex; `none` where there was none.
pub fn encode_winhttp(views: &[Option<Vec<u8>>]) -> String {
    let line = |v: &Option<Vec<u8>>| match v {
        Some(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        None => "none".to_owned(),
    };
    views.iter().map(|v| line(v) + "\n").collect()
}

/// `Err` for a damaged backup.
pub fn decode_winhttp(text: &str) -> Result<Vec<Option<Vec<u8>>>, ()> {
    let views: Vec<Option<Vec<u8>>> = text
        .lines()
        .map(|line| {
            let line = line.trim();
            if line == "none" {
                return Ok(None);
            }
            if line.is_empty() || line.len() % 2 != 0 {
                return Err(());
            }
            (0..line.len())
                .step_by(2)
                .map(|i| line.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or(()))
                .collect::<Result<Vec<u8>, ()>>()
                .map(Some)
        })
        .collect::<Result<_, ()>>()?;
    if views.is_empty() {
        return Err(());
    }
    Ok(views)
}

const INTERNET_SETTINGS_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";

/// A command line for Windows' one-shot logon list (`RunOnce`) that puts the
/// user's own proxy settings back if Detour never got to: the PC was shut
/// down, or the app was killed, while connected. It acts only if the system
/// proxy still points at `127.0.0.1:port`, so a Detour that started after the
/// logon (with another port) is not undone. `None` if a saved value cannot be
/// quoted safely.
pub fn runonce_command(saved: &WinInetSaved, port: u16) -> Option<String> {
    let unsafe_value = |v: &Option<String>| v.as_deref().is_some_and(|v| v.contains(['"', '\r', '\n', '%']));
    if unsafe_value(&saved.server) || unsafe_value(&saved.bypass) || unsafe_value(&saved.pac_url) {
        return None;
    }
    let key = INTERNET_SETTINGS_KEY;
    let put = |name: &str, kind: &str, value: Option<String>| match value {
        Some(v) => format!("reg add \"{key}\" /v {name} /t {kind} /d \"{v}\" /f"),
        None => format!("reg delete \"{key}\" /v {name} /f"),
    };
    let steps = [
        put("ProxyServer", "REG_SZ", saved.server.clone()),
        put("ProxyOverride", "REG_SZ", saved.bypass.clone()),
        put("AutoConfigURL", "REG_SZ", saved.pac_url.clone()),
        put("ProxyEnable", "REG_DWORD", saved.enable.map(|v| v.to_string())),
    ]
    .join(" & ");
    Some(format!(
        "cmd /d /s /c \"reg query \"{key}\" /v ProxyServer | find \"127.0.0.1:{port}\" >nul && ({steps})\""
    ))
}

#[cfg(windows)]
#[path = "sysproxy_windows.rs"]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{enable, restore};

#[cfg(windows)]
pub use windows::{enable, exempt_store_apps, restore};

#[cfg(not(any(target_os = "macos", windows)))]
pub fn enable(_dir: &Path, _port: u16) -> Result<(), String> {
    Err("setting the system proxy is only implemented for macOS and Windows".into())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn restore(_dir: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GET: &str = "Enabled: Yes\nServer: proxy.corp\nPort: 3128\nAuthenticated Proxy Enabled: 0\n";

    #[test]
    fn parses_proxy_settings() {
        let s = parse_setting(GET);
        assert_eq!((s.enabled, s.host.as_str(), s.port), (true, "proxy.corp", 3128));
        let off = parse_setting("Enabled: No\nServer: \nPort: 0\n");
        assert!(!off.enabled && off.host.is_empty());
        assert_eq!(parse_setting("garbage"), Setting::default());
    }

    #[test]
    fn parses_services_skipping_disabled_ones() {
        let out = "An asterisk (*) denotes that a network service is disabled.\nWi-Fi\n*Thunderbolt Bridge\nUSB 10/100/1000 LAN\n";
        assert_eq!(parse_services(out), ["Wi-Fi", "USB 10/100/1000 LAN"]);
        assert!(parse_services("").is_empty());
    }

    #[test]
    fn state_round_trips() {
        let all = vec![
            ServiceProxy {
                service: "Wi-Fi".into(),
                web: parse_setting(GET),
                secure: Setting::default(),
            },
            ServiceProxy {
                service: "USB 10/100/1000 LAN".into(),
                web: Setting::default(),
                secure: Setting { enabled: true, host: "10.0.0.1".into(), port: 8080 },
            },
        ];
        assert_eq!(decode_state(&encode_state(&all)), all);
        assert!(decode_state("broken line\n\n").is_empty());
    }

    #[test]
    fn enable_points_every_service_at_the_local_proxy() {
        let cmds = enable_commands(&["Wi-Fi".into()], 8899);
        assert_eq!(cmds.len(), 4);
        assert_eq!(cmds[0], ["-setwebproxy", "Wi-Fi", "127.0.0.1", "8899"]);
        assert_eq!(cmds[1], ["-setwebproxystate", "Wi-Fi", "on"]);
        assert_eq!(cmds[2], ["-setsecurewebproxy", "Wi-Fi", "127.0.0.1", "8899"]);
        assert_eq!(cmds[3], ["-setsecurewebproxystate", "Wi-Fi", "on"]);
    }

    #[test]
    fn restore_puts_previous_settings_back_or_switches_off() {
        let saved = vec![ServiceProxy {
            service: "Wi-Fi".into(),
            web: parse_setting(GET),
            secure: Setting::default(),
        }];
        let cmds = restore_commands(&saved);
        assert_eq!(cmds[0], ["-setwebproxy", "Wi-Fi", "proxy.corp", "3128"]);
        assert_eq!(cmds[1], ["-setwebproxystate", "Wi-Fi", "on"]);
        assert_eq!(cmds[2], ["-setsecurewebproxystate", "Wi-Fi", "off"]);
        assert_eq!(cmds.len(), 3);
    }

    #[test]
    fn wininet_backup_round_trips_including_missing_values() {
        let full = WinInetSaved {
            enable: Some(1),
            server: Some("proxy.corp:3128".into()),
            bypass: Some("<local>;10.*".into()),
            pac_url: Some("http://wpad/wpad.dat".into()),
        };
        assert_eq!(decode_wininet(&encode_wininet(&full)), full);
        let none = WinInetSaved::default();
        assert_eq!(encode_wininet(&none), "");
        assert_eq!(decode_wininet(&encode_wininet(&none)), none);
        let some = WinInetSaved { enable: Some(0), ..Default::default() };
        assert_eq!(decode_wininet(&encode_wininet(&some)), some);
        assert_eq!(decode_wininet("junk\nProxyEnable\tx\n"), WinInetSaved::default());
    }

    #[test]
    fn bypass_list_keeps_private_networks_direct() {
        let list = windows_bypass_list();
        let hosts: Vec<&str> = list.split(';').collect();
        for must in ["<local>", "localhost", "127.*", "10.*", "192.168.*", "172.16.*", "172.31.*"] {
            assert!(hosts.contains(&must), "{must}");
        }
        assert!(!hosts.contains(&"172.32.*"));
        assert!(!hosts.iter().any(|h| h.contains("discord") || h.contains("roblox")));
        assert!(hosts.contains(&"*.msftconnecttest.com"), "connectivity checks go direct");
        let winhttp = winhttp_bypass_list();
        assert!(winhttp.starts_with("<local>;127.0.0.1;"));
        assert!(!winhttp.contains("discord") && !winhttp.contains("roblox"));
    }

    #[test]
    fn winhttp_backup_round_trips() {
        let views = vec![Some(vec![0x28u8, 0, 0, 0, 3, 0xab]), Some(vec![0x18u8, 0, 0, 0, 3, 0xab])];
        assert_eq!(decode_winhttp(&encode_winhttp(&views)), Ok(views));
        let mixed = vec![Some(vec![1u8]), None];
        assert_eq!(decode_winhttp(&encode_winhttp(&mixed)), Ok(mixed));
        assert_eq!(decode_winhttp("abc\n"), Err(()));
        assert_eq!(decode_winhttp("zz\nnone\n"), Err(()));
        assert_eq!(decode_winhttp(""), Err(()));
    }

    #[test]
    fn logon_safety_net_restores_only_while_our_proxy_is_still_set() {
        let saved = WinInetSaved {
            enable: Some(1),
            server: Some("proxy.corp:3128".into()),
            bypass: Some("<local>;10.*".into()),
            pac_url: None,
        };
        let c = runonce_command(&saved, 61234).unwrap();
        assert!(c.starts_with("cmd /d /s /c \""), "{c}");
        assert!(c.contains("find \"127.0.0.1:61234\" >nul && ("), "{c}");
        assert!(c.contains("/v ProxyServer /t REG_SZ /d \"proxy.corp:3128\" /f"));
        assert!(c.contains("/v ProxyOverride /t REG_SZ /d \"<local>;10.*\" /f"));
        assert!(c.contains("reg delete") && c.contains("/v AutoConfigURL /f"));
        assert!(c.contains("/v ProxyEnable /t REG_DWORD /d \"1\" /f"));
        assert!(c.ends_with(")\""));

        let nothing = runonce_command(&WinInetSaved::default(), 1).unwrap();
        assert!(!nothing.contains("reg add"), "{nothing}");
        assert!(nothing.contains("/v ProxyEnable /f"));

        for bad in ["a\"b", "x%PATH%", "line\nbreak"] {
            let s = WinInetSaved { server: Some(bad.into()), ..Default::default() };
            assert_eq!(runonce_command(&s, 1), None, "{bad:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "briefly changes this user's real proxy settings"]
    fn windows_registry_round_trip_restores_the_users_settings() {
        let dir = std::env::temp_dir().join(format!("detour-sysproxy-{}", std::process::id()));
        let key = windows::Key::open().unwrap();
        let before = key.load();
        windows::enable(&dir, 45678).unwrap();
        let during = key.load();
        assert_eq!(during.enable, Some(1));
        assert_eq!(during.server.as_deref(), Some("127.0.0.1:45678"));
        assert!(during.pac_url.is_none());
        windows::restore(&dir).unwrap();
        assert_eq!(key.load(), before);
        assert!(!backup_path(&dir).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn elevated_script_quotes_awkward_service_names() {
        let cmds = vec![cmd(&["-setwebproxystate", "Bob's \"Wi-Fi\"", "off"])];
        let script = elevated_applescript("/usr/sbin/networksetup", &cmds);
        assert!(script.starts_with("do shell script \""));
        assert!(script.ends_with("\" with administrator privileges"));
        assert!(script.contains("'Bob'\\\\''s \\\"Wi-Fi\\\"'"), "{script}");
    }
}
