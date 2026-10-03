//! Platform-independent pieces of Detour: configuration, presets and domain lists.

pub mod config;
pub mod domains;

pub use config::{Config, ConfigError, Preset};
pub use domains::DomainList;
