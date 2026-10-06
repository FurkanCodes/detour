//! Windows works the way BypaxDPI does: a local proxy, set as the system
//! (WinINet and WinHTTP) proxy, sends each ClientHello one byte per segment
//! and resolves names over encrypted DNS. It resolves names itself, so a
//! poisoned answer a browser cached earlier cannot break a site, and
//! switching Detour off cuts every tunnel that went through it.
//!
//! The WARP method adds a WireSock tunnel for chosen programs (see `warp`),
//! plus the WinDivert packet engine to fix their DNS.

use super::{Counters, LogFn, Warp};
use crate::warp::{self, Tunnel};
use crate::{driver, sysproxy};
use detour_core::DomainList;
use detour_engine::divert::Api;
use detour_engine::doh;
use detour_engine::proxy::{Proxy, ProxyConfig, Resolver};
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
    tunnel: Option<Tunnel>,
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
        warp: Option<Warp>,
        log: LogFn,
    ) -> Result<Self, String> {
        // The tunnel comes first: when the user picked WARP and it cannot
        // connect, nothing else is set up and they see why.
        let tunnel = match warp {
            Some(w) => {
                log("Setting up the WARP tunnel\u{2026}".into());
                Some(Tunnel::connect(&warp::dir(dir), &warp::apps(w.browsers), &log)?)
            }
            None => None,
        };
        let mut config = ProxyConfig::from_options(&opts, hosts.clone());
        if let Some(server) = opts.doh {
            // As BypaxDPI does: names go to encrypted DNS first, which the
            // provider can neither read nor poison; the plain resolver stays
            // behind it for when it gives no answer.
            match doh::probe(server).and_then(|()| doh::Client::new(server)) {
                Ok(client) => config.resolvers.insert(0, encrypted(server, client)),
                Err(e) => log(format!("Encrypted DNS via {server} is unreachable ({e}); using plain DNS")),
            }
        }
        let proxy = Proxy::start(config, 0, log.clone());
        // Like BypaxDPI, the direct method is the proxy alone: nothing
        // rewrites packets of programs that ignore it. With the WARP method
        // the packet engine only fixes DNS, since packets cannot be told
        // apart by program and a decoy ClientHello sent into the tunnel
        // would reach the server (its hop count starts again at Cloudflare).
        let engine = match &tunnel {
            Some(_) if opts.dns_redirect.is_some() || opts.doh.is_some() || opts.block_quic => {
                let mut engine_opts = opts;
                engine_opts.ports.clear();
                Some(start_engine(dir, engine_opts, hosts, log.clone()))
            }
            _ => None,
        };

        let mut backend = Backend {
            proxy: None,
            engine: None,
            tunnel,
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
            Some(Ok(engine)) => backend.engine = Some(engine),
            Some(Err(e)) => problems.push(format!("packet engine: {e}")),
            None => {}
        }

        if backend.proxy.is_none() && backend.engine.is_none() && backend.tunnel.is_none() {
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
    /// a proxy that is about to vanish), then cuts the proxy's tunnels,
    /// closes the packet engine and takes the WARP tunnel down.
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
        if let Some(mut tunnel) = self.tunnel.take() {
            result = result.and(tunnel.disconnect());
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

/// DNS over HTTPS through one shared client, so lookups reuse its connection.
fn encrypted(server: Ipv4Addr, client: doh::Client) -> Resolver {
    let client = Arc::new(client);
    Resolver {
        name: format!("https://{server}"),
        lookup: Arc::new(move |name| client.resolve_a(name)),
    }
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
