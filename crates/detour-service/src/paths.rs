use std::path::PathBuf;

pub const SERVICE_NAME: &str = "Detour";

/// `%ProgramData%\Detour`: config, presets, lists and runtime files.
pub fn default_data_dir() -> Option<PathBuf> {
    std::env::var_os("ProgramData").map(|p| PathBuf::from(p).join("Detour"))
}

pub fn config_file(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("detour.toml")
}
