//! Runs the engine named by the active preset and keeps it alive.

pub mod hostlist;
pub mod paths;
pub mod restart;
pub mod runner;
pub mod supervisor;
#[cfg(windows)]
pub mod winservice;

pub use restart::RestartPolicy;
pub use runner::{Launch, Outcome};
pub use supervisor::{EngineCommand, Supervisor, SupervisorError};
