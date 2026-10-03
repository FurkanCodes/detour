//! The data directory: config, presets and lists, plus editing helpers.

use detour_core::{Config, DomainList, Preset};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub struct Ctx {
    pub data_dir: PathBuf,
    /// Folder holding the executables and the shipped presets/lists.
    pub install_dir: PathBuf,
}

impl Ctx {
    pub fn config_path(&self) -> PathBuf {
        detour_service::paths::config_file(&self.data_dir)
    }

    pub fn config(&self) -> Result<Config> {
        Ok(Config::load(&self.config_path())?)
    }

    pub fn presets_dir(&self, cfg: &Config) -> PathBuf {
        cfg.resolve(&self.data_dir, &cfg.presets_dir)
    }

    pub fn lists_dir(&self, cfg: &Config) -> PathBuf {
        cfg.resolve(&self.data_dir, &cfg.lists_dir)
    }
}

/// Creates the data directory, a default config, and copies the shipped
/// presets and lists that are not there yet (user edits are never
/// overwritten). Returns what was created.
pub fn ensure_data_dir(ctx: &Ctx) -> Result<Vec<PathBuf>> {
    let mut created = Vec::new();
    fs::create_dir_all(&ctx.data_dir)?;

    let config_path = ctx.config_path();
    if !config_path.exists() {
        fs::write(&config_path, Config::default().to_toml())?;
        created.push(config_path);
    }
    let cfg = ctx.config()?;
    for (sub, dst_dir, ext) in [
        ("presets", ctx.presets_dir(&cfg), "toml"),
        ("lists", ctx.lists_dir(&cfg), "txt"),
    ] {
        fs::create_dir_all(&dst_dir)?;
        let Ok(entries) = fs::read_dir(ctx.install_dir.join(sub)) else {
            continue;
        };
        for entry in entries.flatten() {
            let src = entry.path();
            if src.extension().is_some_and(|e| e == ext) {
                let dst = dst_dir.join(entry.file_name());
                if !dst.exists() {
                    fs::copy(&src, &dst)?;
                    created.push(dst);
                }
            }
        }
    }
    Ok(created)
}

pub struct PresetInfo {
    pub stem: String,
    pub preset: std::result::Result<Preset, String>,
}

pub fn presets(ctx: &Ctx) -> Result<Vec<PresetInfo>> {
    let dir = ctx.presets_dir(&ctx.config()?);
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "toml") {
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            let preset = Preset::load(&path).map_err(|e| e.to_string());
            out.push(PresetInfo { stem, preset });
        }
    }
    out.sort_by(|a, b| a.stem.cmp(&b.stem));
    Ok(out)
}

pub fn use_preset(ctx: &Ctx, name: &str) -> Result<Preset> {
    let mut cfg = ctx.config()?;
    cfg.preset = name.to_owned();
    cfg.validate()?;
    let preset = Preset::load(&cfg.preset_path(&ctx.data_dir))?;
    for list in &preset.lists {
        let path = ctx.lists_dir(&cfg).join(format!("{list}.txt"));
        if !path.exists() {
            return Err(format!(
                "preset {name:?} needs list {list:?}, but {} is missing",
                path.display()
            )
            .into());
        }
    }
    fs::write(ctx.config_path(), cfg.to_toml())?;
    Ok(preset)
}

fn list_file(ctx: &Ctx, name: &str) -> Result<PathBuf> {
    let cfg = ctx.config()?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(format!("bad list name {name:?}").into());
    }
    Ok(ctx.lists_dir(&cfg).join(format!("{name}.txt")))
}

pub fn read_list(ctx: &Ctx, name: &str) -> Result<DomainList> {
    let path = list_file(ctx, name)?;
    let text =
        fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(DomainList::parse(&text))
}

/// Appends `domain` to the list file. Returns the normalized domain and
/// whether it was new.
pub fn add_domain(ctx: &Ctx, list: &str, domain: &str) -> Result<(String, bool)> {
    let target = normalized(domain)?;
    let path = list_file(ctx, list)?;
    let text = fs::read_to_string(&path).unwrap_or_default();
    if lines_without(&text, &target).1 {
        return Ok((target, false));
    }
    let mut text = text;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&target);
    text.push('\n');
    fs::write(&path, text)?;
    Ok((target, true))
}

/// Removes every line naming `domain` (comments and other lines stay).
pub fn remove_domain(ctx: &Ctx, list: &str, domain: &str) -> Result<(String, bool)> {
    let target = normalized(domain)?;
    let path = list_file(ctx, list)?;
    let text =
        fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (kept, found) = lines_without(&text, &target);
    if found {
        fs::write(&path, kept)?;
    }
    Ok((target, found))
}

fn normalized(domain: &str) -> Result<String> {
    let mut l = DomainList::new();
    if !l.insert(domain) {
        return Err(format!("{domain:?} is not a valid domain name").into());
    }
    Ok(l.sorted()[0].to_owned())
}

/// Returns `text` minus lines equal to `target`, and whether any was found.
fn lines_without(text: &str, target: &str) -> (String, bool) {
    let mut found = false;
    let mut kept = String::new();
    for line in text.lines() {
        let entry = DomainList::parse(line);
        if entry.sorted() == [target] {
            found = true;
        } else {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    (kept, found)
}

pub fn file_exists(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(name: &str) -> Ctx {
        let dir = std::env::temp_dir().join(format!("detour-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let install = dir.join("install");
        fs::create_dir_all(install.join("presets")).unwrap();
        fs::create_dir_all(install.join("lists")).unwrap();
        fs::write(
            install.join("presets/p1.toml"),
            "name = \"P1\"\nlists = [\"a\", \"custom\"]\n[engine]\npath = \"e.exe\"\n",
        )
        .unwrap();
        fs::write(install.join("lists/a.txt"), "# header\nx.com\n").unwrap();
        fs::write(install.join("lists/custom.txt"), "# mine\n").unwrap();
        Ctx {
            data_dir: dir.join("data"),
            install_dir: install,
        }
    }

    fn cleanup(c: &Ctx) {
        let _ = fs::remove_dir_all(c.data_dir.parent().unwrap());
    }

    #[test]
    fn bootstraps_without_overwriting() {
        let c = ctx("boot");
        assert_eq!(ensure_data_dir(&c).unwrap().len(), 4);
        fs::write(c.data_dir.join("lists/a.txt"), "mine.com\n").unwrap();
        assert!(ensure_data_dir(&c).unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(c.data_dir.join("lists/a.txt")).unwrap(),
            "mine.com\n"
        );
        cleanup(&c);
    }

    #[test]
    fn switches_preset_and_checks_lists() {
        let c = ctx("preset");
        ensure_data_dir(&c).unwrap();
        assert_eq!(use_preset(&c, "p1").unwrap().name, "P1");
        assert_eq!(c.config().unwrap().preset, "p1");
        assert!(use_preset(&c, "nope").is_err());
        assert!(use_preset(&c, "../x").is_err());
        assert_eq!(c.config().unwrap().preset, "p1");
        fs::remove_file(c.data_dir.join("lists/custom.txt")).unwrap();
        assert!(use_preset(&c, "p1").is_err());
        cleanup(&c);
    }

    #[test]
    fn adds_and_removes_domains_keeping_comments() {
        let c = ctx("lists");
        ensure_data_dir(&c).unwrap();

        assert_eq!(
            add_domain(&c, "custom", "*.Example.COM").unwrap(),
            ("example.com".into(), true)
        );
        assert_eq!(
            add_domain(&c, "custom", "example.com").unwrap(),
            ("example.com".into(), false)
        );
        assert!(add_domain(&c, "custom", "not a domain").is_err());
        assert!(add_domain(&c, "../evil", "a.com").is_err());
        assert!(read_list(&c, "custom").unwrap().matches("www.example.com"));

        let text = fs::read_to_string(c.data_dir.join("lists/custom.txt")).unwrap();
        assert_eq!(text, "# mine\nexample.com\n");

        assert!(remove_domain(&c, "custom", "example.com").unwrap().1);
        assert!(!remove_domain(&c, "custom", "example.com").unwrap().1);
        assert_eq!(
            fs::read_to_string(c.data_dir.join("lists/custom.txt")).unwrap(),
            "# mine\n"
        );
        cleanup(&c);
    }
}
