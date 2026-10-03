//! Fonts: Plus Jakarta Sans (bundled, SIL Open Font License) for text,
//! Phosphor for icons, the system monospace font for logs.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

const REGULAR: &[u8] = include_bytes!("../../../assets/fonts/PlusJakartaSans-Regular.ttf");
const MEDIUM: &[u8] = include_bytes!("../../../assets/fonts/PlusJakartaSans-Medium.ttf");
const BOLD: &[u8] = include_bytes!("../../../assets/fonts/PlusJakartaSans-Bold.ttf");

#[cfg(target_os = "windows")]
const MONO_FONTS: &[&str] = &[
    "C:\\Windows\\Fonts\\CascadiaMono.ttf",
    "C:\\Windows\\Fonts\\consola.ttf",
];
#[cfg(target_os = "macos")]
const MONO_FONTS: &[&str] = &["/System/Library/Fonts/SFNSMono.ttf", "/System/Library/Fonts/Menlo.ttc"];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const MONO_FONTS: &[&str] = &[];

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    // Registers "phosphor" and appends it to the proportional fallbacks.
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

    for (name, bytes) in [("jakarta", REGULAR), ("jakarta-medium", MEDIUM), ("jakarta-bold", BOLD)] {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    // Every family falls back to the regular stack so icons and rare
    // characters still render inside bold or medium text.
    let mut proportional = fonts.families[&FontFamily::Proportional].clone();
    proportional.insert(0, "jakarta".to_owned());
    for (family, first) in [("medium", "jakarta-medium"), ("bold", "jakarta-bold")] {
        let mut stack = vec![first.to_owned()];
        stack.extend(proportional.iter().cloned());
        fonts.families.insert(FontFamily::Name(family.into()), stack);
    }
    fonts.families.insert(FontFamily::Proportional, proportional);

    if let Some(bytes) = MONO_FONTS.iter().find_map(|p| std::fs::read(p).ok()) {
        fonts
            .font_data
            .insert("system-mono".to_owned(), Arc::new(FontData::from_owned(bytes)));
        if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
            list.insert(0, "system-mono".to_owned());
        }
    }
    ctx.set_fonts(fonts);
}
