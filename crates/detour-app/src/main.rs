#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod backend;
mod brand;
mod controller;
mod diag;
#[cfg(windows)]
mod driver;
mod fonts;
#[path = "icon_art.rs"]
mod icon;
mod settings;
mod sysproxy;
mod tray;
mod ui;
#[cfg(windows)]
mod warp;
#[cfg(not(windows))]
#[path = "unix.rs"]
mod sys;
#[cfg(windows)]
#[path = "win.rs"]
mod sys;

use controller::Controller;
use eframe::egui;
use settings::Settings;
use std::sync::{Arc, Mutex};

/// Developer harness (`detour-app --headless=FILE`): runs the real
/// controller without a window and obeys what is written to FILE. A change
/// to its content ("on", "off" or "quit", optionally followed by a counter so
/// the same command can be repeated) is acted on within 200 ms; the log goes
/// to stdout.
fn run_headless(ctl: &Arc<Mutex<Controller>>, control: &str) {
    let (mut last, mut printed) = (String::new(), 0);
    loop {
        let command = std::fs::read_to_string(control).unwrap_or_default();
        let mut c = ctl.lock().unwrap();
        if command != last {
            match command.split_whitespace().next() {
                Some("on") => c.enable(),
                Some("off") => c.disable(),
                Some("quit") => break,
                _ => {}
            }
            last = command;
        }
        c.poll();
        let lines = c.log_lines();
        for line in lines.iter().skip(printed) {
            println!("{line}");
        }
        printed = lines.len();
        drop(c);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    ctl.lock().unwrap().disable();
}

fn main() {
    // Packaging helper: `detour-app --export-icon=icon.png` (used to build the macOS .icns).
    if let Some(path) = std::env::args().find_map(|a| a.strip_prefix("--export-icon=").map(str::to_owned)) {
        let (size, rgba) = (1024, icon::shield_rgba(1024));
        match image::save_buffer(&path, &rgba, size, size, image::ColorType::Rgba8) {
            Ok(()) => return,
            Err(e) => {
                eprintln!("cannot write {path}: {e}");
                std::process::exit(1);
            }
        }
    }
    let capture = std::env::args().find_map(|a| {
        a.strip_prefix("--screenshot=")
            .map(std::path::PathBuf::from)
    });
    let screen = std::env::args()
        .find_map(|a| a.strip_prefix("--screen=").map(str::to_owned))
        .unwrap_or_default();
    let headless = std::env::args().find_map(|a| a.strip_prefix("--headless=").map(str::to_owned));
    if capture.is_none() && headless.is_none() && !sys::claim_single_instance() {
        sys::signal_existing_instance();
        return;
    }
    // A crash can leave the system proxy pointing at a dead port; undo that first.
    if capture.is_none() {
        if let Some(dir) = sys::runtime_dir() {
            backend::recover(&dir);
        }
    }
    let start_hidden = std::env::args().any(|a| a == "--tray");
    let exe = std::env::current_exe().unwrap_or_default();

    let settings_path = if capture.is_some() {
        None
    } else {
        Settings::default_path()
    };
    let settings = settings_path
        .as_deref()
        .map(Settings::load)
        .unwrap_or_default();
    let auto_enable = settings.auto_enable;
    let ctl = Arc::new(Mutex::new(Controller::new(
        settings,
        settings_path,
        sys::runtime_dir(),
    )));
    if let Some(control) = headless {
        run_headless(&ctl, &control);
        return;
    }
    if auto_enable {
        ctl.lock().unwrap().enable();
    }

    let icon = egui::IconData {
        rgba: icon::shield_rgba(64),
        width: 64,
        height: 64,
    };
    let viewport = egui::ViewportBuilder::default()
        .with_title("Detour")
        .with_inner_size([1120.0, 740.0])
        .with_min_inner_size([940.0, 640.0])
        .with_icon(icon)
        .with_visible(!start_hidden);
    // macOS keeps its native title bar (traffic lights, resizing, rounded
    // corners) but lets Detour draw underneath it; Windows gets a fully
    // custom borderless window.
    let viewport = if cfg!(target_os = "macos") {
        viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
    } else {
        viewport.with_decorations(false)
    };
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let app_ctl = ctl.clone();
    let result = eframe::run_native(
        "Detour",
        options,
        Box::new(move |cc| {
            ui::apply_theme(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            if capture.is_none() {
                sys::listen_for_show_requests(move || ctx.request_repaint());
            }
            let tray = if capture.is_some() {
                None
            } else {
                match tray::Tray::new(&cc.egui_ctx, app_ctl.clone()) {
                    Ok(tray) => Some(tray),
                    Err(e) => {
                        app_ctl.lock().unwrap().log(format!("No tray icon: {e}"));
                        None
                    }
                }
            };
            if capture.is_none() {
                // Minimizing to the menu bar only makes sense with an icon there.
                sys::setup_window(cc, tray.is_some());
            }
            if start_hidden {
                if tray.is_some() {
                    sys::show_in_dock(false);
                } else {
                    cc.egui_ctx
                        .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                }
            }
            let mut app = ui::DetourApp::new(&cc.egui_ctx, app_ctl, exe, tray);
            if let Some(path) = capture {
                app.capture(path, &screen);
            }
            Ok(Box::new(app))
        }),
    );

    // The tray's event handlers keep the controller alive, so stop the
    // engine explicitly instead of relying on Drop.
    if let Ok(mut c) = ctl.lock() {
        c.disable();
    }
    if let Err(e) = result {
        sys::error_box(&format!("Detour could not open its window.\n\n{e}"));
    }
}
