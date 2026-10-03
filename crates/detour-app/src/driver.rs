//! Unpacks the embedded WinDivert files so the DLL can be loaded.

use crate::assets::{WINDIVERT_DLL, WINDIVERT_SYS};
use std::path::{Path, PathBuf};

pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Detour").join("driver"))
}

/// Writes the DLL and driver into `dir` (skipping files that already have
/// the right content) and returns the DLL path.
pub fn extract(dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for (name, bytes) in [
        ("WinDivert.dll", WINDIVERT_DLL),
        ("WinDivert64.sys", WINDIVERT_SYS),
    ] {
        let path = dir.join(name);
        if std::fs::read(&path).is_ok_and(|existing| existing == bytes) {
            continue;
        }
        std::fs::write(&path, bytes).map_err(|e| {
            std::io::Error::new(e.kind(), format!("cannot write {}: {e}", path.display()))
        })?;
    }
    Ok(dir.join("WinDivert.dll"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_once_and_repairs_damage() {
        let dir = std::env::temp_dir().join(format!("detour-driver-{}", std::process::id()));
        let dll = extract(&dir).unwrap();
        assert_eq!(std::fs::read(&dll).unwrap(), WINDIVERT_DLL);
        assert_eq!(
            std::fs::read(dir.join("WinDivert64.sys")).unwrap(),
            WINDIVERT_SYS
        );

        std::fs::write(&dll, b"corrupt").unwrap();
        extract(&dir).unwrap();
        assert_eq!(std::fs::read(&dll).unwrap(), WINDIVERT_DLL);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
