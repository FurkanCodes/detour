//! User settings, stored in `%APPDATA%\Detour\settings.toml`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Profile,
    Turbo,
    Balanced,
    Strong,
}

impl Mode {
    pub const ALL: [Self; 4] = [Self::Profile, Self::Turbo, Self::Balanced, Self::Strong];
    pub fn title(self) -> &'static str {
        match self {
            Self::Profile => "Use provider profile",
            Self::Turbo => "Turbo",
            Self::Balanced => "Balanced",
            Self::Strong => "Strong",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Profile => "Exactly the selected provider's strategy.",
            Self::Turbo => "Each byte of the site name in its own packet. Fastest; not enough for strict filters such as Türk Telekom's.",
            Self::Balanced => "The TLS handshake in two-byte packets.",
            Self::Strong => "The TLS handshake one byte per packet, as BypaxDPI does it for strict filters.",
        }
    }
}

/// How traffic gets past the filter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Method {
    /// Direct connections with reshaped handshakes and clean DNS.
    #[default]
    Detour,
    /// Discord and Roblox (and optionally browsers) through Cloudflare WARP,
    /// run by WireSock. Windows only.
    Warp,
}

impl Method {
    pub const ALL: [Self; 2] = [Self::Detour, Self::Warp];
    pub fn title(self) -> &'static str {
        match self {
            Self::Detour => "Detour",
            Self::Warp => "WARP tunnel",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Detour => {
                "Direct connections: Detour reshapes the first packets and fixes DNS so the \
                 provider's filter misses the site. Not a VPN; your IP address stays the same."
            }
            Self::Warp => {
                "Discord and Roblox, voice included, go through a free Cloudflare WARP tunnel; \
                 everything else stays on the Detour method. The first connect downloads and \
                 installs WireSock Secure Connect (free for personal use) and creates a WARP \
                 account. Tunnelled apps show a Cloudflare IP address."
            }
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Resolver {
    #[default]
    Profile,
    Cloudflare,
    Google,
    Quad9,
    System,
}

impl Resolver {
    pub const ALL: [Self; 5] = [
        Self::Profile,
        Self::Cloudflare,
        Self::Google,
        Self::Quad9,
        Self::System,
    ];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// File stem of the selected preset.
    pub preset: String,
    pub custom_domains: Vec<String>,
    /// Built-in lists the user switched off.
    pub disabled_lists: Vec<String>,
    /// Turn protection on as soon as Detour opens.
    pub auto_enable: bool,
    /// Closing the window keeps Detour running in the tray.
    pub close_to_tray: bool,
    pub mode: Mode,
    pub resolver: Resolver,
    /// Domain-level logs are optional to keep the packet path quiet.
    pub detailed_logs: bool,
    pub all_sites: bool,
    pub quic_fallback: bool,
    pub encrypted_dns: bool,
    pub method: Method,
    /// WARP method: browsers go through the tunnel too.
    pub warp_browsers: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            preset: "turk-telekom".into(),
            custom_domains: Vec::new(),
            disabled_lists: Vec::new(),
            auto_enable: false,
            close_to_tray: true,
            mode: Mode::default(),
            resolver: Resolver::default(),
            detailed_logs: false,
            all_sites: true,
            quic_fallback: true,
            encrypted_dns: true,
            method: Method::default(),
            warp_browsers: false,
        }
    }
}

impl Settings {
    /// `%APPDATA%\Detour\settings.toml` on Windows,
    /// `~/Library/Application Support/Detour/settings.toml` on macOS.
    /// Settings from the app's earlier name (Kalkan) are carried over once.
    pub fn default_path() -> Option<PathBuf> {
        let base = if cfg!(windows) {
            PathBuf::from(std::env::var_os("APPDATA")?)
        } else {
            PathBuf::from(std::env::var_os("HOME")?).join("Library").join("Application Support")
        };
        let path = base.join("Detour").join("settings.toml");
        migrate(&base.join("Kalkan").join("settings.toml"), &path);
        Some(path)
    }

}

/// Copies `old` to `new` if only `old` exists. Failures are ignored: the
/// worst case is starting with default settings.
fn migrate(old: &Path, new: &Path) {
    if new.exists() || !old.is_file() {
        return;
    }
    if let Some(dir) = new.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::copy(old, new);
}

impl Settings {
    /// Missing or unreadable files give the defaults: a broken settings
    /// file must never stop the app from opening.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    pub fn list_enabled(&self, name: &str) -> bool {
        !self.disabled_lists.iter().any(|l| l == name)
    }

    pub fn set_list_enabled(&mut self, name: &str, on: bool) {
        self.disabled_lists.retain(|l| l != name);
        if !on {
            self.disabled_lists.push(name.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("detour-app-{name}-{}", std::process::id()))
    }

    #[test]
    fn round_trips() {
        let path = temp("rt").join("settings.toml");
        let s = Settings {
            preset: "generic".into(),
            custom_domains: vec!["example.com".into()],
            disabled_lists: vec!["roblox".into()],
            auto_enable: false,
            close_to_tray: false,
            mode: Mode::Balanced,
            resolver: Resolver::Cloudflare,
            detailed_logs: true,
            all_sites: false,
            quic_fallback: false,
            encrypted_dns: false,
            method: Method::Warp,
            warp_browsers: true,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn broken_or_missing_file_gives_defaults() {
        assert_eq!(
            Settings::load(&temp("missing").join("x.toml")),
            Settings::default()
        );
        let path = temp("bad");
        std::fs::create_dir_all(&path).unwrap();
        let file = path.join("settings.toml");
        std::fs::write(&file, "preset = [not toml").unwrap();
        assert_eq!(Settings::load(&file), Settings::default());
        std::fs::write(&file, "preset = \"x\"\n").unwrap();
        assert_eq!(Settings::load(&file).preset, "x");
        assert!(Settings::load(&file).all_sites);
        assert_eq!(Settings::load(&file).method, Method::Detour, "older files keep the direct method");
        std::fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn migrates_old_settings_once() {
        let dir = temp("migrate");
        let (old, new) = (dir.join("Kalkan/settings.toml"), dir.join("Detour/settings.toml"));
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "preset = \"generic\"\n").unwrap();
        migrate(&old, &new);
        assert_eq!(Settings::load(&new).preset, "generic");
        std::fs::write(&old, "preset = \"aggressive\"\n").unwrap();
        migrate(&old, &new);
        assert_eq!(Settings::load(&new).preset, "generic", "an existing file is never overwritten");
        migrate(&dir.join("missing.toml"), &dir.join("other/settings.toml"));
        assert!(!dir.join("other/settings.toml").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn toggling_lists() {
        let mut s = Settings::default();
        assert!(s.list_enabled("discord"));
        s.set_list_enabled("discord", false);
        s.set_list_enabled("discord", false);
        assert!(!s.list_enabled("discord"));
        assert_eq!(s.disabled_lists.len(), 1);
        s.set_list_enabled("discord", true);
        assert!(s.list_enabled("discord"));
    }
}
