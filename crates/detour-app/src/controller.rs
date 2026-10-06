//! App logic: owns the settings and the running engine. No UI code here.

use crate::assets::{self, PresetEntry};
use crate::backend::{Backend, Warp};
use crate::settings::{Method, Mode, Resolver, Settings};
use crate::sys;
use detour_core::DomainList;
use detour_engine::Options;
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LOG_LINES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Off,
    Starting,
    On,
    Failed(String),
}

pub struct Controller {
    pub settings: Settings,
    pub status: Status,
    settings_path: Option<PathBuf>,
    driver_dir: Option<PathBuf>,
    presets: Vec<PresetEntry>,
    engine: Option<Backend>,
    pending: Option<PendingStart>,
    hosts: DomainList,
    started: Option<Instant>,
    log: Arc<Mutex<VecDeque<String>>>,
}

struct PendingStart {
    receiver: Receiver<Result<Backend, String>>,
    cancelled: Arc<AtomicBool>,
}

impl Controller {
    pub fn new(
        settings: Settings,
        settings_path: Option<PathBuf>,
        driver_dir: Option<PathBuf>,
    ) -> Self {
        let mut ctl = Self {
            settings,
            status: Status::Off,
            settings_path,
            driver_dir,
            presets: assets::presets(),
            engine: None,
            pending: None,
            hosts: DomainList::new(),
            started: None,
            log: Arc::default(),
        };
        ctl.rebuild_hosts();
        ctl
    }

    pub fn presets(&self) -> &[PresetEntry] {
        &self.presets
    }

    /// The selected preset, falling back to the first if the saved one no
    /// longer exists.
    pub fn preset(&self) -> &PresetEntry {
        self.presets
            .iter()
            .find(|p| p.id == self.settings.preset)
            .unwrap_or(&self.presets[0])
    }

    /// Every domain Detour currently works on.
    pub fn hosts(&self) -> DomainList {
        self.hosts.clone()
    }

    fn rebuild_hosts(&mut self) {
        let mut all = DomainList::new();
        for l in assets::LISTS {
            if self.settings.list_enabled(l.id) {
                all.extend(&DomainList::parse(l.text));
            }
        }
        for d in &self.settings.custom_domains {
            all.insert(d);
        }
        self.hosts = all;
    }

    pub fn log(&self, msg: impl Into<String>) {
        push_log(&self.log, msg.into());
    }

    pub fn log_lines(&self) -> Vec<String> {
        self.log
            .lock()
            .map(|l| l.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn clear_log(&self) {
        if let Ok(mut l) = self.log.lock() {
            l.clear();
        }
    }

    pub fn is_on(&self) -> bool {
        self.status == Status::On
    }

    pub fn enable(&mut self) {
        if self.engine.is_some() || self.pending.is_some() {
            return;
        }
        match self.start_background() {
            Ok(pending) => {
                self.pending = Some(pending);
                self.status = Status::Starting;
                self.log(if self.warp().is_some() {
                    "Starting the WARP tunnel, the proxy and the packet engine…"
                } else {
                    "Starting the proxy…"
                });
            }
            Err(e) => {
                self.log(format!("Could not start: {e}"));
                self.status = Status::Failed(e);
            }
        }
    }

    pub fn disable(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.cancelled.store(true, Ordering::Release);
        }
        if let Some(mut engine) = self.engine.take() {
            if let Err(e) = engine.stop() {
                self.log(format!("Engine reported: {e}"));
            }
            self.log("Protection off");
        }
        self.started = None;
        self.status = Status::Off;
    }

    pub fn toggle(&mut self) {
        if self.engine.is_some() || self.pending.is_some() {
            self.disable();
        } else {
            self.enable();
        }
    }

    /// Applies changed sites or preset to a running engine.
    pub fn restart_if_on(&mut self) {
        if self.engine.is_some() || self.pending.is_some() {
            self.disable();
            self.enable();
        }
    }

    /// Notices an engine that died on its own.
    pub fn poll(&mut self) {
        if let Some(pending) = &self.pending {
            let result = match pending.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(
                    "The startup worker stopped unexpectedly. Try connecting again.".into(),
                )),
            };
            if let Some(result) = result {
                self.pending = None;
                match result {
                    Ok(engine) => {
                        self.engine = Some(engine);
                        self.started = Some(Instant::now());
                        self.status = Status::On;
                        let warp = if self.warp().is_some() { " · WARP tunnel" } else { "" };
                        self.log(format!(
                            "Connected · {} · {}{warp}",
                            self.preset().preset.name,
                            self.settings.mode.title()
                        ));
                    }
                    Err(why) => {
                        self.log(format!("Could not connect: {why}"));
                        self.status = Status::Failed(why);
                    }
                }
            }
        }
        if self.engine.as_ref().is_some_and(|e| !e.is_running()) {
            let mut engine = self.engine.take().unwrap();
            let why = engine
                .join()
                .err()
                .unwrap_or_else(|| "the engine stopped unexpectedly".into());
            self.log(format!("Engine stopped: {why}"));
            self.started = None;
            self.status = Status::Failed(why);
        }
    }

    /// (TLS connections rewritten, DNS queries redirected, time on)
    pub fn stats(&self) -> (u64, u64, Option<Duration>) {
        match &self.engine {
            Some(e) => {
                let c = e.counters();
                (c.tls + c.http, c.dns, self.started.map(|s| s.elapsed()))
            }
            None => (0, 0, None),
        }
    }

    /// (packets or connections handled, errors)
    pub fn packet_stats(&self) -> (u64, u64) {
        self.engine.as_ref().map_or((0, 0), |e| {
            let c = e.counters();
            (c.handled, c.errors)
        })
    }

    pub fn options(&self) -> Result<Options, String> {
        let mut opts = engine_options(&self.preset().preset)?;
        use detour_engine::strategy::SplitPos::{Abs, Sni};
        use detour_engine::StreamSplit;
        // Profile mode runs the preset exactly as written. The other modes
        // are BypaxDPI's: they replace the splitting (the proxy's whole-
        // handshake cuts and the packet engine's positions); the preset's
        // decoy (--fake-ttl) stays for the packet engine, since on some ISPs
        // it is the part that gets other programs through.
        let (split, chunk_size, stream) = match self.settings.mode {
            Mode::Profile => (
                opts.strategy.split.clone(),
                opts.strategy.chunk_size,
                opts.strategy.stream,
            ),
            Mode::Turbo => (vec![Sni(1)], None, StreamSplit::Sni),
            Mode::Balanced => (vec![Abs(1), Sni(1)], Some(2), StreamSplit::Chunk(2)),
            Mode::Strong => (vec![Abs(2), Sni(2)], Some(1), StreamSplit::Chunk(1)),
        };
        if self.settings.mode != Mode::Profile {
            opts.strategy.split = split;
            opts.strategy.chunk_size = chunk_size;
            opts.strategy.stream = stream;
            opts.strategy.disorder = false;
        }
        opts.dns_redirect = match self.settings.resolver {
            Resolver::Profile => opts.dns_redirect,
            Resolver::Cloudflare => Some("1.1.1.1:53".parse().unwrap()),
            Resolver::Google => Some("8.8.8.8:53".parse().unwrap()),
            Resolver::Quad9 => Some("9.9.9.9:53".parse().unwrap()),
            Resolver::System => None,
        };
        opts.verbose = self.settings.detailed_logs;
        // Encrypted DNS goes first, as in BypaxDPI (Cloudflare by default,
        // so "provider DNS" uses it too). The plain redirect above stays set
        // as the fallback when HTTPS lookups fail.
        if self.settings.encrypted_dns {
            opts.doh = match self.settings.resolver {
                Resolver::Profile | Resolver::Cloudflare => Some(Ipv4Addr::new(1, 1, 1, 1)),
                Resolver::Google => Some(Ipv4Addr::new(8, 8, 8, 8)),
                Resolver::Quad9 => Some(Ipv4Addr::new(9, 9, 9, 9)),
                Resolver::System => None,
            };
        }
        opts.block_quic = self.settings.all_sites && self.settings.quic_fallback;
        if !opts.ports.contains(&80) {
            opts.ports.push(80);
        }
        if !opts.ports.contains(&8443) {
            opts.ports.push(8443);
        }
        Ok(opts)
    }

    pub fn set_all_sites(&mut self, all: bool) {
        if self.settings.all_sites != all {
            self.settings.all_sites = all;
            self.save();
            self.restart_if_on();
        }
    }

    fn active_hosts(&self) -> Option<DomainList> {
        if self.settings.all_sites {
            None
        } else {
            Some(self.hosts())
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        if self.settings.mode != mode {
            self.settings.mode = mode;
            self.save();
            self.restart_if_on();
        }
    }

    pub fn set_method(&mut self, method: Method) {
        if self.settings.method != method {
            self.settings.method = method;
            self.save();
            self.restart_if_on();
        }
    }

    pub fn set_warp_browsers(&mut self, on: bool) {
        if self.settings.warp_browsers != on {
            self.settings.warp_browsers = on;
            self.save();
            if self.settings.method == Method::Warp {
                self.restart_if_on();
            }
        }
    }

    /// The WARP tunnel to run, if the method asks for one. Only Windows
    /// has it.
    pub fn warp(&self) -> Option<Warp> {
        (cfg!(windows) && self.settings.method == Method::Warp).then_some(Warp {
            browsers: self.settings.warp_browsers,
        })
    }

    pub fn set_resolver(&mut self, resolver: Resolver) {
        if self.settings.resolver != resolver {
            self.settings.resolver = resolver;
            self.save();
            self.restart_if_on();
        }
    }

    pub fn set_preset(&mut self, id: &str) {
        if self.settings.preset != id {
            self.settings.preset = id.to_owned();
            self.save();
            self.restart_if_on();
        }
    }

    pub fn set_list_enabled(&mut self, id: &str, on: bool) {
        self.settings.set_list_enabled(id, on);
        self.rebuild_hosts();
        self.save();
        self.restart_if_on();
    }

    /// Adds a custom domain. Returns the normalized form, or why it was refused.
    pub fn add_domain(&mut self, input: &str) -> Result<String, String> {
        let mut probe = DomainList::new();
        if !probe.insert(input) {
            return Err(format!("\"{}\" is not a valid domain name", input.trim()));
        }
        let domain = probe.sorted()[0].to_owned();
        if self.settings.custom_domains.contains(&domain) {
            return Err(format!("{domain} is already on your list"));
        }
        self.settings.custom_domains.push(domain.clone());
        self.rebuild_hosts();
        self.save();
        self.restart_if_on();
        Ok(domain)
    }

    pub fn remove_domain(&mut self, domain: &str) {
        self.settings.custom_domains.retain(|d| d != domain);
        self.rebuild_hosts();
        self.save();
        self.restart_if_on();
    }

    pub fn save(&self) {
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.settings.save(path) {
                self.log(format!("Could not save settings: {e}"));
            }
        }
    }

    fn start_background(&self) -> Result<PendingStart, String> {
        let hosts = self.active_hosts();
        if hosts.as_ref().is_some_and(DomainList::is_empty) {
            return Err("No sites selected. Pick some on the Sites tab.".into());
        }
        let opts = self.options()?;
        let warp = self.warp();
        let log = self.log.clone();
        let dir = self
            .driver_dir
            .clone()
            .ok_or("cannot find a folder for Detour's runtime files")?;
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelled.clone();
        std::thread::Builder::new()
            .name("detour-startup".into())
            .spawn(move || {
                let result = (|| {
                    if cancel.load(Ordering::Acquire) {
                        return Err("Startup cancelled".into());
                    }
                    let backend = Backend::start(
                        &dir,
                        opts,
                        hosts,
                        warp,
                        Arc::new(move |line| push_log(&log, line)),
                    )?;
                    if cancel.load(Ordering::Acquire) {
                        // Dropping the backend undoes what it set up.
                        return Err("Startup cancelled".into());
                    }
                    Ok(backend)
                })();
                // A disconnected receiver drops any backend returned after cancel.
                let _ = sender.send(result);
            })
            .map_err(|e| e.to_string())?;
        Ok(PendingStart {
            receiver,
            cancelled,
        })
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.disable();
    }
}

/// Engine options from a preset's arguments. The hostlist argument is
/// ignored: the app passes its own domain list in memory.
pub fn engine_options(preset: &detour_core::Preset) -> Result<Options, String> {
    let args = preset.expand_args(Path::new(""), Path::new("hostlist"));
    Options::parse(&args)?.ok_or_else(|| "preset asks for help output".to_string())
}

fn push_log(log: &Mutex<VecDeque<String>>, msg: String) {
    if let Ok(mut l) = log.lock() {
        if l.len() >= LOG_LINES {
            l.pop_front();
        }
        l.push_back(format!("{}  {msg}", sys::local_hms()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(name: &str) -> (Controller, PathBuf) {
        let dir = std::env::temp_dir().join(format!("detour-ctl-{name}-{}", std::process::id()));
        let path = dir.join("settings.toml");
        (Controller::new(Settings::default(), Some(path), None), dir)
    }

    #[test]
    fn hosts_follow_toggles_and_custom_domains() {
        let (mut c, dir) = controller("hosts");
        let base = c.hosts();
        assert!(base.matches("gateway.discord.gg") && base.matches("tr.rbxcdn.com"));

        c.set_list_enabled("roblox", false);
        assert!(!c.hosts().matches("roblox.com") && c.hosts().matches("discord.com"));

        assert_eq!(c.add_domain(" *.Example.com ").unwrap(), "example.com");
        assert!(c.hosts().matches("www.example.com"));
        assert!(c.add_domain("example.com").is_err());
        assert!(c.add_domain("not a domain").is_err());

        c.remove_domain("example.com");
        assert!(!c.hosts().matches("example.com"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn changes_are_saved() {
        let (mut c, dir) = controller("save");
        c.set_preset("generic");
        c.add_domain("example.org").unwrap();
        let saved = Settings::load(&dir.join("settings.toml"));
        assert_eq!(saved.preset, "generic");
        assert_eq!(saved.custom_domains, ["example.org"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_preset_falls_back() {
        let (mut c, dir) = controller("fallback");
        c.settings.preset = "gone".into();
        assert_eq!(c.preset().id, c.presets()[0].id);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn refuses_to_start_without_sites() {
        let (mut c, dir) = controller("empty");
        c.set_all_sites(false);
        c.set_list_enabled("discord", false);
        c.set_list_enabled("roblox", false);
        c.enable();
        assert!(matches!(&c.status, Status::Failed(m) if m.contains("No sites")));
        assert!(!c.is_on());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_preset_yields_engine_options() {
        let (c, dir) = controller("opts");
        for p in c.presets() {
            engine_options(&p.preset).unwrap();
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mode_and_resolver_overrides_reach_engine() {
        let (mut c, dir) = controller("overrides");
        c.set_mode(Mode::Turbo);
        c.set_resolver(Resolver::Cloudflare);
        let opts = c.options().unwrap();
        assert_eq!(opts.strategy.split.len(), 1);
        assert!(!opts.strategy.disorder);
        assert_eq!(opts.strategy.chunk_size, None);
        assert_eq!(opts.strategy.stream, detour_engine::StreamSplit::Sni);
        assert_eq!(opts.dns_redirect.unwrap().to_string(), "1.1.1.1:53");
        assert_eq!(opts.doh, Some(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!opts.verbose);
        c.set_mode(Mode::Strong);
        c.set_resolver(Resolver::System);
        let opts = c.options().unwrap();
        assert_eq!(opts.strategy.chunk_size, Some(1));
        assert_eq!(opts.strategy.stream, detour_engine::StreamSplit::Chunk(1));
        assert!(!opts.strategy.disorder);
        assert!(opts.dns_redirect.is_none() && opts.doh.is_none());
        let saved = Settings::load(&dir.join("settings.toml"));
        assert_eq!(saved.mode, Mode::Strong);
        assert_eq!(saved.resolver, Resolver::System);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn profile_mode_runs_the_preset_as_written() {
        let (mut c, dir) = controller("profile");
        c.set_resolver(Resolver::Profile);
        for id in ["turk-telekom", "superonline", "generic", "aggressive"] {
            c.set_preset(id);
            let preset = engine_options(&c.preset().preset).unwrap();
            let opts = c.options().unwrap();
            assert_eq!(opts.strategy, preset.strategy, "{id}");
            assert_eq!(opts.dns_redirect, preset.dns_redirect, "{id}");
            assert_eq!(opts.doh, Some(Ipv4Addr::new(1, 1, 1, 1)), "{id}: Cloudflare over HTTPS first, as BypaxDPI");
        }
        c.settings.encrypted_dns = false;
        assert!(c.options().unwrap().doh.is_none());
        c.set_preset("aggressive");
        assert_eq!(c.options().unwrap().strategy.fake_ttl, Some(4));
        assert!(c.options().unwrap().strategy.disorder);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn other_modes_keep_the_presets_decoy() {
        let (mut c, dir) = controller("decoy");
        c.set_preset("turk-telekom");
        for mode in [Mode::Turbo, Mode::Balanced, Mode::Strong] {
            c.set_mode(mode);
            assert_eq!(c.options().unwrap().strategy.fake_ttl, Some(4), "{mode:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn default_scope_covers_any_hostname_without_a_list() {
        let (mut c, dir) = controller("all-sites");
        assert!(c.settings.all_sites);
        assert!(c.active_hosts().is_none());
        assert!(c.options().unwrap().block_quic);
        c.set_list_enabled("discord", false);
        c.set_list_enabled("roblox", false);
        assert!(c.active_hosts().is_none());
        c.set_all_sites(false);
        assert!(c.active_hosts().unwrap().is_empty());
        assert!(!c.options().unwrap().block_quic);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn warp_method_is_saved_and_windows_only() {
        let (mut c, dir) = controller("warp");
        assert!(c.warp().is_none(), "the direct method is the default");
        c.set_method(Method::Warp);
        c.set_warp_browsers(true);
        let expected = cfg!(windows).then_some(Warp { browsers: true });
        assert_eq!(c.warp(), expected);
        let saved = Settings::load(&dir.join("settings.toml"));
        assert_eq!((saved.method, saved.warp_browsers), (Method::Warp, true));
        c.set_method(Method::Detour);
        assert!(c.warp().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cancelled_start_cannot_restore_connection_state() {
        let (mut c, dir) = controller("cancel");
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        c.pending = Some(PendingStart {
            receiver,
            cancelled: cancelled.clone(),
        });
        c.status = Status::Starting;
        c.disable();
        assert!(cancelled.load(Ordering::Acquire));
        assert!(sender.send(Err("late startup error".into())).is_err());
        c.poll();
        assert_eq!(c.status, Status::Off);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_worker_failure_reaches_the_ui() {
        let (mut c, dir) = controller("worker");
        let (sender, receiver) = mpsc::channel();
        c.pending = Some(PendingStart {
            receiver,
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        c.status = Status::Starting;
        sender.send(Err("driver unavailable".into())).unwrap();
        c.poll();
        assert_eq!(c.status, Status::Failed("driver unavailable".into()));
        assert!(c.log_lines().last().unwrap().contains("driver unavailable"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn log_is_capped_and_timestamped() {
        let (c, dir) = controller("log");
        for i in 0..(LOG_LINES + 20) {
            c.log(format!("line {i}"));
        }
        let lines = c.log_lines();
        assert_eq!(lines.len(), LOG_LINES);
        assert!(lines.last().unwrap().ends_with("line 519"));
        assert_eq!(lines[0].as_bytes()[2], b':');
        c.clear_log();
        assert!(c.log_lines().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
