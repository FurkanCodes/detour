use std::path::PathBuf;

include!("src/icon_art.rs");

fn main() {
    // The exe icon and the "run as administrator" manifest are Windows-only.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());

    let ico = out.join("detour.ico");
    std::fs::write(&ico, build_ico(&[16, 32, 48, 256])).unwrap();

    let slash = |p: PathBuf| p.display().to_string().replace('\\', "/");
    let manifest = slash(manifest_dir.join("app.manifest"));
    let rc = out.join("app.rc");
    std::fs::write(
        &rc,
        format!("1 24 \"{manifest}\"\n1 ICON \"{}\"\n", slash(ico)),
    )
    .unwrap();

    embed_resource::compile_for(&rc, ["detour-app"], embed_resource::NONE)
        .manifest_required()
        .unwrap();
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=src/icon_art.rs");
}

/// ICO container with uncompressed 32-bit DIB images.
fn build_ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| dib(s)).collect();
    let mut out = vec![0, 0, 1, 0];
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&s, img) in sizes.iter().zip(&images) {
        out.push(if s >= 256 { 0 } else { s as u8 });
        out.push(if s >= 256 { 0 } else { s as u8 });
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}

fn dib(size: u32) -> Vec<u8> {
    let rgba = shield_rgba(size);
    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // XOR + AND mask heights
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for row in (0..size).rev() {
        for col in 0..size {
            let i = ((row * size + col) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    let mask_row = size.div_ceil(32) * 4;
    out.extend(std::iter::repeat_n(0u8, (mask_row * size) as usize));
    out
}
