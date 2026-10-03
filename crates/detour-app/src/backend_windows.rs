//! Windows runs two engines side by side:
//!
//! * a local proxy, set as the system proxy, which browsers and other
//!   proxy-aware programs use. It resolves names itself, so a poisoned DNS
//!   answer a browser cached earlier cannot break a site, and switching
//!   Detour off cuts every tunnel that went through it;
//! * the WinDivert packet engine, for programs that ignore the system proxy
//!   (games, launchers, desktop clients).

use super::{Counters, LogFn};
use crate::{driver, sysproxy};
use detour_core::DomainList;
use detour_engine::divert::Api;
use detour_engine::doh;
use detour_engine::proxy::{Proxy, ProxyConfig, ResolveFn};
use detour_engine::runtime::Engine;
use detour_engine::Options;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

static API: Mutex<Option<Arc<Api>>> = Mutex::new(None);

pub struct Backend {
    proxy: Option<Proxy>,
    engine: Option<Engine>,
    dir: PathBuf,
    /// The system proxy points at `proxy` and has not been restored yet.
    system_proxy_set: bool,
}

impl Backend {
    /// `dir` receives the extracted WinDivert driver files and the backup of
    /// the user's own proxy settings.
    pub fn start(
        dir: &Path,
        opts: Options,
        hosts: Option<DomainList>,
        log: LogFn,
    ) -> Result<Self, String> {
        let mut config = ProxyConfig::from_options(&opts, hosts.clone());
        config.fallback = Some(encrypted_fallback(opts.doh.unwrap_or(Ipv4Addr::new(1, 1, 1, 1))));
        let proxy = Proxy::start(config, 0, log.clone());
        let engine = start_engine(dir, opts, hosts, log.clone());

        let mut backend = Backend {
            proxy: None,
            engine: None,
            dir: dir.to_owned(),
            system_proxy_set: false,
        };
        let mut problems = Vec::new();
        match proxy {
            Ok(proxy) => match sysproxy::enable(dir, proxy.addr().port()) {
                Ok(()) => {
                    backend.system_proxy_set = true;
                    backend.proxy = Some(proxy);
                    sysproxy::exempt_store_apps(dir);
                }
                Err(e) => problems.push(format!("browser proxy: {e}")),
            },
            Err(e) => problems.push(format!("browser proxy: cannot start the local proxy: {e}")),
        }
        match engine {
            Ok(engine) => backend.engine = Some(engine),
            Err(e) => problems.push(format!("packet engine: {e}")),
        }

        if backend.proxy.is_none() && backend.engine.is_none() {
            return Err(problems.join("; "));
        }
        for p in problems {
            // One half is enough to work; say which one is missing.
            log(format!("Running without the {p}"));
        }
        Ok(backend)
    }

    pub fn is_running(&self) -> bool {
        self.proxy.as_ref().is_none_or(Proxy::is_running)
            && self.engine.as_ref().is_none_or(Engine::is_running)
    }

    pub fn counters(&self) -> Counters {
        let mut c = Counters::default();
        if let Some(e) = &self.engine {
            let s = &e.stats;
            c.tls += s.tls.load(Ordering::Relaxed);
            c.http += s.http.load(Ordering::Relaxed);
            c.dns += s.dns.load(Ordering::Relaxed);
            c.handled += s.packets.load(Ordering::Relaxed);
            c.errors += s.errors.load(Ordering::Relaxed);
        }
        if let Some(p) = &self.proxy {
            let s = &p.stats;
            c.tls += s.tls.load(Ordering::Relaxed);
            c.http += s.http.load(Ordering::Relaxed);
            c.dns += s.dns.load(Ordering::Relaxed);
            c.handled += s.connections.load(Ordering::Relaxed);
            c.errors += s.errors.load(Ordering::Relaxed);
        }
        c
    }

    /// Gives the user's proxy settings back first (so nothing new is sent to
    /// a proxy that is about to vanish), then cuts the proxy's tunnels and
    /// closes the packet engine.
    pub fn stop(&mut self) -> Result<(), String> {
        let mut result = Ok(());
        if self.system_proxy_set {
            self.system_proxy_set = false;
            result = sysproxy::restore(&self.dir);
        }
        if let Some(mut proxy) = self.proxy.take() {
            proxy.stop();
        }
        if let Some(engine) = self.engine.as_mut() {
            let stopped = engine.stop();
            result = result.and(stopped);
        }
        result
    }

    /// Why the engine ended, if it ended by itself.
    pub fn join(&mut self) -> Result<(), String> {
        let ended = match self.engine.as_mut() {
            Some(engine) => engine.join(),
            None => Ok(()),
        };
        let stopped = self.stop();
        ended.and(stopped)
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Some resolvers return nothing for names they steer per client (Roblox's
/// telemetry hosts, for one). The proxy then asks `server` over HTTPS, which
/// the provider cannot read or poison.
fn encrypted_fallback(server: Ipv4Addr) -> ResolveFn {
    Arc::new(move |name| doh::Client::new(server)?.resolve_a(name))
}

fn start_engine(
    dir: &Path,
    opts: Options,
    hosts: Option<DomainList>,
    log: LogFn,
) -> Result<Engine, String> {
    let api = {
        let mut cached = API.lock().unwrap_or_else(|e| e.into_inner());
        match &*cached {
            Some(api) => api.clone(),
            None => {
                let dll = driver::extract(dir).map_err(|e| e.to_string())?;
                let api = Api::load(&dll).map_err(|e| e.to_string())?;
                *cached = Some(api.clone());
                api
            }
        }
    };
    Engine::start(&api, opts, hosts, log)
}
