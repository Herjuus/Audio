#![allow(clippy::manual_range_contains)]
#![allow(clippy::collapsible_if)]
// On Windows, don't open a console window when launching the GUI.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use eframe::egui;
use micapp::app::MicApp;
use tray_icon::{
    menu::{Menu, MenuItem},
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

#[derive(Debug, Clone, PartialEq)]
enum TrayCmd { Show, Quit }

fn build_tray(
    pending: Arc<Mutex<Vec<TrayCmd>>>,
    ctx_cell: Arc<Mutex<Option<egui::Context>>>,
) -> TrayIcon {
    let show_item = MenuItem::new("Show", true, None);
    let quit_item = MenuItem::new("Quit", true, None);

    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();

    // Forward menu events into our shared queue and wake egui.
    tray_icon::menu::MenuEvent::set_event_handler(Some(move |event: tray_icon::menu::MenuEvent| {
        let cmd = if event.id == show_id {
            TrayCmd::Show
        } else if event.id == quit_id {
            TrayCmd::Quit
        } else {
            return;
        };
        pending.lock().unwrap().push(cmd);
        if let Some(ctx) = ctx_cell.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
    }));

    let menu = Menu::new();
    menu.append(&show_item).ok();
    menu.append(&quit_item).ok();

    let (rgba, w, h) = tray_icon_rgba();
    let icon = tray_icon::Icon::from_rgba(rgba, w, h).expect("valid icon");

    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("micapp — mic processing")
        .with_icon(icon)
        .build()
        .expect("tray icon")
}

fn main() -> eframe::Result {
    let pending: Arc<Mutex<Vec<TrayCmd>>> = Arc::new(Mutex::new(Vec::new()));
    let ctx_cell: Arc<Mutex<Option<egui::Context>>> = Arc::new(Mutex::new(None));

    let _tray = build_tray(Arc::clone(&pending), Arc::clone(&ctx_cell));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("micapp")
            .with_inner_size([420.0, 720.0])
            .with_visible(false),
        ..Default::default()
    };

    eframe::run_native(
        "micapp",
        options,
        Box::new(move |cc| {
            // Store the egui context so the event handler can wake it.
            *ctx_cell.lock().unwrap() = Some(cc.egui_ctx.clone());
            Ok(Box::new(TrayApp::new(cc, Arc::clone(&pending))))
        }),
    )
}

struct TrayApp {
    inner: MicApp,
    pending: Arc<Mutex<Vec<TrayCmd>>>,
    quitting: bool,
}

impl TrayApp {
    fn new(cc: &eframe::CreationContext, pending: Arc<Mutex<Vec<TrayCmd>>>) -> Self {
        Self { inner: MicApp::new(cc), pending, quitting: false }
    }
}

impl eframe::App for TrayApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Drain tray commands queued by the event handler.
        let cmds: Vec<TrayCmd> = std::mem::take(&mut *self.pending.lock().unwrap());
        for cmd in cmds {
            match cmd {
                TrayCmd::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                TrayCmd::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
            }
        }

        // Intercept window close — hide to tray unless Quit was clicked.
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.quitting {
                return;
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }

        self.inner.update(ctx, frame);

        // Keep ticking when hidden so tray events wake us promptly.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}
