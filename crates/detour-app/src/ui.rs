//! The window. The Connect page follows a classic VPN layout: provider
//! picker on top, a big power button with the session timer in the middle,
//! live measurements on the right. Sites, Activity and Settings sit behind a
//! floating navigation bar.

use crate::controller::{Controller, Status};
use crate::diag::{Monitor, Phase, Repaint, DOWN_SECS, UP_SECS};
use crate::fonts::{bold, medium};
use crate::settings::{Mode, Resolver};
use crate::tray::Tray;
use crate::{assets, sys};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Layout,
    Margin, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder,
};
use egui::epaint::{PathShape, PathStroke};
use egui_phosphor::regular as icon;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WIN_BG: Color32 = Color32::from_rgb(10, 23, 20);
const PANEL: Color32 = Color32::from_rgb(15, 34, 29);
const TILE: Color32 = Color32::from_rgb(21, 46, 40);
const TILE_HI: Color32 = Color32::from_rgb(30, 64, 55);
const BUBBLE: Color32 = Color32::from_rgb(11, 27, 23);
const BORDER: Color32 = Color32::from_rgb(32, 68, 58);
const TEXT: Color32 = Color32::from_rgb(236, 246, 241);
const MUTED: Color32 = Color32::from_rgb(130, 168, 155);
const LIGHT: Color32 = Color32::from_rgb(214, 243, 228);
const INK: Color32 = Color32::from_rgb(8, 20, 16);
const OK: Color32 = Color32::from_rgb(52, 220, 160);
const CYAN: Color32 = Color32::from_rgb(80, 200, 230);
const AMBER: Color32 = Color32::from_rgb(240, 160, 90);
const DANGER: Color32 = Color32::from_rgb(240, 110, 110);
const AVATARS: [Color32; 5] = [
    Color32::from_rgb(217, 119, 87),
    Color32::from_rgb(98, 160, 214),
    Color32::from_rgb(126, 190, 140),
    Color32::from_rgb(190, 140, 210),
    Color32::from_rgb(214, 180, 90),
];

const WINDOW_RADIUS: u8 = 22;
const TITLE_H: f32 = 44.0;
const NAV_W: f32 = 84.0;
const CARDS_W: f32 = 252.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Home,
    Sites,
    Activity,
    Settings,
}

pub struct DetourApp {
    ctl: Arc<Mutex<Controller>>,
    tab: Tab,
    new_domain: String,
    domain_error: Option<String>,
    autostart: bool,
    settings_error: Option<String>,
    logo: egui::TextureHandle,
    map: egui::TextureHandle,
    exe: PathBuf,
    tray: Option<Tray>,
    capture: Option<PathBuf>,
    capture_frames: u8,
    monitor: Monitor,
    was_on: bool,
}

pub fn apply_theme(ctx: &egui::Context) {
    crate::fonts::install(ctx);

    let mut v = egui::Visuals::dark();
    v.panel_fill = WIN_BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = BUBBLE;
    v.faint_bg_color = PANEL;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = OK.gamma_multiply(0.3);
    v.selection.stroke = Stroke::new(1.0, OK);
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(16);
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(12);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.bg_fill = TILE;
    v.widgets.inactive.weak_bg_fill = TILE;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.hovered.bg_fill = TILE_HI;
    v.widgets.hovered.weak_bg_fill = TILE_HI;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(60, 120, 104));
    v.widgets.active.bg_fill = TILE_HI;
    v.widgets.active.weak_bg_fill = TILE_HI;
    v.widgets.open.bg_fill = TILE;
    v.widgets.open.weak_bg_fill = TILE;
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = vec2(10.0, 8.0);
        s.spacing.button_padding = vec2(12.0, 7.0);
        s.spacing.interact_size.y = 30.0;
        s.text_styles.insert(egui::TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(egui::TextStyle::Monospace, FontId::monospace(12.0));
    });
}

impl DetourApp {
    pub fn new(
        ctx: &egui::Context,
        ctl: Arc<Mutex<Controller>>,
        exe: PathBuf,
        tray: Option<Tray>,
    ) -> Self {
        let size = 64;
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [size as usize, size as usize],
            &crate::icon::shield_rgba(size),
        );
        Self {
            ctl,
            tab: Tab::Home,
            new_domain: String::new(),
            domain_error: None,
            autostart: sys::autostart_enabled(),
            settings_error: None,
            logo: ctx.load_texture("logo", image, egui::TextureOptions::LINEAR),
            map: ctx.load_texture("world-map", crate::brand::world_map(1400), egui::TextureOptions::LINEAR),
            exe,
            tray,
            capture: None,
            capture_frames: 0,
            monitor: Monitor::new(),
            was_on: false,
        }
    }

    pub fn capture(&mut self, path: PathBuf, screen: &str) {
        self.capture = Some(path);
        self.tab = match screen {
            "sites" => Tab::Sites,
            "activity" => Tab::Activity,
            "settings" => Tab::Settings,
            _ => Tab::Home,
        };
    }
}

impl eframe::App for DetourApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(path) = &self.capture {
            let screenshot = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = screenshot {
                let pixels: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                if let Err(e) = image::save_buffer(
                    path,
                    &pixels,
                    image.width() as u32,
                    image.height() as u32,
                    image::ColorType::Rgba8,
                ) {
                    eprintln!("Could not save screenshot: {e}");
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if let Ok(mut c) = self.ctl.lock() {
            c.poll();
            if let Some(tray) = &mut self.tray {
                tray.sync(c.is_on());
            }
        }
        if sys::take_show_request() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctl_arc = self.ctl.clone();
        let mut ctl = ctl_arc.lock().unwrap_or_else(|e| e.into_inner());

        if ctx.input(|i| i.viewport().close_requested())
            && ctl.settings.close_to_tray
            && self.tray.is_some()
            && self.capture.is_none()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // Measurements: apply finished ones, and keep latency fresh while visible.
        let repaint: Repaint = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        self.monitor.poll();
        if self.capture.is_none() {
            self.monitor.probe_if_due(&repaint);
        }
        let on = ctl.is_on();
        if on && !self.was_on && self.capture.is_none() {
            self.monitor.forget_sites();
            self.monitor
                .check_sites_later(check_hosts(&ctl), Duration::from_millis(1500), &repaint);
        }
        self.was_on = on;

        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let full = ui.max_rect();
        let radius = if maximized { 0 } else { WINDOW_RADIUS };
        sys::round_window(
            (f32::from(WINDOW_RADIUS) * ctx.pixels_per_point()).round() as i32,
            !maximized,
        );
        // On Windows the drawing is offset one pixel down inside the window,
        // so the frame ends one pixel early to keep the bottom border visible.
        let frame = if cfg!(windows) && !maximized {
            Rect::from_min_max(full.min, pos2(full.max.x, full.max.y - 1.0 / ctx.pixels_per_point()))
        } else {
            full
        };
        ui.painter().rect_filled(frame, CornerRadius::same(radius), WIN_BG);
        ui.painter()
            .rect_stroke(frame, CornerRadius::same(radius), Stroke::new(1.0, BORDER), StrokeKind::Inside);

        let title = Rect::from_min_size(full.min, vec2(full.width(), TITLE_H));
        self.title_bar(ui, title, maximized);

        let body = Rect::from_min_max(
            pos2(full.left() + 16.0, full.top() + TITLE_H),
            pos2(full.right() - 16.0, full.bottom() - 16.0),
        );
        let nav_x = body.left();
        let content = Rect::from_min_max(pos2(nav_x + NAV_W + 16.0, body.top()), body.max);
        match self.tab {
            Tab::Home => self.connect_page(ui, body, content, &mut ctl, &repaint),
            tab => {
                panel(ui, content, |ui| match tab {
                    Tab::Sites => self.sites(ui, &mut ctl),
                    Tab::Activity => self.activity(ui, &ctl),
                    _ => self.settings(ui, &mut ctl),
                });
            }
        }
        self.nav(ui, body, &ctl);

        if !maximized {
            resize_edges(ui, full);
        }

        ctx.request_repaint_after(Duration::from_millis(if ctl.status == Status::Starting {
            40
        } else {
            1000
        }));
        if self.capture.is_some() {
            self.capture_frames = self.capture_frames.saturating_add(1);
            if self.capture_frames == 4 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // macOS composites the transparent window; on Windows the corners are
        // cut off by a window region, so the background is simply opaque.
        if cfg!(target_os = "macos") {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            WIN_BG.to_normalized_gamma_f32()
        }
    }
}

impl DetourApp {
    fn title_bar(&mut self, ui: &mut Ui, rect: Rect, maximized: bool) {
        let ctx = ui.ctx().clone();
        let drag = ui.interact(rect, ui.id().with("title-drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }
        let logo = Rect::from_center_size(pos2(rect.left() + 38.0, rect.center().y), vec2(26.0, 26.0));
        ui.painter().image(
            self.logo.id(),
            logo,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        ui.painter().text(
            pos2(logo.right() + 10.0, rect.center().y),
            Align2::LEFT_CENTER,
            "DETOUR",
            bold(16.0),
            TEXT,
        );

        let size = vec2(38.0, 28.0);
        let mut x = rect.right() - 16.0 - size.x;
        let y = rect.center().y - size.y / 2.0;
        let buttons = [
            (icon::X, 0, true),
            (if maximized { icon::ARROWS_IN_SIMPLE } else { icon::SQUARE }, 1, false),
            (icon::MINUS, 2, false),
        ];
        for (glyph, which, danger) in buttons {
            let r = Rect::from_min_size(pos2(x, y), size);
            let resp = ui.interact(r, ui.id().with(("win-btn", which)), Sense::click());
            if resp.hovered() {
                let fill = if danger { DANGER.gamma_multiply(0.85) } else { TILE_HI };
                ui.painter().rect_filled(r, CornerRadius::same(8), fill);
            }
            let color = if resp.hovered() && danger { Color32::WHITE } else { MUTED };
            ui.painter().text(r.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(14.0), color);
            if resp.clicked() {
                match which {
                    0 => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    1 => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                    _ => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                }
            }
            x -= size.x + 4.0;
        }
    }

    /// Floating, labelled navigation bar on the left.
    fn nav(&mut self, ui: &mut Ui, body: Rect, ctl: &Controller) {
        let items = [
            (Tab::Home, icon::SHIELD_CHECK, "Connect"),
            (Tab::Sites, icon::GLOBE, "Sites"),
            (Tab::Activity, icon::PULSE, "Activity"),
            (Tab::Settings, icon::GEAR_SIX, "Settings"),
        ];
        let item_h = 70.0;
        let height = item_h * items.len() as f32 + 64.0;
        let top = (body.center().y - height / 2.0).max(body.top());
        let rect = Rect::from_min_size(pos2(body.left(), top), vec2(NAV_W, height));
        ui.painter().rect_filled(rect, CornerRadius::same(24), PANEL);
        ui.painter()
            .rect_stroke(rect, CornerRadius::same(24), Stroke::new(1.0, BORDER), StrokeKind::Inside);

        let mut y = rect.top() + 10.0;
        for (tab, glyph, label) in items {
            let r = Rect::from_min_size(pos2(rect.left() + 8.0, y), vec2(NAV_W - 16.0, item_h - 6.0));
            let resp = ui.interact(r, ui.id().with(("nav", label)), Sense::click());
            let selected = self.tab == tab;
            let hover = ui.ctx().animate_bool(resp.id, resp.hovered());
            if selected {
                ui.painter().rect_filled(r, CornerRadius::same(16), TILE_HI);
            } else if hover > 0.0 {
                ui.painter().rect_filled(r, CornerRadius::same(16), TILE.gamma_multiply(hover));
            }
            let color = if selected { OK } else { MUTED.lerp_to_gamma(TEXT, hover * 0.7) };
            ui.painter().text(
                pos2(r.center().x, r.top() + 20.0),
                Align2::CENTER_CENTER,
                glyph,
                FontId::proportional(22.0),
                color,
            );
            ui.painter().text(
                pos2(r.center().x, r.bottom() - 14.0),
                Align2::CENTER_CENTER,
                label,
                medium(11.5),
                color,
            );
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.tab = tab;
            }
            y += item_h;
        }

        let (text, color) = status_summary(&ctl.status);
        let dot = pos2(rect.center().x, rect.bottom() - 26.0);
        ui.painter().circle_filled(dot, 5.0, color);
        ui.painter().circle_filled(dot, 9.0, color.gamma_multiply(0.16));
        ui.interact(Rect::from_center_size(dot, vec2(30.0, 30.0)), ui.id().with("nav-status"), Sense::hover())
            .on_hover_text(text);
    }

    fn connect_page(&mut self, ui: &mut Ui, body: Rect, content: Rect, ctl: &mut Controller, repaint: &Repaint) {
        // Layout: provider row, name, timer, power button, status on the left;
        // measurement cards on the right.
        let cards_x = content.right() - CARDS_W;
        let area = Rect::from_min_max(content.min, pos2(cards_x - 12.0, content.bottom()));
        let cx = area.center().x;
        let shift = ((body.height() - 560.0) * 0.5).clamp(0.0, 120.0);
        let top = content.top() + shift;

        // World map behind everything on this page, centred on the button.
        let center = pos2(cx, top + 318.0);
        let map_w = (body.right() - body.left()) * 1.05;
        let map_h = map_w * self.map.size_vec2().y / self.map.size_vec2().x;
        let map_rect = Rect::from_center_size(pos2(body.center().x, center.y - 40.0), vec2(map_w, map_h));
        ui.painter().image(
            self.map.id(),
            map_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::from_rgba_unmultiplied(70, 150, 120, 30),
        );

        // Ambient glow behind the button.
        let glow_color = match ctl.status {
            Status::On => OK,
            Status::Failed(_) => DANGER,
            _ => CYAN,
        };
        let strength = if ctl.is_on() { 1.0 } else { 0.45 };
        for (r, a) in [(260.0, 0.018), (200.0, 0.03), (150.0, 0.045)] {
            ui.painter().circle_filled(center, r, glow_color.gamma_multiply(a * strength));
        }

        // Provider "flags".
        let active = ctl.preset().id;
        let ids: Vec<(&'static str, String)> =
            ctl.presets().iter().map(|p| (p.id, p.preset.name.clone())).collect();
        let slot = 74.0;
        let row_w = slot * ids.len() as f32;
        let mut x = cx - row_w / 2.0 + slot / 2.0;
        let mut chosen = None;
        for (i, (id, name)) in ids.iter().enumerate() {
            let selected = *id == active;
            let c = pos2(x, top + 40.0);
            let r = Rect::from_center_size(c, vec2(64.0, 64.0));
            let resp = ui.interact(r, ui.id().with(("provider", *id)), Sense::click());
            let grow = ui.ctx().animate_bool(resp.id.with("sel"), selected);
            let hover = ui.ctx().animate_bool(resp.id.with("hover"), resp.hovered());
            let radius = 20.0 + 11.0 * grow + 1.5 * hover;
            self.provider_badge(ui, id, c, radius, name, AVATARS[i % AVATARS.len()]);
            if grow > 0.0 {
                ui.painter().circle_stroke(c, radius + 4.0, Stroke::new(2.0, OK.gamma_multiply(grow)));
            }
            if resp.on_hover_text(name.as_str()).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                chosen = Some(*id);
            }
            x += slot;
        }
        if let Some(id) = chosen {
            ctl.set_preset(id);
        }
        let name = ctl.preset().preset.name.clone();
        ui.painter().text(pos2(cx, top + 98.0), Align2::CENTER_CENTER, name, bold(24.0), TEXT);

        // Session timer.
        let (_, _, up) = ctl.stats();
        ui.painter().text(
            pos2(cx, top + 148.0),
            Align2::CENTER_CENTER,
            "Connection time",
            medium(13.0),
            MUTED,
        );
        let (clock, clock_color) = match up {
            Some(d) => (format_clock(d), TEXT),
            None => ("00:00:00".to_owned(), MUTED.gamma_multiply(0.55)),
        };
        ui.painter().text(pos2(cx, top + 186.0), Align2::CENTER_CENTER, clock, bold(46.0), clock_color);

        // Power button.
        if power_button(ui, center, &ctl.status).clicked() {
            ctl.toggle();
        }

        // Status line and the way into the advanced settings.
        let (status_text, status_color) = status_summary(&ctl.status);
        let y = top + 438.0;
        ui.painter().text(pos2(cx - 3.0, y), Align2::RIGHT_CENTER, "Status:", bold(15.0), TEXT);
        ui.painter().text(pos2(cx + 3.0, y), Align2::LEFT_CENTER, status_text, medium(15.0), status_color);
        if let Status::Failed(message) = &ctl.status {
            ui.painter().text(
                pos2(cx, y + 24.0),
                Align2::CENTER_CENTER,
                short(message, 70),
                FontId::proportional(12.0),
                DANGER,
            );
        }
        let button = Rect::from_center_size(pos2(cx, y + 58.0), vec2(168.0, 34.0));
        let resp = ui.interact(button, ui.id().with("advanced"), Sense::click());
        let hover = ui.ctx().animate_bool(resp.id, resp.hovered());
        ui.painter().rect_filled(button, CornerRadius::same(10), TILE.lerp_to_gamma(TILE_HI, hover));
        ui.painter()
            .rect_stroke(button, CornerRadius::same(10), Stroke::new(1.0, BORDER), StrokeKind::Inside);
        ui.painter().text(
            button.center(),
            Align2::CENTER_CENTER,
            format!("Advanced settings  {}", icon::CARET_RIGHT),
            medium(13.0),
            TEXT,
        );
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            self.tab = Tab::Settings;
        }

        self.cards(ui, Rect::from_min_size(pos2(cards_x, top), vec2(CARDS_W, 500.0)), ctl, repaint);
    }

    /// The round provider "flag": initials for an ISP profile, an icon for
    /// presets that are not tied to one ISP.
    fn provider_badge(&self, ui: &Ui, id: &str, c: Pos2, radius: f32, name: &str, color: Color32) {
        let glyph = match id {
            "generic" => Some(icon::GLOBE),
            "aggressive" => Some(icon::LIGHTNING),
            _ => None,
        };
        match glyph {
            Some(g) => {
                ui.painter().circle_filled(c, radius, color.gamma_multiply(0.28));
                ui.painter().circle_stroke(c, radius - 0.5, Stroke::new(1.0, color.gamma_multiply(0.8)));
                ui.painter().text(c, Align2::CENTER_CENTER, g, FontId::proportional(radius * 0.95), color);
            }
            None => draw_avatar(ui, c, radius, name, color),
        }
    }

    fn cards(&mut self, ui: &mut Ui, rect: Rect, ctl: &Controller, repaint: &Repaint) {
        let h = 104.0;
        let gap = 10.0;
        let m = &self.monitor;
        let card = |i: usize| Rect::from_min_size(pos2(rect.left(), rect.top() + i as f32 * (h + gap)), vec2(rect.width(), h));

        let last = |v: &std::collections::VecDeque<f32>| v.back().copied();
        // While a phase runs, show the live reading; afterwards the average.
        let down = if m.phase == Phase::Download { last(&m.download_samples) } else { m.download };
        let up = if m.phase == Phase::Upload { last(&m.upload_samples) } else { m.upload };
        let lat = last(&m.latency);
        let data = |v: &std::collections::VecDeque<f32>| v.iter().copied().collect::<Vec<f32>>();
        let pink = Color32::from_rgb(235, 90, 150);
        let live = |p| if m.phase == p { TEXT } else { OK };
        let metrics = [
            Metric { glyph: icon::DOWNLOAD_SIMPLE, label: "Download", value: fmt_value(down, "Mbps"), value_color: live(Phase::Download), data: data(&m.download_samples), from: CYAN, to: OK },
            Metric { glyph: icon::UPLOAD_SIMPLE, label: "Upload", value: fmt_value(up, "Mbps"), value_color: live(Phase::Upload), data: data(&m.upload_samples), from: CYAN, to: OK },
            Metric { glyph: icon::PULSE, label: "Latency", value: fmt_value(lat, "ms"), value_color: AMBER, data: data(&m.latency), from: AMBER, to: pink },
        ];
        for (i, metric) in metrics.iter().enumerate() {
            metric_card(ui, card(i), metric);
        }
        site_card(ui, card(3), m.sites.as_deref(), ctl.is_on());

        // The speed test is the only thing that moves real data, so it is a button.
        let button = Rect::from_min_size(pos2(rect.left(), rect.top() + 4.0 * (h + gap) + 2.0), vec2(rect.width(), 38.0));
        let testing = m.testing();
        let resp = ui.interact(button, ui.id().with("speed-test"), Sense::click());
        let hover = ui.ctx().animate_bool(resp.id, resp.hovered() && !testing);
        let fill = if testing { TILE } else { OK.gamma_multiply(0.16 + 0.12 * hover) };
        ui.painter().rect_filled(button, CornerRadius::same(12), fill);
        let elapsed = m.phase_started.map_or(0.0, |t| t.elapsed().as_secs_f32());
        let (label, progress) = match m.phase {
            Phase::Idle => (format!("{} Run speed test", icon::LIGHTNING), None),
            Phase::Download => (
                format!("Testing download \u{00b7} {:.0}s", (DOWN_SECS - elapsed).max(0.0).ceil()),
                Some(elapsed / DOWN_SECS),
            ),
            Phase::Upload => (
                format!("Testing upload \u{00b7} {:.0}s", (UP_SECS - elapsed).max(0.0).ceil()),
                Some(elapsed / UP_SECS),
            ),
            Phase::Sites => ("Checking sites\u{2026}".to_owned(), Some(1.0)),
        };
        if let Some(p) = progress {
            let done = Rect::from_min_size(button.min, vec2(button.width() * p.clamp(0.0, 1.0), button.height()));
            ui.painter().rect_filled(done, CornerRadius::same(12), OK.gamma_multiply(0.22));
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        ui.painter()
            .rect_stroke(button, CornerRadius::same(12), Stroke::new(1.0, OK.gamma_multiply(0.5)), StrokeKind::Inside);
        ui.painter().text(button.center(), Align2::CENTER_CENTER, label, medium(14.0), if testing { TEXT } else { OK });
        let resp = if testing {
            resp
        } else {
            resp.on_hover_text(format!(
                "About {} s. Downloads and uploads as fast as your line allows, \
                 which can use several hundred MB on a fast connection.",
                (DOWN_SECS + UP_SECS) as u32 + 2
            ))
        };
        if let (Some(why), false) = (m.speed_error, testing) {
            let galley = ui.painter().layout(
                why.to_owned(),
                FontId::proportional(12.0),
                AMBER,
                button.width(),
            );
            ui.painter().galley(pos2(button.left(), button.bottom() + 8.0), galley, AMBER);
        }
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !testing {
            self.monitor.run_speed_test(check_hosts(ctl), repaint);
        }
    }

    fn sites(&mut self, ui: &mut Ui, ctl: &mut Controller) {
        let full = ui.max_rect();
        let composer_h = 58.0;
        let scroll_rect = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - composer_h - 12.0));
        let composer_rect = Rect::from_min_max(pos2(full.left(), full.bottom() - composer_h), full.max);

        ui.scope_builder(UiBuilder::new().max_rect(scroll_rect), |ui| {
            page_title(ui, "Sites", "All websites are included by default. Use lists to limit coverage.");
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                bubble(ui, |ui| {
                    let mut all = ctl.settings.all_sites;
                    row(ui, "All websites", "Apply the bypass to every domain, listed or not", |ui| {
                        if toggle(ui, &mut all).changed() {
                            ctl.set_all_sites(all);
                        }
                    });
                    if all {
                        ui.label(RichText::new("The lists below are used only when this is off.").small().color(MUTED));
                    }
                });
                bubble(ui, |ui| {
                    ui.label(RichText::new("Built-in lists").font(medium(15.0)));
                    for list in assets::LISTS {
                        divider(ui);
                        let mut on = ctl.settings.list_enabled(list.id);
                        let count = assets::builtin_list(list.id).map_or(0, |l| l.len());
                        row(ui, list.title, &format!("{count} domains"), |ui| {
                            if toggle(ui, &mut on).changed() {
                                ctl.set_list_enabled(list.id, on);
                            }
                        });
                    }
                });
                bubble(ui, |ui| {
                    ui.label(RichText::new("Your sites").font(medium(15.0)));
                    let domains = ctl.settings.custom_domains.clone();
                    if domains.is_empty() {
                        ui.label(
                            RichText::new("Nothing here yet. Type a site below and press Enter. Subdomains are included.")
                                .small()
                                .color(MUTED),
                        );
                    }
                    for d in domains {
                        divider(ui);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&d).monospace());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let remove = egui::Button::new(RichText::new(icon::X).color(MUTED)).frame(false);
                                if ui.add(remove).on_hover_text("Remove").clicked() {
                                    ctl.remove_domain(&d);
                                }
                            });
                        });
                    }
                });
            });
        });
        ui.scope_builder(UiBuilder::new().max_rect(composer_rect), |ui| {
            self.composer(ui, ctl);
        });
    }

    /// Type a site and press Enter to cover it.
    fn composer(&mut self, ui: &mut Ui, ctl: &mut Controller) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, CornerRadius::same(29), TILE);
        ui.painter()
            .rect_stroke(rect, CornerRadius::same(29), Stroke::new(1.0, BORDER), StrokeKind::Inside);
        ui.painter().text(
            pos2(rect.left() + 26.0, rect.center().y),
            Align2::CENTER_CENTER,
            icon::GLOBE,
            FontId::proportional(18.0),
            MUTED,
        );

        let button = Rect::from_center_size(pos2(rect.right() - 29.0, rect.center().y), vec2(46.0, 46.0));
        let field = Rect::from_min_max(
            pos2(rect.left() + 48.0, rect.top() + 8.0),
            pos2(button.left() - 12.0, rect.bottom() - 8.0),
        );
        let edit = egui::TextEdit::singleline(&mut self.new_domain)
            .hint_text("Add a site to cover, e.g. example.com")
            .frame(Frame::NONE)
            .vertical_align(Align::Center)
            .desired_width(field.width());
        let resp = ui.put(field, edit);
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

        let resp_btn = ui.interact(button, ui.id().with("composer-add"), Sense::click());
        let hover = ui.ctx().animate_bool(resp_btn.id, resp_btn.hovered());
        ui.painter().circle_filled(button.center(), 23.0 + hover, OK);
        ui.painter().text(button.center(), Align2::CENTER_CENTER, icon::PLUS, FontId::proportional(21.0), INK);
        let clicked = resp_btn.on_hover_text("Add site").on_hover_cursor(CursorIcon::PointingHand).clicked();

        if (enter || clicked) && !self.new_domain.trim().is_empty() {
            match ctl.add_domain(&self.new_domain) {
                Ok(_) => {
                    self.new_domain.clear();
                    self.domain_error = None;
                }
                Err(e) => self.domain_error = Some(e),
            }
            resp.request_focus();
        }
        if let Some(e) = &self.domain_error {
            ui.painter().text(
                pos2(rect.left() + 24.0, rect.top() - 10.0),
                Align2::LEFT_BOTTOM,
                e,
                FontId::proportional(12.0),
                DANGER,
            );
        }
    }

    fn activity(&mut self, ui: &mut Ui, ctl: &Controller) {
        ui.horizontal(|ui| {
            let (handled, errors) = ctl.packet_stats();
            let sub = format!(
                "What the engine is doing right now \u{00b7} {handled} handled, {errors} errors"
            );
            ui.vertical(|ui| page_title(ui, "Activity", &sub));
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                if pill(ui, &format!("{} Clear", icon::TRASH)).clicked() {
                    ctl.clear_log();
                }
            });
        });
        let lines = ctl.log_lines();
        Frame::new()
            .fill(BUBBLE)
            .corner_radius(CornerRadius::same(18))
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if lines.is_empty() {
                            ui.label(RichText::new("Nothing yet. Connect and open a website.").color(MUTED));
                        }
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for l in &lines {
                            let bad = l.contains("Could not") || l.contains("failed");
                            ui.label(RichText::new(l).monospace().color(if bad { DANGER } else { MUTED }));
                        }
                    });
            });
    }

    fn settings(&mut self, ui: &mut Ui, ctl: &mut Controller) {
        page_title(ui, "Settings", "Make Detour work the way you do.");
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            bubble(ui, |ui| {
                ui.label(RichText::new("Bypass mode").font(medium(15.0)));
                let mut mode = ctl.settings.mode;
                if let Some(m) = chips(ui, &Mode::ALL.map(|m| (m, mode_label(m))), mode) {
                    mode = m;
                }
                ui.label(RichText::new(mode.description()).small().color(MUTED));
                ctl.set_mode(mode);
            });
            bubble(ui, |ui| {
                ui.label(RichText::new("DNS resolver").font(medium(15.0)));
                let mut resolver = ctl.settings.resolver;
                if let Some(r) = chips(ui, &Resolver::ALL.map(|r| (r, resolver_label(r))), resolver) {
                    resolver = r;
                }
                let address = ctl.options().ok().map_or("System resolver".into(), |o| {
                    o.doh.map(|r| format!("{r} over HTTPS")).unwrap_or_else(|| {
                        o.dns_redirect.map_or("System resolver".into(), |r| r.to_string())
                    })
                });
                ui.label(RichText::new(address).small().monospace().color(MUTED));
                ctl.set_resolver(resolver);
            });

            bubble(ui, |ui| {
                ui.label(RichText::new("General").font(medium(15.0)));
                let mut changed = false;
                divider(ui);
                row(ui, "Connect when Detour opens", "Protection starts by itself", |ui| {
                    changed |= toggle(ui, &mut ctl.settings.auto_enable).changed();
                });
                divider(ui);
                row(ui, "Keep running in the tray", "Closing the window hides it instead of quitting", |ui| {
                    changed |= toggle(ui, &mut ctl.settings.close_to_tray).changed();
                });
                divider(ui);
                row(ui, "Start at login", "Opens hidden in the tray when you sign in", |ui| {
                    if toggle(ui, &mut self.autostart).changed() {
                        self.settings_error = sys::set_autostart(self.autostart, &self.exe).err();
                        if self.settings_error.is_some() {
                            self.autostart = sys::autostart_enabled();
                        }
                    }
                });
                if changed {
                    ctl.save();
                }
                if let Some(e) = &self.settings_error {
                    ui.label(RichText::new(e).small().color(DANGER));
                }
            });

            bubble(ui, |ui| {
                ui.label(RichText::new("Network").font(medium(15.0)));
                // These two need the packet engine, which only Windows has.
                if cfg!(windows) {
                    divider(ui);
                    row(ui, "Encrypted DNS", "DNS-over-HTTPS for Cloudflare, Google or Quad9", |ui| {
                        if toggle(ui, &mut ctl.settings.encrypted_dns).changed() {
                            ctl.save();
                            ctl.restart_if_on();
                        }
                    });
                    divider(ui);
                    row(ui, "HTTP/3 fallback", "Block UDP/443 so sites retry over TCP (all-website mode)", |ui| {
                        if toggle(ui, &mut ctl.settings.quic_fallback).changed() {
                            ctl.save();
                            ctl.restart_if_on();
                        }
                    });
                }
                divider(ui);
                row(ui, "Detailed activity", "Record domain names in memory during this session", |ui| {
                    if toggle(ui, &mut ctl.settings.detailed_logs).changed() {
                        ctl.save();
                        ctl.restart_if_on();
                    }
                });
            });

            bubble(ui, |ui| {
                ui.label(RichText::new(format!("Detour {}", env!("CARGO_PKG_VERSION"))).font(medium(15.0)));
                ui.label(
                    RichText::new(
                        "Detour splits TLS handshakes and redirects DNS lookups system-wide or for a \
                         chosen list. Processing runs locally; your traffic goes directly to its \
                         destination. No accounts or telemetry. Speed tests use Cloudflare's public \
                         speed servers and only run when you press the button.",
                    )
                    .small()
                    .color(MUTED),
                );
                let engine = if cfg!(windows) {
                    "Local proxy for browsers and other proxy-aware apps, plus packet capture \
                     (WinDivert 2.2.2) for everything else."
                } else {
                    "Local proxy engine. Covers apps that follow the system proxy settings."
                };
                ui.label(RichText::new(engine).small().color(MUTED));
            });
            if pill(ui, "Quit Detour").clicked() {
                ctl.disable();
                ctl.settings.close_to_tray = false;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ui.add_space(6.0);
        });
    }
}

/// Sites the "site check" visits: one per enabled built-in list, plus the
/// first custom sites.
fn check_hosts(ctl: &Controller) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for (list, host) in [("discord", "discord.com"), ("roblox", "www.roblox.com")] {
        if ctl.settings.list_enabled(list) {
            hosts.push(host.to_owned());
        }
    }
    hosts.extend(ctl.settings.custom_domains.iter().take(2).cloned());
    if hosts.is_empty() {
        hosts = vec!["discord.com".into(), "www.roblox.com".into()];
    }
    hosts
}

fn mode_label(m: Mode) -> &'static str {
    match m {
        Mode::Profile => "Provider",
        Mode::Turbo => "Turbo",
        Mode::Balanced => "Balanced",
        Mode::Strong => "Strong",
    }
}

fn resolver_label(r: Resolver) -> &'static str {
    match r {
        Resolver::Profile => "Provider",
        Resolver::Cloudflare => "Cloudflare",
        Resolver::Google => "Google",
        Resolver::Quad9 => "Quad9",
        Resolver::System => "System",
    }
}

fn status_summary(status: &Status) -> (&'static str, Color32) {
    match status {
        Status::On => ("Connected", OK),
        Status::Starting => ("Connecting\u{2026}", CYAN),
        Status::Off => ("Disconnected", MUTED),
        Status::Failed(_) => ("Needs attention", DANGER),
    }
}

fn short(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or(text).trim();
    if first.chars().count() <= max {
        first.to_owned()
    } else {
        let cut: String = first.chars().take(max.saturating_sub(1)).collect();
        format!("{}\u{2026}", cut.trim_end())
    }
}

pub fn format_clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

fn fmt_value(v: Option<f32>, unit: &str) -> String {
    match v {
        Some(v) if v >= 100.0 => format!("{v:.0} {unit}"),
        Some(v) => format!("{v:.1} {unit}"),
        None => format!("\u{2014} {unit}"),
    }
}

/// The big round connect button.
fn power_button(ui: &mut Ui, center: Pos2, status: &Status) -> egui::Response {
    let radius = 74.0;
    let rect = Rect::from_center_size(center, vec2(radius * 2.0 + 24.0, radius * 2.0 + 24.0));
    let resp = ui.interact(rect, ui.id().with("power"), Sense::click());
    let ctx = ui.ctx().clone();
    let on = *status == Status::On;
    let starting = *status == Status::Starting;
    let failed = matches!(status, Status::Failed(_));
    let t = ctx.animate_bool(resp.id.with("on"), on);
    let hover = ctx.animate_bool(resp.id.with("hover"), resp.hovered());

    let accent = if failed { DANGER } else { OK };
    let r = radius + 2.0 * hover;
    let painter = ui.painter();
    if on {
        for (d, a) in [(22.0, 0.07), (14.0, 0.10), (7.0, 0.16)] {
            painter.circle_filled(center, r + d, accent.gamma_multiply(a));
        }
    }
    painter.circle_filled(center, r, TILE.lerp_to_gamma(accent.gamma_multiply(0.30), t));
    let ring = if failed { DANGER } else { BORDER.lerp_to_gamma(OK, t.max(hover * 0.6)) };
    painter.circle_stroke(center, r, Stroke::new(3.0, ring));
    painter.circle_stroke(center, r - 12.0, Stroke::new(1.0, ring.gamma_multiply(0.35)));

    if starting {
        // A short arc that circles the button while connecting.
        let time = ctx.input(|i| i.time) as f32;
        let start = time * 4.0;
        let points: Vec<Pos2> = (0..=24)
            .map(|i| {
                let a = start + 1.6 * i as f32 / 24.0;
                center + vec2(a.cos(), a.sin()) * r
            })
            .collect();
        painter.add(Shape::line(points, Stroke::new(4.0, CYAN)));
    }
    let glyph_color = if on {
        OK
    } else if failed {
        DANGER
    } else {
        MUTED.lerp_to_gamma(TEXT, hover)
    };
    painter.text(center, Align2::CENTER_CENTER, icon::POWER, FontId::proportional(54.0), glyph_color);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

struct Metric {
    glyph: &'static str,
    label: &'static str,
    value: String,
    value_color: Color32,
    data: Vec<f32>,
    from: Color32,
    to: Color32,
}

fn metric_card(ui: &mut Ui, rect: Rect, metric: &Metric) {
    let Metric { glyph, label, value, value_color, data, from, to } = metric;
    ui.painter().rect_filled(rect, CornerRadius::same(16), PANEL);
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(16), Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let badge = Rect::from_min_size(rect.left_top() + vec2(14.0, 12.0), vec2(24.0, 24.0));
    ui.painter().rect_filled(badge, CornerRadius::same(8), TILE_HI);
    ui.painter().text(badge.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(13.0), MUTED);
    ui.painter().text(
        pos2(badge.right() + 10.0, badge.center().y),
        Align2::LEFT_CENTER,
        label,
        medium(13.5),
        TEXT,
    );
    ui.painter().text(
        pos2(rect.right() - 14.0, badge.center().y),
        Align2::RIGHT_CENTER,
        value,
        bold(13.5),
        *value_color,
    );
    let area = Rect::from_min_max(pos2(rect.left() + 14.0, rect.top() + 46.0), pos2(rect.right() - 14.0, rect.bottom() - 12.0));
    sparkline(ui, area, data, *from, *to);
}

fn site_card(ui: &mut Ui, rect: Rect, sites: Option<&[(String, bool)]>, connected: bool) {
    ui.painter().rect_filled(rect, CornerRadius::same(16), PANEL);
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(16), Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let badge = Rect::from_min_size(rect.left_top() + vec2(14.0, 12.0), vec2(24.0, 24.0));
    ui.painter().rect_filled(badge, CornerRadius::same(8), TILE_HI);
    ui.painter().text(badge.center(), Align2::CENTER_CENTER, icon::SHIELD_CHECK, FontId::proportional(13.0), MUTED);
    ui.painter().text(
        pos2(badge.right() + 10.0, badge.center().y),
        Align2::LEFT_CENTER,
        "Site check",
        medium(13.5),
        TEXT,
    );

    let (ok, total) = sites.map_or((0, 0), |s| (s.iter().filter(|x| x.1).count(), s.len()));
    let ratio = if total == 0 { 0.0 } else { ok as f32 / total as f32 };
    let color = if total == 0 {
        MUTED
    } else if ok == total {
        OK
    } else if ok == 0 {
        DANGER
    } else {
        AMBER
    };
    let value = if total == 0 { "\u{2014}".to_owned() } else { format!("{ok}/{total} reachable") };
    ui.painter().text(pos2(rect.right() - 14.0, badge.center().y), Align2::RIGHT_CENTER, value, bold(13.5), color);

    let bar = Rect::from_min_size(pos2(rect.left() + 14.0, rect.top() + 54.0), vec2(rect.width() - 28.0, 6.0));
    ui.painter().rect_filled(bar, CornerRadius::same(3), TILE_HI);
    if ratio > 0.0 {
        ui.painter().rect_filled(
            Rect::from_min_size(bar.min, vec2(bar.width() * ratio, bar.height())),
            CornerRadius::same(3),
            color,
        );
    }
    let detail = match sites {
        Some(s) if !s.is_empty() => s
            .iter()
            .map(|(h, ok)| format!("{} {}", if *ok { icon::CHECK } else { icon::X }, h.trim_start_matches("www.")))
            .take(3)
            .collect::<Vec<_>>()
            .join("   "),
        _ if connected => "Checking\u{2026}".to_owned(),
        _ => "Connect to check your sites".to_owned(),
    };
    ui.painter().text(
        pos2(rect.left() + 14.0, rect.bottom() - 18.0),
        Align2::LEFT_CENTER,
        short(&detail, 44),
        FontId::proportional(11.5),
        MUTED,
    );
}

/// A smooth-looking trend line with a gradient stroke.
fn sparkline(ui: &mut Ui, area: Rect, data: &[f32], from: Color32, to: Color32) {
    // One measurement is drawn as a short flat line.
    let single;
    let data = if let [only] = data {
        single = [*only, *only];
        &single[..]
    } else {
        data
    };
    if data.len() < 2 {
        let y = area.center().y + 6.0;
        ui.painter().line_segment(
            [pos2(area.left(), y), pos2(area.right(), y)],
            Stroke::new(1.5, BORDER),
        );
        return;
    }
    let min = data.iter().copied().fold(f32::INFINITY, f32::min);
    let max = data.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let span = (max - min).max(max * 0.25).max(0.001);
    let n = (data.len() - 1) as f32;
    let points: Vec<Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = area.left() + area.width() * i as f32 / n;
            let y = area.bottom() - 2.0 - (v - min) / span * (area.height() - 6.0);
            pos2(x, y)
        })
        .collect();
    let stroke = PathStroke::new_uv(2.0, move |rect: Rect, p: Pos2| {
        let t = ((p.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        from.lerp_to_gamma(to, t)
    });
    ui.painter().add(Shape::Path(PathShape::line(points, stroke)));
}

/// A rounded panel with the content drawn inside an 18 px inset.
fn panel(ui: &mut Ui, rect: Rect, add: impl FnOnce(&mut Ui)) {
    ui.painter().rect_filled(rect, CornerRadius::same(20), PANEL);
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(20), Stroke::new(1.0, BORDER), StrokeKind::Inside);
    ui.scope_builder(UiBuilder::new().max_rect(rect.shrink(18.0)), |ui| {
        ui.set_clip_rect(rect.shrink(1.0));
        add(ui);
    });
}

fn page_title(ui: &mut Ui, title: &str, sub: &str) {
    ui.label(RichText::new(title).font(bold(26.0)));
    if !sub.is_empty() {
        ui.label(RichText::new(sub).color(MUTED));
    }
    ui.add_space(8.0);
}

fn draw_avatar(ui: &Ui, center: Pos2, radius: f32, name: &str, color: Color32) {
    ui.painter().circle_filled(center, radius, color.gamma_multiply(0.22));
    ui.painter().circle_stroke(center, radius - 0.5, Stroke::new(1.0, color.gamma_multiply(0.7)));
    let initials: String = name
        .split_whitespace()
        .filter(|w| w.chars().next().is_some_and(char::is_alphabetic))
        .take(2)
        .filter_map(|w| w.chars().next())
        .collect::<String>()
        .to_uppercase();
    ui.painter().text(
        center,
        Align2::CENTER_CENTER,
        initials,
        bold(radius * 0.72),
        color,
    );
}

fn pill(ui: &mut Ui, label: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(14.0), TEXT);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 32.0, 36.0), Sense::click());
    let hover = ui.ctx().animate_bool(resp.id, resp.hovered());
    ui.painter().rect_filled(rect, CornerRadius::same(18), TILE.lerp_to_gamma(TILE_HI, hover));
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, TEXT);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A row of selectable chips; returns the newly picked value, if any.
fn chips<T: Copy + PartialEq>(ui: &mut Ui, items: &[(T, &str)], current: T) -> Option<T> {
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        for (value, label) in items {
            let selected = *value == current;
            let color = if selected { INK } else { TEXT };
            let galley = ui.painter().layout_no_wrap((*label).to_owned(), FontId::proportional(13.5), color);
            let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 28.0, 32.0), Sense::click());
            let hover = ui.ctx().animate_bool(resp.id, resp.hovered());
            let fill = if selected { LIGHT } else { TILE.lerp_to_gamma(TILE_HI, hover) };
            ui.painter().rect_filled(rect, CornerRadius::same(16), fill);
            ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !selected {
                picked = Some(*value);
            }
        }
    });
    picked
}

fn bubble(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(BUBBLE)
        .corner_radius(CornerRadius::same(18))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            add(ui);
        });
}

fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, BORDER);
}

fn row(ui: &mut Ui, title: &str, sub: &str, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new(title).font(medium(14.5)));
            ui.label(RichText::new(sub).small().color(MUTED));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
}

fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(42.0, 24.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let track = TILE_HI.lerp_to_gamma(OK, t);
    ui.painter().rect_filled(rect, CornerRadius::same(12), track);
    let x = egui::lerp(rect.left() + 12.0..=rect.right() - 12.0, t);
    ui.painter().circle_filled(pos2(x, rect.center().y), 9.0, Color32::WHITE);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Resize handles for the borderless window.
fn resize_edges(ui: &mut Ui, full: Rect) {
    use egui::ResizeDirection as D;
    let (t, c) = (6.0, 14.0);
    let handles = [
        (Rect::from_min_max(pos2(full.left() + c, full.top()), pos2(full.right() - c, full.top() + t)), D::North, CursorIcon::ResizeVertical),
        (Rect::from_min_max(pos2(full.left() + c, full.bottom() - t), pos2(full.right() - c, full.bottom())), D::South, CursorIcon::ResizeVertical),
        (Rect::from_min_max(pos2(full.left(), full.top() + c), pos2(full.left() + t, full.bottom() - c)), D::West, CursorIcon::ResizeHorizontal),
        (Rect::from_min_max(pos2(full.right() - t, full.top() + c), pos2(full.right(), full.bottom() - c)), D::East, CursorIcon::ResizeHorizontal),
        (Rect::from_min_size(full.left_top(), vec2(c, c)), D::NorthWest, CursorIcon::ResizeNwSe),
        (Rect::from_min_size(pos2(full.right() - c, full.top()), vec2(c, c)), D::NorthEast, CursorIcon::ResizeNeSw),
        (Rect::from_min_size(pos2(full.left(), full.bottom() - c), vec2(c, c)), D::SouthWest, CursorIcon::ResizeNeSw),
        (Rect::from_min_size(pos2(full.right() - c, full.bottom() - c), vec2(c, c)), D::SouthEast, CursorIcon::ResizeNwSe),
    ];
    for (i, (rect, dir, cursor)) in handles.into_iter().enumerate() {
        let resp = ui.interact(rect, ui.id().with(("resize", i)), Sense::drag());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(cursor);
        }
        if resp.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats_hours_minutes_seconds() {
        assert_eq!(format_clock(Duration::from_secs(0)), "00:00:00");
        assert_eq!(format_clock(Duration::from_secs(45 * 60 + 29)), "00:45:29");
        assert_eq!(format_clock(Duration::from_secs(3 * 3600 + 5)), "03:00:05");
        assert_eq!(format_clock(Duration::from_secs(100 * 3600)), "100:00:00");
    }

    #[test]
    fn values_show_a_dash_until_measured() {
        assert_eq!(fmt_value(None, "Mbps"), "\u{2014} Mbps");
        assert_eq!(fmt_value(Some(47.34), "Mbps"), "47.3 Mbps");
        assert_eq!(fmt_value(Some(451.2), "Mbps"), "451 Mbps");
    }

    #[test]
    fn short_keeps_first_line_and_truncates() {
        assert_eq!(short("one\ntwo", 10), "one");
        assert_eq!(short("abcdefghij", 5), "abcd\u{2026}");
        assert_eq!(short("ok", 5), "ok");
    }
}
