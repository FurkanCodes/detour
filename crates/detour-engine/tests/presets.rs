//! Every shipped preset must load, reference existing lists, and pass
//! arguments the engine accepts.

use detour_core::{DomainList, Preset};
use detour_engine::Options;
use std::path::Path;

#[test]
fn shipped_presets_are_valid() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut count = 0;
    for entry in std::fs::read_dir(root.join("presets")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let preset = Preset::load(&path).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(preset.engine.path.to_str(), Some("detour-engine.exe"));

        let mut hosts = DomainList::new();
        for list in &preset.lists {
            let text = std::fs::read_to_string(root.join("lists").join(format!("{list}.txt")))
                .unwrap_or_else(|e| panic!("{}: list {list}: {e}", path.display()));
            hosts.extend(&DomainList::parse(&text));
        }
        assert!(!hosts.is_empty(), "{} has no domains", path.display());

        let args = preset.expand_args(Path::new("lists"), Path::new("run/hostlist.txt"));
        let opts = Options::parse(&args)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .expect("not --help");
        assert!(
            opts.hostlist.is_some(),
            "{} must pass --hostlist",
            path.display()
        );
        count += 1;
    }
    assert!(count >= 4, "expected the shipped presets, found {count}");
}
