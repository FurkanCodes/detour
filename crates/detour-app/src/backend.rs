//! The packet-handling engine behind the Connect button. Windows runs the
//! WinDivert packet engine; other platforms run a local proxy and point the
//! system proxy settings at it.

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

/// Undoes anything a previous run left behind after a crash.
pub fn recover(dir: &std::path::Path) {
    let _ = crate::sysproxy::restore(dir);
}
