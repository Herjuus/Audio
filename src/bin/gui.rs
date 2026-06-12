#![allow(clippy::manual_range_contains)]
#![allow(clippy::collapsible_if)]

use eframe::egui;
use micapp::app::MicApp;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIcon, TrayIconBuilder,
};

// 16x16 RGBA mic icon — simple circle with a rectangle, all in green.
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

            // Outer circle ring (mic body)
            let on = (dist >= 4.5 && dist <= 6.5)
                // stem at bottom centre
                || (dx.abs() < 1.0 && dy > 4.5 && dy < 7.0)
                // base bar at bottom
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
    // Build tray before the event loop so it's visible immediately.
    let (tray, show_item, quit_item) = build_tray();

    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();

    // Keep tray alive for the duration of the app.
    let _tray = tray;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("micapp")
            .with_inner_size([420.0, 720.0])
            // Start hidden — user opens via tray.
            .with_visible(false)
            // Closing the window hides it instead of exiting.
            .with_close_button(true),
        ..Default::default()
    };

    eframe::run_native(
        "micapp",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();

            // Poll tray menu events each frame via a repaint request.
            // egui runs update() on repaint, so we hook a channel-check there.
            let show_id = show_id.clone();
            let quit_id = quit_id.clone();

            // Spawn a thread that forwards tray events as egui repaints.
            std::thread::spawn(move || {
                let receiver = MenuEvent::receiver();
                loop {
                    if let Ok(event) = receiver.recv() {
                        if event.id == show_id || event.id == quit_id {
                            ctx.request_repaint();
                        }
                    }
                }
            });

            Ok(Box::new(TrayApp::new(cc, show_item.id().clone(), quit_item.id().clone())))
        }),
    )
}

struct TrayApp {
    inner: MicApp,
    show_id: tray_icon::menu::MenuId,
    quit_id: tray_icon::menu::MenuId,
}

impl TrayApp {
    fn new(
        cc: &eframe::CreationContext,
        show_id: tray_icon::menu::MenuId,
        quit_id: tray_icon::menu::MenuId,
    ) -> Self {
        Self {
            inner: MicApp::new(cc),
            show_id,
            quit_id,
        }
    }
}

impl eframe::App for TrayApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Handle tray menu events.
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.quit_id {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if event.id == self.show_id {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }

        // Intercept close — hide to tray instead of quitting.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }

        self.inner.update(ctx, frame);
    }
}
