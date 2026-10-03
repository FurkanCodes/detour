//! DPI circumvention logic: finds TLS ClientHello packets for listed hosts
//! and rewrites them (split, reorder, decoy) so SNI-based filters miss the
//! hostname. The packet logic is pure; `divert` and `runtime` do the
//! WinDivert I/O.

pub mod dns;
pub mod dnsclient;
pub mod http;
pub mod options;
pub mod packet;
pub mod proxy;
pub mod strategy;
pub mod tls;

#[cfg(windows)]
pub mod divert;
#[cfg(windows)]
pub mod doh;
#[cfg(windows)]
pub mod runtime;

pub use dns::DnsRedirect;
pub use options::Options;
pub use strategy::{Plan, SplitPos, Strategy};
