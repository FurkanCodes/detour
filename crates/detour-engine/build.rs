use std::path::{Path, PathBuf};

// Puts the WinDivert DLL and driver next to the built executables so that
// `cargo run` works without manual copying.
fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest.join("../../vendor/windivert");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    // OUT_DIR is <target>/<profile>/build/<crate>-<hash>/out
    let Some(profile_dir) = out.ancestors().nth(3) else {
        return;
    };
    for file in ["WinDivert.dll", "WinDivert64.sys"] {
        copy(&vendor.join(file), &profile_dir.join(file));
    }
    println!("cargo:rerun-if-changed={}", vendor.display());
}

fn copy(from: &Path, to: &Path) {
    if let Err(e) = std::fs::copy(from, to) {
        println!("cargo:warning=cannot copy {}: {e}", from.display());
    }
}
