//! Notification-area icon with a small menu.

use crate::controller::Controller;
use crate::icon::shield_rgba;
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
    pub fn new(ctx: &egui::Context, ctl: Arc<Mutex<Controller>>) -> Option<Tray> {
        let menu = Menu::new();
        let items = [
            MenuItem::with_id("show", "Open Detour", true, None),
            MenuItem::with_id("on", "Turn on", true, None),
            MenuItem::with_id("off", "Turn off", true, None),
        ];
        let quit = MenuItem::with_id("quit", "Quit", true, None);
        menu.append(&items[0]).ok()?;
        menu.append(&PredefinedMenuItem::separator()).ok()?;
        menu.append(&items[1]).ok()?;
        menu.append(&items[2]).ok()?;
        menu.append(&PredefinedMenuItem::separator()).ok()?;
        menu.append(&quit).ok()?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Detour")
            .with_icon(tray_image(false))
            .build()
            .ok()?;

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
        Some(Tray {
            icon,
            shown_on: None,
        })
    }

    /// Keeps the icon and tooltip in step with the protection state.
    pub fn sync(&mut self, on: bool) {
        if self.shown_on != Some(on) {
            self.shown_on = Some(on);
            let _ = self.icon.set_icon(Some(tray_image(on)));
            let _ = self.icon.set_tooltip(Some(if on {
                "Detour: protected"
            } else {
                "Detour: off"
            }));
        }
    }
}

/// The app icon in colour when protecting, greyed out when off.
fn tray_image(on: bool) -> tray_icon::Icon {
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
