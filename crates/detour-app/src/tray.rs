//! Notification-area icon (menu-bar icon on macOS) with a small menu.

use crate::controller::Controller;
use crate::icon::{shield_rgba, shield_template_rgba};
use crate::sys;
use eframe::egui;
use std::sync::{Arc, Mutex};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub struct Tray {
    icon: TrayIcon,
    shown_on: Option<bool>,
}

impl Tray {
    pub fn new(ctx: &egui::Context, ctl: Arc<Mutex<Controller>>) -> Result<Tray, String> {
        let menu = Menu::new();
        let items = [
            MenuItem::with_id("show", "Open Detour", true, None),
            MenuItem::with_id("on", "Turn on", true, None),
            MenuItem::with_id("off", "Turn off", true, None),
        ];
        let quit = MenuItem::with_id("quit", "Quit", true, None);
        let appended = (|| -> tray_icon::menu::Result<()> {
            menu.append(&items[0])?;
            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&items[1])?;
            menu.append(&items[2])?;
            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&quit)
        })();
        appended.map_err(|e| format!("cannot build the menu: {e}"))?;

        // On macOS a click on a menu-bar icon opens its menu; elsewhere a left
        // click opens the window and the menu is on the right button.
        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(cfg!(target_os = "macos"))
            .with_tooltip("Detour");
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(tray_image(false));
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(tray_image(false));
        let icon = builder.build().map_err(|e| format!("cannot create the icon: {e}"))?;

        let c = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            match e.id.as_ref() {
                "show" => sys::request_show(),
                "on" | "off" | "quit" => {
                    let mut ctl = ctl.lock().unwrap_or_else(|p| p.into_inner());
                    match e.id.as_ref() {
                        "on" => ctl.enable(),
                        "off" => ctl.disable(),
                        _ => {
                            ctl.disable();
                            std::process::exit(0);
                        }
                    }
                }
                _ => {}
            }
            c.request_repaint();
        }));
        let c = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            // macOS shows the menu on a click instead; "Open Detour" is in it.
            if cfg!(target_os = "macos") {
                return;
            }
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                sys::request_show();
                c.request_repaint();
            }
        }));
        Ok(Tray {
            icon,
            shown_on: None,
        })
    }

    /// Keeps the icon and tooltip in step with the protection state.
    pub fn sync(&mut self, on: bool) {
        if self.shown_on != Some(on) {
            self.shown_on = Some(on);
            #[cfg(target_os = "macos")]
            let _ = self.icon.set_icon_templated(Some(tray_image(on)));
            #[cfg(not(target_os = "macos"))]
            let _ = self.icon.set_icon(Some(tray_image(on)));
            let _ = self.icon.set_tooltip(Some(if on {
                "Detour: protected"
            } else {
                "Detour: off"
            }));
        }
    }
}

/// The app icon in colour when protecting, greyed out when off. On macOS a
/// monochrome template instead (solid when on, faded when off): a grey tuned
/// for a light taskbar would all but vanish on a dark menu bar.
fn tray_image(on: bool) -> tray_icon::Icon {
    if cfg!(target_os = "macos") {
        // 22 pt tall menu bar, drawn at 2x.
        return tray_icon::Icon::from_rgba(shield_template_rgba(44, on), 44, 44)
            .expect("44x44 RGBA is valid");
    }
    let mut rgba = shield_rgba(32);
    if !on {
        for px in rgba.as_chunks_mut::<4>().0 {
            let grey = (px[0] as u16 * 3 + px[1] as u16 * 6 + px[2] as u16) / 10;
            let grey = (grey as f32 * 0.55 + 40.0) as u8;
            px[..3].fill(grey);
        }
    }
    tray_icon::Icon::from_rgba(rgba, 32, 32).expect("32x32 RGBA is valid")
}
