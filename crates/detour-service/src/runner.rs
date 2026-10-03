use crate::{EngineCommand, Supervisor, SupervisorError};
use detour_core::Config;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

#[derive(Debug, Clone)]
pub struct Launch {
    pub config_path: PathBuf,
    /// Directory the preset's engine path is relative to.
    pub install_dir: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stopped,
    /// `enabled = false` in the config; nothing was started.
    Disabled,
}

pub fn run(
    launch: &Launch,
    stop: &Receiver<()>,
    log: Box<dyn Fn(&str) + Send>,
) -> Result<Outcome, SupervisorError> {
    let cfg = Config::load(&launch.config_path)?;
    if !cfg.enabled {
        log(&format!("disabled in {}", launch.config_path.display()));
        return Ok(Outcome::Disabled);
    }
    let config_dir = launch.config_path.parent().unwrap_or(Path::new("."));
    let command = EngineCommand::prepare(&cfg, config_dir, &launch.install_dir)?;
    log(&format!(
        "preset {:?}: starting {}",
        cfg.preset,
        command.program.display()
    ));
    let mut supervisor = Supervisor::new(command, &cfg);
    supervisor.log = log;
    supervisor.run(stop)?;
    Ok(Outcome::Stopped)
}
