use crate::{hostlist, RestartPolicy};
use detour_core::{Config, ConfigError, Preset};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(200);

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("cannot write hostlist {path}: {source}")]
    Hostlist {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot start engine {path}: {source}")]
    Spawn {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("lost track of engine process: {0}")]
    Wait(std::io::Error),
    #[error("engine exited ({0}) and auto_restart is off")]
    Exited(ExitStatus),
    #[error("engine exited {count} times within {window_secs}s, giving up (last: {last})")]
    GaveUp {
        count: usize,
        window_secs: u64,
        last: ExitStatus,
    },
}

/// A fully resolved engine invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Where the engine's stdout/stderr go (a service has no console).
    pub log_file: Option<PathBuf>,
}

const MAX_LOG_BYTES: u64 = 1 << 20;

impl EngineCommand {
    /// Loads the active preset, writes its merged hostlist to
    /// `<config_dir>/run/hostlist.txt` and resolves the engine path
    /// against `install_dir`. The engine log is `<config_dir>/run/engine.log`.
    pub fn prepare(
        cfg: &Config,
        config_dir: &Path,
        install_dir: &Path,
    ) -> Result<Self, SupervisorError> {
        let preset = Preset::load(&cfg.preset_path(config_dir))?;
        let lists_dir = cfg.resolve(config_dir, &cfg.lists_dir);
        let hostlist_path = config_dir.join("run").join("hostlist.txt");

        let list = hostlist::merge(&preset, &lists_dir)?;
        if list.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "preset {:?} has no domains; add some with `detour list add <domain>`",
                cfg.preset
            ))
            .into());
        }
        hostlist::write(&list, &hostlist_path).map_err(|source| SupervisorError::Hostlist {
            path: hostlist_path.clone(),
            source,
        })?;

        let log_file = config_dir.join("run").join("engine.log");
        if std::fs::metadata(&log_file).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
            let _ = std::fs::remove_file(&log_file);
        }

        Ok(Self {
            program: install_dir.join(&preset.engine.path),
            args: preset.expand_args(&lists_dir, &hostlist_path),
            log_file: Some(log_file),
        })
    }

    fn spawn(&self) -> Result<Child, SupervisorError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).stdin(Stdio::null());
        if let Some(file) = self.open_log() {
            if let Ok(clone) = file.try_clone() {
                cmd.stdout(clone);
            }
            cmd.stderr(file);
        }
        // Engines look for WinDivert and their own data files next to the exe.
        if let Some(dir) = self.program.parent().filter(|d| !d.as_os_str().is_empty()) {
            cmd.current_dir(dir);
        }
        cmd.spawn().map_err(|source| SupervisorError::Spawn {
            path: self.program.clone(),
            source,
        })
    }

    fn open_log(&self) -> Option<File> {
        let path = self.log_file.as_ref()?;
        let _ = std::fs::create_dir_all(path.parent()?);
        OpenOptions::new().create(true).append(true).open(path).ok()
    }
}

pub struct Supervisor {
    pub command: EngineCommand,
    pub auto_restart: bool,
    pub policy: RestartPolicy,
    pub restart_delay: Duration,
    /// Receives status messages ("engine exited, restarting", ...).
    pub log: Box<dyn Fn(&str) + Send>,
}

impl Supervisor {
    pub fn new(command: EngineCommand, cfg: &Config) -> Self {
        Self {
            command,
            auto_restart: cfg.auto_restart,
            policy: RestartPolicy::new(
                cfg.max_restarts,
                Duration::from_secs(cfg.restart_window_secs),
            ),
            restart_delay: Duration::from_secs(1),
            log: Box::new(|m| eprintln!("detour: {m}")),
        }
    }

    /// Runs the engine until a stop is requested (a message on `stop`, or
    /// every sender dropped), restarting it per the policy. Returns `Ok`
    /// only when stopped on request.
    pub fn run(&mut self, stop: &Receiver<()>) -> Result<(), SupervisorError> {
        loop {
            let mut child = self.command.spawn()?;
            let status = loop {
                if let Some(status) = child.try_wait().map_err(SupervisorError::Wait)? {
                    break status;
                }
                if stop_requested(stop, POLL) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
            };

            if !self.auto_restart {
                return Err(SupervisorError::Exited(status));
            }
            if !self.policy.allow_restart(Instant::now()) {
                return Err(SupervisorError::GaveUp {
                    count: self.policy.recent_exits(),
                    window_secs: self.policy.window().as_secs(),
                    last: status,
                });
            }
            (self.log)(&format!("engine exited ({status}), restarting"));
            if stop_requested(stop, self.restart_delay) {
                return Ok(());
            }
        }
    }
}

fn stop_requested(stop: &Receiver<()>, wait: Duration) -> bool {
    !matches!(stop.recv_timeout(wait), Err(RecvTimeoutError::Timeout))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn supervisor(args: &[&str], program: &str, auto_restart: bool, max: u32) -> Supervisor {
        Supervisor {
            command: EngineCommand {
                program: program.into(),
                args: args.iter().map(|s| s.to_string()).collect(),
                log_file: None,
            },
            auto_restart,
            policy: RestartPolicy::new(max, Duration::from_secs(60)),
            restart_delay: Duration::ZERO,
            log: Box::new(|_| {}),
        }
    }

    #[test]
    fn empty_preset_is_rejected_up_front() {
        let dir = std::env::temp_dir().join(format!("detour-empty-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("presets")).unwrap();
        std::fs::create_dir_all(dir.join("lists")).unwrap();
        std::fs::write(dir.join("lists/custom.txt"), "# nothing\n").unwrap();
        std::fs::write(
            dir.join("presets/p.toml"),
            "name = \"P\"\nlists = [\"custom\"]\n[engine]\npath = \"e.exe\"\n",
        )
        .unwrap();
        let cfg = Config {
            preset: "p".into(),
            ..Config::default()
        };
        let err = EngineCommand::prepare(&cfg, &dir, &dir).unwrap_err();
        assert!(err.to_string().contains("no domains"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exit_without_auto_restart() {
        let (_tx, rx) = mpsc::channel();
        let err = supervisor(&["/C", "exit 3"], "cmd", false, 5)
            .run(&rx)
            .unwrap_err();
        assert!(matches!(err, SupervisorError::Exited(s) if s.code() == Some(3)));
    }

    #[test]
    fn gives_up_after_repeated_crashes() {
        let (_tx, rx) = mpsc::channel();
        let err = supervisor(&["/C", "exit 1"], "cmd", true, 2)
            .run(&rx)
            .unwrap_err();
        assert!(matches!(err, SupervisorError::GaveUp { count: 3, .. }));
    }

    #[test]
    fn stop_kills_running_engine() {
        let (tx, rx) = mpsc::channel();
        let mut sup = supervisor(&["-n", "30", "127.0.0.1"], "ping", true, 5);
        sup.restart_delay = Duration::from_secs(1);
        let handle = std::thread::spawn(move || sup.run(&rx));
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        tx.send(()).unwrap();
        handle.join().unwrap().unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn missing_engine_is_spawn_error() {
        let (_tx, rx) = mpsc::channel();
        let err = supervisor(&[], "C:\\nonexistent\\engine.exe", true, 5)
            .run(&rx)
            .unwrap_err();
        assert!(matches!(err, SupervisorError::Spawn { .. }));
    }
}
