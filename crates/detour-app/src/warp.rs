//! The WARP method: chosen programs go through a free Cloudflare WARP
//! tunnel, run by WireSock Secure Connect, the way SplitWire-Turkey does it.
//!
//! The first connect downloads WireSock and wgcf (both pinned by SHA-256),
//! installs WireSock and registers a WARP device with wgcf. Every connect
//! then writes a WireGuard profile limited to the chosen programs
//! (`AllowedApps`) and has WireSock bring it up; disconnect takes it down.

use crate::backend::LogFn;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The profile name Detour imports into WireSock.
const PROFILE: &str = "detour-warp";
/// Exists while Detour's profile may be connected, so a crash can be undone.
const MARKER: &str = "connected";
/// wgcf writes WARP's entry point as a hostname, which the provider's DNS
/// could poison; Cloudflare documents this as its address.
const ENDPOINT_HOST: &str = "engage.cloudflareclient.com";
const ENDPOINT_IP: &str = "162.159.192.1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CLI_TIMEOUT: Duration = Duration::from_secs(20);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct Pinned {
    name: &'static str,
    url: &'static str,
    file: &'static str,
    sha256: &'static str,
}

/// WireSock Secure Connect, free for personal and non-profit use
/// (https://www.wiresock.net/license/wiresock_eula). Hash as published on
/// https://www.wiresock.net/wiresock-secure-connect/download.
const WIRESOCK: Pinned = Pinned {
    name: "WireSock Secure Connect 3.6.1.1 (17 MB)",
    url: "https://wiresock.net/_api/download-release.php?product=wiresock-secure-connect&platform=windows_x64&version=3.6.1.1",
    file: "wiresock-secure-connect-x64-3.6.1.1.exe",
    sha256: "734f3090ba095a698f7293ff90ebd6dc64fcd646277011dfa3648ac5dbdf5d37",
};

/// wgcf (MIT) registers a WARP device and writes its WireGuard profile.
/// Hash from the release's asset digest.
const WGCF: Pinned = Pinned {
    name: "wgcf 2.3.0 (15 MB)",
    url: "https://github.com/ViRb3/wgcf/releases/download/v2.3.0/wgcf_2.3.0_windows_amd64.exe",
    file: "wgcf.exe",
    sha256: "5c633e265fb969c9507b68b39c5b0762e39181ce9f578ab66cb3b4edb0696ef5",
};

const APPS: [&str; 6] = [
    "Discord.exe",
    "DiscordPTB.exe",
    "DiscordCanary.exe",
    "RobloxPlayerBeta.exe",
    "RobloxPlayerInstaller.exe",
    "RobloxStudioBeta.exe",
];
/// Their install folders under %LOCALAPPDATA%, which also hold their updaters.
const APP_FOLDERS: [&str; 4] = ["Discord", "DiscordPTB", "DiscordCanary", "Roblox"];
const BROWSERS: [&str; 10] = [
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "opera.exe",
    "brave.exe",
    "vivaldi.exe",
    "browser.exe",
    "zen.exe",
    "librewolf.exe",
    "chromium.exe",
];

/// Where the downloads, the WARP account and the profile live: next to the
/// driver folder, in `%LOCALAPPDATA%\Detour\warp`.
pub fn dir(runtime: &Path) -> PathBuf {
    runtime.parent().unwrap_or(runtime).join("warp")
}

/// The programs to tunnel. Browsers send most of their traffic through
/// Detour's local proxy, so tunnelling them also means tunnelling Detour.
pub fn apps(browsers: bool) -> Vec<String> {
    let mut apps: Vec<String> = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        apps.extend(
            APP_FOLDERS
                .iter()
                .map(|f| local.join(f))
                .filter(|p| p.is_dir())
                .map(|p| p.display().to_string()),
        );
    }
    apps.extend(APPS.map(String::from));
    if browsers {
        apps.extend(BROWSERS.map(String::from));
        if let Some(own) = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        {
            apps.push(own);
        }
    }
    apps
}

/// WireSock holds one connection. Connects and disconnects take turns, and
/// the number counts connects: a tunnel whose start was abandoned (the user
/// changed a setting meanwhile) must not take down the newer one.
static LATEST: Mutex<u64> = Mutex::new(0);

/// A connected WARP tunnel. Dropping it disconnects.
pub struct Tunnel {
    cli: PathBuf,
    marker: PathBuf,
    id: u64,
    connected: bool,
}

impl Tunnel {
    /// Installs and registers whatever is missing, then connects. Slow the
    /// first time (downloads and an install); a few seconds afterwards.
    pub fn connect(dir: &Path, apps: &[String], log: &LogFn) -> Result<Self, String> {
        let mut latest = LATEST.lock().unwrap_or_else(|e| e.into_inner());
        *latest += 1;
        let id = *latest;
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let cli = match find_cli() {
            Some(cli) => cli,
            None => {
                install(dir, log)?;
                find_cli().ok_or(
                    "WireSock was installed but its command-line tool is missing. \
                     Restart Windows, then connect again.",
                )?
            }
        };
        start_services()?;
        let base = profile(dir, log)?;
        let conf = dir.join(format!("{PROFILE}.conf"));
        std::fs::write(&conf, with_apps(&base, apps))
            .map_err(|e| format!("cannot write {}: {e}", conf.display()))?;

        // Import refuses an existing name, so replace the previous copy.
        let _ = run(&cli, &["disconnect"], None, CLI_TIMEOUT);
        let _ = run(&cli, &["delete", PROFILE], None, CLI_TIMEOUT);
        let (code, out) = run(&cli, &["import", &conf.to_string_lossy()], None, CLI_TIMEOUT)?;
        if code != 0 {
            return Err(format!("WireSock could not import the WARP profile: {}", last_line(&out)));
        }
        let marker = dir.join(MARKER);
        let _ = std::fs::write(&marker, PROFILE);
        log("Connecting to Cloudflare WARP\u{2026}".into());
        let failure = match run(&cli, &["connect", PROFILE, "-exit"], None, CONNECT_TIMEOUT) {
            Ok((0, _)) => None,
            Ok((code, out)) => Some(format!("WireSock exit code {code}: {}", last_line(&out))),
            Err(e) => Some(e),
        };
        if let Some(why) = failure {
            let _ = take_down(&cli, &marker);
            return Err(format!(
                "could not reach Cloudflare WARP ({why}). Your provider may be blocking it; \
                 switch back to the Detour method in Settings."
            ));
        }
        Ok(Tunnel {
            cli,
            marker,
            id,
            connected: true,
        })
    }

    pub fn disconnect(&mut self) -> Result<(), String> {
        if !std::mem::take(&mut self.connected) {
            return Ok(());
        }
        let latest = LATEST.lock().unwrap_or_else(|e| e.into_inner());
        if *latest != self.id {
            return Ok(());
        }
        take_down(&self.cli, &self.marker)
    }
}

fn take_down(cli: &Path, marker: &Path) -> Result<(), String> {
    match run(cli, &["disconnect"], None, CLI_TIMEOUT) {
        Ok((0, _)) => {
            let _ = std::fs::remove_file(marker);
            Ok(())
        }
        Ok((code, out)) => Err(format!("WARP disconnect failed ({code}): {}", last_line(&out))),
        Err(e) => Err(format!("WARP disconnect failed: {e}")),
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.disconnect();
    }
}

/// Takes down a tunnel a crashed or killed Detour left up.
pub fn recover(dir: &Path) {
    let marker = dir.join(MARKER);
    if !marker.exists() {
        return;
    }
    match find_cli() {
        Some(cli) => {
            let _ = take_down(&cli, &marker);
        }
        None => {
            let _ = std::fs::remove_file(marker);
        }
    }
}

/// `profile` (as wgcf writes it) limited to `apps`, with the entry point
/// given as an address.
fn with_apps(profile: &str, apps: &[String]) -> String {
    let mut out = String::new();
    for line in profile.lines() {
        let key = line.split('=').next().unwrap_or("").trim();
        if key.eq_ignore_ascii_case("AllowedApps") || key.eq_ignore_ascii_case("DisallowedApps") {
            continue;
        }
        if key.eq_ignore_ascii_case("Endpoint") {
            out.push_str(&line.replace(ENDPOINT_HOST, ENDPOINT_IP));
            out.push_str("\r\n");
            out.push_str(&format!("AllowedApps = {}\r\n", apps.join(", ")));
        } else {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out
}

fn find_cli() -> Option<PathBuf> {
    ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join("WireSock Secure Connect"))
        .flat_map(|base| {
            [
                base.join("command-line").join("wiresock-connect-cli.exe"),
                base.join("bin").join("wiresock-connect-cli.exe"),
            ]
        })
        .find(|p| p.is_file())
}

fn install(dir: &Path, log: &LogFn) -> Result<(), String> {
    let installer = fetch(dir, &WIRESOCK, log)?;
    log("Installing WireSock\u{2026}".into());
    // No captured output: the installer's own child processes would keep
    // the pipes open after it exits.
    let mut child = command(&installer, &["/quiet", "/norestart"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run the WireSock installer: {e}"))?;
    let code = wait(&mut child, &installer, Duration::from_secs(300))?;
    let _ = std::fs::remove_file(&installer);
    match code {
        0 => Ok(()),
        3010 | 1641 => Err("WireSock is installed but needs a restart. Restart Windows, then connect again.".into()),
        code => Err(format!("the WireSock installer failed (exit code {code})")),
    }
}

/// WireSock's services must exist (a fresh install may need a restart to
/// register them) and run.
fn start_services() -> Result<(), String> {
    let sc = system32("sc.exe");
    let (code, _) = run(&sc, &["query", "WireSockConnectService"], None, CLI_TIMEOUT)?;
    if code != 0 {
        return Err("WireSock's service is not registered yet. Restart Windows, then connect again.".into());
    }
    for service in ["WireSockAppService", "WireSockConnectService"] {
        // Fails harmlessly when it is already running.
        let _ = run(&sc, &["start", service], None, CLI_TIMEOUT);
    }
    Ok(())
}

/// The WARP device's WireGuard profile, registering the device the first
/// time. The account is kept, so later connects need no registration.
fn profile(dir: &Path, log: &LogFn) -> Result<String, String> {
    let path = dir.join("wgcf-profile.conf");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if text.contains("PrivateKey") {
            return Ok(text);
        }
    }
    let wgcf = fetch(dir, &WGCF, log)?;
    if !dir.join("wgcf-account.toml").is_file() {
        log("Creating a free Cloudflare WARP account\u{2026}".into());
        let (code, out) = run(&wgcf, &["register", "--accept-tos"], Some(dir), Duration::from_secs(60))?;
        if code != 0 {
            let _ = std::fs::remove_file(dir.join("wgcf-account.toml"));
            return Err(format!(
                "Cloudflare did not create a WARP account: {}. Try again in a few minutes.",
                last_line(&out)
            ));
        }
    }
    let (code, out) = run(&wgcf, &["generate", "--keepalive=25"], Some(dir), Duration::from_secs(60))?;
    if code != 0 {
        return Err(format!("could not get the WARP profile: {}", last_line(&out)));
    }
    std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// Downloads `pin` into `dir` unless a verified copy is already there.
fn fetch(dir: &Path, pin: &Pinned, log: &LogFn) -> Result<PathBuf, String> {
    let path = dir.join(pin.file);
    if sha256(&path).is_some_and(|h| h == pin.sha256) {
        return Ok(path);
    }
    log(format!("Downloading {}\u{2026}", pin.name));
    let part = dir.join(format!("{}.part", pin.file));
    let curl = system32("curl.exe");
    let args = ["-fsSL", "--retry", "2", "--connect-timeout", "20", "-o", &part.to_string_lossy(), pin.url];
    let (code, out) = run(&curl, &args, None, Duration::from_secs(600))?;
    if code != 0 {
        let _ = std::fs::remove_file(&part);
        return Err(format!("could not download {}: {}", pin.name, last_line(&out)));
    }
    if sha256(&part).as_deref() != Some(pin.sha256) {
        let _ = std::fs::remove_file(&part);
        return Err(format!("the {} download did not match its checksum", pin.name));
    }
    std::fs::rename(&part, &path).map_err(|e| format!("cannot save {}: {e}", path.display()))?;
    Ok(path)
}

fn sha256(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match file.read(&mut buf).ok()? {
            0 => break,
            n => hasher.update(&buf[..n]),
        }
    }
    Some(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn system32(exe: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join(exe)
}

/// Runs `program` without a console window and returns its exit code and
/// combined output. It is killed after `timeout`.
fn run(program: &Path, args: &[&str], cwd: Option<&Path>, timeout: Duration) -> Result<(i32, String), String> {
    let mut cmd = command(program, args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot run {}: {e}", file_name(program)))?;
    let readers = [
        child.stdout.take().map(|s| reader(Box::new(s))),
        child.stderr.take().map(|s| reader(Box::new(s))),
    ];
    let code = wait(&mut child, program, timeout)?;
    let output = readers
        .into_iter()
        .flatten()
        .filter_map(|r| r.join().ok())
        .collect::<Vec<_>>()
        .join("\n");
    Ok((code, output))
}

fn command(program: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// The exit code, or an error once `timeout` passes (the process is then
/// killed; output readers are left to finish on their own).
fn wait(child: &mut Child, program: &Path, timeout: Duration) -> Result<i32, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code().unwrap_or(-1)),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} did not finish within {} seconds",
                    file_name(program),
                    timeout.as_secs_f32()
                ));
            }
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn reader(mut pipe: Box<dyn Read + Send>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn last_line(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("no details")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WGCF_PROFILE: &str = "[Interface]\n\
        PrivateKey = abc=\n\
        Address = 172.16.0.2/32, 2606:4700:110:8a36::1/128\n\
        DNS = 1.1.1.1, 1.0.0.1, 2606:4700:4700::1111, 2606:4700:4700::1001\n\
        MTU = 1280\n\
        [Peer]\n\
        PublicKey = bmXOC+F1FxEMF9dyiK2H5/1SUtzH0JuVo51h2wPfgyo=\n\
        AllowedIPs = 0.0.0.0/0, ::/0\n\
        Endpoint = engage.cloudflareclient.com:2408\n\
        PersistentKeepalive = 25\n";

    #[test]
    fn profile_is_limited_to_the_apps() {
        let apps = vec!["Discord.exe".to_owned(), "C:\\Users\\x\\AppData\\Local\\Discord".to_owned()];
        let conf = with_apps(WGCF_PROFILE, &apps);
        assert!(conf.contains("Endpoint = 162.159.192.1:2408\r\nAllowedApps = Discord.exe, C:\\Users\\x\\AppData\\Local\\Discord\r\n"));
        assert!(conf.contains("PersistentKeepalive = 25\r\n"));
        assert!(!conf.contains(ENDPOINT_HOST));
        // Rewriting an already rewritten profile keeps one AllowedApps line.
        let again = with_apps(&conf, &apps[..1]);
        assert_eq!(again.matches("AllowedApps").count(), 1);
        assert!(again.contains("AllowedApps = Discord.exe\r\n"));
    }

    #[test]
    fn browsers_bring_detour_along() {
        let base = apps(false);
        assert!(base.iter().any(|a| a == "Discord.exe"));
        assert!(!base.iter().any(|a| a == "chrome.exe"));
        let all = apps(true);
        assert!(all.iter().any(|a| a == "chrome.exe"));
        let own = std::env::current_exe().unwrap();
        let own = own.file_name().unwrap().to_string_lossy();
        assert!(all.iter().any(|a| *a == own), "the proxy's own process must be tunnelled too");
    }

    #[test]
    fn warp_files_sit_next_to_the_driver_folder() {
        assert_eq!(dir(Path::new("C:\\x\\Detour\\driver")), Path::new("C:\\x\\Detour\\warp"));
    }

    #[test]
    fn hashes_files() {
        let path = std::env::temp_dir().join(format!("detour-warp-hash-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(&path).unwrap();
        assert!(sha256(&path).is_none());
    }

    #[test]
    fn runs_programs_and_times_out() {
        let cmd = system32("cmd.exe");
        let (code, out) = run(&cmd, &["/c", "echo", "hi&&", "exit", "3"], None, Duration::from_secs(10)).unwrap();
        assert_eq!((code, out.trim()), (3, "hi"));
        let started = Instant::now();
        let ping = system32("PING.EXE");
        let err = run(&ping, &["-n", "30", "127.0.0.1"], None, Duration::from_millis(500)).unwrap_err();
        assert!(err.contains("did not finish"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
