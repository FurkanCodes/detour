//! Everything the app needs, embedded in the executable.

use detour_core::{DomainList, Preset};
use std::path::Path;

pub struct BuiltinList {
    pub id: &'static str,
    pub title: &'static str,
    pub text: &'static str,
}

pub const LISTS: &[BuiltinList] = &[
    BuiltinList {
        id: "discord",
        title: "Discord",
        text: include_str!("../../../lists/discord.txt"),
    },
    BuiltinList {
        id: "roblox",
        title: "Roblox",
        text: include_str!("../../../lists/roblox.txt"),
    },
];

const PRESETS: &[(&str, &str)] = &[
    (
        "turk-telekom",
        include_str!("../../../presets/turk-telekom.toml"),
    ),
    (
        "superonline",
        include_str!("../../../presets/superonline.toml"),
    ),
    ("generic", include_str!("../../../presets/generic.toml")),
    (
        "aggressive",
        include_str!("../../../presets/aggressive.toml"),
    ),
];

#[cfg(windows)]
pub const WINDIVERT_DLL: &[u8] = include_bytes!("../../../vendor/windivert/WinDivert.dll");
#[cfg(windows)]
pub const WINDIVERT_SYS: &[u8] = include_bytes!("../../../vendor/windivert/WinDivert64.sys");

pub struct PresetEntry {
    pub id: &'static str,
    pub preset: Preset,
}

pub fn presets() -> Vec<PresetEntry> {
    PRESETS
        .iter()
        .map(|(id, text)| PresetEntry {
            id,
            preset: Preset::from_toml(text, Path::new(id))
                .unwrap_or_else(|e| panic!("embedded preset {id} is invalid: {e}")),
        })
        .collect()
}

pub fn builtin_list(id: &str) -> Option<DomainList> {
    LISTS
        .iter()
        .find(|l| l.id == id)
        .map(|l| DomainList::parse(l.text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use detour_engine::Options;

    #[test]
    fn embedded_presets_parse_and_have_engine_args() {
        let all = presets();
        assert!(all.len() >= 4);
        for p in &all {
            let args = p.preset.expand_args(Path::new(""), Path::new("hostlist"));
            let opts = Options::parse(&args).unwrap().unwrap();
            assert!(opts.strategy.fake_ttl.is_some() || !opts.strategy.split.is_empty());
        }
    }

    #[test]
    fn builtin_lists_are_not_empty() {
        for l in LISTS {
            assert!(!builtin_list(l.id).unwrap().is_empty(), "{}", l.id);
        }
        assert!(builtin_list("nope").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn driver_files_are_embedded() {
        assert!(WINDIVERT_DLL.starts_with(b"MZ"));
        assert!(WINDIVERT_SYS.starts_with(b"MZ"));
    }
}
