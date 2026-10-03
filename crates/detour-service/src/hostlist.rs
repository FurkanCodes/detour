//! Merges the preset's domain lists into the single file the engine reads.

use detour_core::{ConfigError, DomainList, Preset};
use std::path::Path;

pub fn merge(preset: &Preset, lists_dir: &Path) -> Result<DomainList, ConfigError> {
    let mut all = DomainList::new();
    for name in &preset.lists {
        let path = lists_dir.join(format!("{name}.txt"));
        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        all.extend(&DomainList::parse(&text));
    }
    Ok(all)
}

pub fn write(list: &DomainList, path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, list.to_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use detour_core::config::EngineSpec;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("detour-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn preset(lists: &[&str]) -> Preset {
        Preset {
            name: "T".into(),
            description: String::new(),
            engine: EngineSpec {
                path: "e.exe".into(),
                args: vec![],
            },
            lists: lists.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn merges_and_writes() {
        let dir = temp_dir("merge");
        std::fs::write(dir.join("a.txt"), "x.com\ny.com\n").unwrap();
        std::fs::write(dir.join("b.txt"), "# c\nY.com\nz.com\n").unwrap();

        let list = merge(&preset(&["a", "b"]), &dir).unwrap();
        let out = dir.join("run").join("hostlist.txt");
        write(&list, &out).unwrap();

        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "x.com\ny.com\nz.com\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_list_is_an_error() {
        let dir = temp_dir("missing");
        let err = merge(&preset(&["nope"]), &dir).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
