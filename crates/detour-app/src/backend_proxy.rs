use super::{Counters, LogFn};
use crate::sysproxy;
use detour_core::DomainList;
use detour_engine::proxy::{Proxy, ProxyConfig};
use detour_engine::Options;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

pub struct Backend {
    proxy: Proxy,
    dir: PathBuf,
    restored: bool,
}

impl Backend {
    /// `dir` keeps the backup of the user's own proxy settings.
    pub fn start(
        dir: &Path,
        opts: Options,
        hosts: Option<DomainList>,
        log: LogFn,
    ) -> Result<Self, String> {
        let config = ProxyConfig::from_options(&opts, hosts);
        let proxy = Proxy::start(config, 0, log).map_err(|e| format!("cannot start the local proxy: {e}"))?;
        // On failure `proxy` is dropped here, which stops it.
        sysproxy::enable(dir, proxy.addr().port())?;
        Ok(Backend {
            proxy,
            dir: dir.to_owned(),
            restored: false,
        })
    }

    pub fn is_running(&self) -> bool {
        self.proxy.is_running()
    }

    pub fn counters(&self) -> Counters {
        let s = &self.proxy.stats;
        Counters {
            tls: s.tls.load(Ordering::Relaxed),
            http: s.http.load(Ordering::Relaxed),
            dns: s.dns.load(Ordering::Relaxed),
            handled: s.connections.load(Ordering::Relaxed),
            errors: s.errors.load(Ordering::Relaxed),
        }
    }

    /// Restores the system proxy settings, then stops the proxy.
    pub fn stop(&mut self) -> Result<(), String> {
        let result = if self.restored {
            Ok(())
        } else {
            self.restored = true;
            sysproxy::restore(&self.dir)
        };
        self.proxy.stop();
        result
    }

    pub fn join(&mut self) -> Result<(), String> {
        self.stop()
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
