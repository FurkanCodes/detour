//! The engine behind the Connect button: a local proxy the system proxy
//! settings point at. On Windows the WARP method adds a WireSock tunnel and
//! the WinDivert packet engine.

#[cfg(windows)]
#[path = "backend_windows.rs"]
mod imp;
#[cfg(not(windows))]
#[path = "backend_proxy.rs"]
mod imp;

pub use imp::Backend;

pub type LogFn = std::sync::Arc<dyn Fn(String) + Send + Sync>;

#[derive(Debug, Default, Clone, Copy)]
pub struct Counters {
    /// TLS handshakes rewritten.
    pub tls: u64,
    /// Plain HTTP requests rewritten.
    pub http: u64,
    /// DNS lookups redirected.
    pub dns: u64,
    /// Packets (or connections, for the proxy) handled.
    pub handled: u64,
    pub errors: u64,
}

/// The WARP method's choices (Windows only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Warp {
    pub browsers: bool,
}

/// Undoes anything a previous run left behind after a crash.
pub fn recover(dir: &std::path::Path) {
    let _ = crate::sysproxy::restore(dir);
    #[cfg(windows)]
    crate::warp::recover(&crate::warp::dir(dir));
}
