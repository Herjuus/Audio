#![allow(clippy::manual_range_contains)]
#![allow(clippy::collapsible_if)]
// On Windows, don't open a console window when launching the GUI.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui;
use micapp::app::MicApp;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIcon, TrayIconBuilder,
};

// 16x16 RGBA mic icon — simple filled circle in green.
fn tray_icon_rgba() -> (Vec<u8>, u32, u32) {
    const W: u32 = 16;
    const H: u32 = 16;
    let mut rgba = vec![0u8; (W * H * 4) as usize];

    let cx = W as f32 / 2.0;
    let cy = H as f32 / 2.0 - 1.0;

    for y in 0..H {
        for x in 0..W {
            let idx = ((y * W + x) * 4) as usize;
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();

            let on = (dist >= 4.5 && dist <= 6.5)
                || (dx.abs() < 1.0 && dy > 4.5 && dy < 7.0)
                || (dy > 6.0 && dy < 7.5 && dx.abs() < 2.5);

            if on {
                rgba[idx]     = 80;
                rgba[idx + 1] = 200;
                rgba[idx + 2] = 120;
                rgba[idx + 3] = 255;
            }
        }
    }
    (rgba, W, H)
}

fn build_tray() -> (TrayIcon, MenuItem, MenuItem) {
    let show_item = MenuItem::new("Show", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append(&show_item).ok();
    menu.append(&quit_item).ok();

    let (rgba, w, h) = tray_icon_rgba();
    let icon = tray_icon::Icon::from_rgba(rgba, w, h).expect("valid icon");

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("micapp — mic processing")
        .with_icon(icon)
        .build()
        .expect("tray icon");

    (tray, show_item, quit_item)
}

fn main() -> eframe::Result {
    let (tray, show_item, quit_item) = build_tray();
    let _tray = tray; // keep alive for the lifetime of the app

    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("micapp")
            .with_inner_size([420.0, 720.0])
            .with_visible(false), // start hidden in tray
        ..Default::default()
    };

    eframe::run_native(
        "micapp",
        options,
        Box::new(move |cc| {
            Ok(Box::new(TrayApp::new(cc, show_id, quit_id)))
        }),
    )
}

struct TrayApp {
    inner: MicApp,
    show_id: tray_icon::menu::MenuId,
    quit_id: tray_icon::menu::MenuId,
    /// Set to true by the Quit menu item so the close handler lets it through.
    quitting: bool,
}

impl TrayApp {
    fn new(
        cc: &eframe::CreationContext,
        show_id: tray_icon::menu::MenuId,
        quit_id: tray_icon::menu::MenuId,
    ) -> Self {
        Self { inner: MicApp::new(cc), show_id, quit_id, quitting: false }
    }
}

impl eframe::App for TrayApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.quit_id {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if event.id == self.show_id {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }

        // Intercept window close — hide to tray unless Quit was clicked.
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.quitting {
                return; // let egui close normally
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }

        self.inner.update(ctx, frame);

        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}
