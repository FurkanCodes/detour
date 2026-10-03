//! App configuration (`detour.toml`) and ISP presets (`presets/*.toml`).
//!
//! Detour does not process packets itself: a preset says which engine
//! executable to run and with which arguments. The app supervises it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid TOML in {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// Top-level app settings, stored in `%ProgramData%\Detour\detour.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// File stem of the active preset in the presets directory.
    pub preset: String,
    /// Start the engine when the service starts.
    pub enabled: bool,
    pub presets_dir: PathBuf,
    pub lists_dir: PathBuf,
    /// Restart the engine if it exits unexpectedly.
    pub auto_restart: bool,
    /// Give up after this many crashes within `restart_window_secs`.
    pub max_restarts: u32,
    pub restart_window_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            preset: "turk-telekom".into(),
            enabled: true,
            presets_dir: "presets".into(),
            lists_dir: "lists".into(),
            auto_restart: true,
            max_restarts: 5,
            restart_window_secs: 60,
        }
    }
}

impl Config {
    pub fn from_toml(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let cfg: Config = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::from_toml(&read(path)?, path)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("Config always serializes")
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if !is_safe_stem(&self.preset) {
            return Err(ConfigError::Invalid(format!(
                "preset name {:?} must be letters, digits, '-' or '_'",
                self.preset
            )));
        }
        if self.max_restarts == 0 {
            return Err(ConfigError::Invalid(
                "max_restarts must be at least 1".into(),
            ));
        }
        if self.restart_window_secs == 0 {
            return Err(ConfigError::Invalid(
                "restart_window_secs must be at least 1".into(),
            ));
        }
        Ok(())
    }

    /// Resolves a possibly relative directory against `base` (the folder
    /// containing the config file).
    pub fn resolve(&self, base: &Path, dir: &Path) -> PathBuf {
        if dir.is_absolute() {
            dir.to_owned()
        } else {
            base.join(dir)
        }
    }

    pub fn preset_path(&self, base: &Path) -> PathBuf {
        self.resolve(base, &self.presets_dir)
            .join(format!("{}.toml", self.preset))
    }
}

/// One ISP preset: which engine to run, how, and which domain lists it uses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    /// Display name, e.g. "Türk Telekom".
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub engine: EngineSpec,
    /// Domain list file stems from the lists directory.
    #[serde(default)]
    pub lists: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EngineSpec {
    /// Executable path, relative to the install directory.
    pub path: PathBuf,
    /// Arguments passed verbatim. `{lists_dir}` and `{hostlist}` are
    /// substituted by the supervisor.
    #[serde(default)]
    pub args: Vec<String>,
}

impl Preset {
    pub fn from_toml(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let p: Preset = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })?;
        p.validate()?;
        Ok(p)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::from_toml(&read(path)?, path)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.name.trim().is_empty() {
            return Err(ConfigError::Invalid("preset name is empty".into()));
        }
        if self.engine.path.as_os_str().is_empty() {
            return Err(ConfigError::Invalid("engine.path is empty".into()));
        }
        if let Some(bad) = self.lists.iter().find(|l| !is_safe_stem(l)) {
            return Err(ConfigError::Invalid(format!("bad list name {bad:?}")));
        }
        Ok(())
    }

    /// Engine arguments with placeholders filled in.
    pub fn expand_args(&self, lists_dir: &Path, hostlist: &Path) -> Vec<String> {
        let lists_dir = lists_dir.display().to_string();
        let hostlist = hostlist.display().to_string();
        self.engine
            .args
            .iter()
            .map(|a| {
                a.replace("{lists_dir}", &lists_dir)
                    .replace("{hostlist}", &hostlist)
            })
            .collect()
    }
}

fn read(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_owned(),
        source,
    })
}

/// File stems we accept for presets and lists: no path separators or dots,
/// so a config value can never point outside its directory.
fn is_safe_stem(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "test.toml";

    #[test]
    fn default_config_round_trips() {
        let cfg = Config::default();
        let back = Config::from_toml(&cfg.to_toml(), Path::new(P)).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn partial_config_uses_defaults() {
        let cfg = Config::from_toml("preset = \"superonline\"\n", Path::new(P)).unwrap();
        assert_eq!(cfg.preset, "superonline");
        assert!(cfg.auto_restart);
    }

    #[test]
    fn rejects_path_traversal_in_preset() {
        let err = Config::from_toml("preset = \"../evil\"\n", Path::new(P)).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = Config::from_toml("presett = \"x\"\n", Path::new(P)).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn preset_parses_and_expands() {
        let text = r#"
            name = "Example"
            lists = ["discord", "roblox"]
            [engine]
            path = "engine/engine.exe"
            args = ["--hostlist={hostlist}", "--dir", "{lists_dir}"]
        "#;
        let p = Preset::from_toml(text, Path::new(P)).unwrap();
        let args = p.expand_args(Path::new("C:/k/lists"), Path::new("C:/k/run/hosts.txt"));
        assert_eq!(
            args,
            ["--hostlist=C:/k/run/hosts.txt", "--dir", "C:/k/lists"]
        );
    }

    #[test]
    fn preset_rejects_bad_list_name() {
        let text = r#"
            name = "X"
            lists = ["../../windows"]
            [engine]
            path = "e.exe"
        "#;
        assert!(Preset::from_toml(text, Path::new(P)).is_err());
    }
}
