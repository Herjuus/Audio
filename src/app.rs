// GUI application — MicApp.
//
// Runs on the main (GUI) thread. Audio engine runs on a background thread.
// All communication is lock-free: settings travel via AudioCmd/ring-buffer,
// peak level travels back via Arc<AtomicU32>.

use eframe::egui;
use egui::{Color32, Rect, Rounding, Stroke, Vec2};

use crate::audio::{AudioEngine, input_device_names, output_device_names};
use crate::state::{Settings, config_path};

fn make_auto_launch() -> auto_launch::AutoLaunch {
    let exe = std::env::current_exe().unwrap_or_default();
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name("micapp")
        .set_app_path(&exe.to_string_lossy())
        .build()
        .expect("auto-launch")
}

pub struct MicApp {
    engine: AudioEngine,
    input_devices: Vec<String>,
    output_devices: Vec<String>,
    selected_input: String,
    selected_output: String,
    selected_monitor: String,
    settings: Settings,
    /// Decayed display value for the peak meter (dBFS).
    displayed_peak_db: f32,
    status_message: String,
}

impl MicApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let input_devices = input_device_names();
        let output_devices = output_device_names();

        // Load persisted settings; fall back to defaults if the file is missing.
        let settings = Settings::load(&config_path()).unwrap_or_default();

        // Restore last-used devices, falling back to the first available device.
        let selected_input = settings.input_device.clone()
            .filter(|n| input_devices.contains(n))
            .or_else(|| input_devices.first().cloned())
            .unwrap_or_default();

        let selected_output = settings.output_device.clone()
            .filter(|n| output_devices.contains(n))
            .or_else(|| output_devices.first().cloned())
            .unwrap_or_default();

        let selected_monitor = settings.monitor_device.clone()
            .filter(|n| output_devices.contains(n))
            .or_else(|| output_devices.first().cloned())
            .unwrap_or_default();

        let monitor_enabled = settings.monitor_enabled;

        let engine = AudioEngine::new(
            if selected_input.is_empty() { None } else { Some(selected_input.clone()) },
            if selected_output.is_empty() { None } else { Some(selected_output.clone()) },
            settings.clone(),
            if selected_monitor.is_empty() || !monitor_enabled { None } else { Some(selected_monitor.clone()) },
            monitor_enabled,
        );

        Self {
            engine,
            input_devices,
            output_devices,
            selected_input,
            selected_output,
            selected_monitor,
            settings,
            displayed_peak_db: -120.0,
            status_message: String::new(),
        }
    }

    fn save_config(&mut self) {
        self.settings.input_device = if self.selected_input.is_empty() {
            None
        } else {
            Some(self.selected_input.clone())
        };
        self.settings.output_device = if self.selected_output.is_empty() {
            None
        } else {
            Some(self.selected_output.clone())
        };
        self.settings.monitor_device = if self.selected_monitor.is_empty() {
            None
        } else {
            Some(self.selected_monitor.clone())
        };

        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Err(e) = self.settings.save(&path) {
            self.status_message = format!("Config save error: {e}");
        }
    }
}

impl eframe::App for MicApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // --- Meter update (instant attack, ~5% decay per frame in dB) ----------
        let raw_db = self.engine.peak_db();
        if raw_db > self.displayed_peak_db {
            self.displayed_peak_db = raw_db;
        } else {
            // 5% decay per frame towards -120 dBFS
            self.displayed_peak_db += (-120.0 - self.displayed_peak_db) * 0.05;
        }

        // Repaint at ~30fps for meter animation — avoid burning CPU.
        ctx.request_repaint_after(std::time::Duration::from_millis(33));

        let mut settings_changed = false;

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("micapp");
                ui.add_space(4.0);

                // --- Device selectors -------------------------------------------
                ui.horizontal(|ui| {
                    ui.label("Input:");
                    let prev_input = self.selected_input.clone();
                    egui::ComboBox::from_id_salt("input_device")
                        .selected_text(&self.selected_input)
                        .show_ui(ui, |ui| {
                            for name in &self.input_devices {
                                ui.selectable_value(
                                    &mut self.selected_input,
                                    name.clone(),
                                    name,
                                );
                            }
                        });
                    if self.selected_input != prev_input {
                        self.engine.change_devices(
                            Some(self.selected_input.clone()),
                            Some(self.selected_output.clone()),
                            if self.settings.monitor_enabled { Some(self.selected_monitor.clone()) } else { None },
                            self.settings.monitor_enabled,
                        );
                        self.save_config();
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Output:");
                    let prev_output = self.selected_output.clone();
                    egui::ComboBox::from_id_salt("output_device")
                        .selected_text(&self.selected_output)
                        .show_ui(ui, |ui| {
                            for name in &self.output_devices {
                                ui.selectable_value(
                                    &mut self.selected_output,
                                    name.clone(),
                                    name,
                                );
                            }
                        });
                    if self.selected_output != prev_output {
                        self.engine.change_devices(
                            Some(self.selected_input.clone()),
                            Some(self.selected_output.clone()),
                            if self.settings.monitor_enabled { Some(self.selected_monitor.clone()) } else { None },
                            self.settings.monitor_enabled,
                        );
                        self.save_config();
                    }
                });

                // --- Monitor (hear yourself) ------------------------------------
                ui.horizontal(|ui| {
                    let prev_mon_enabled = self.settings.monitor_enabled;
                    ui.checkbox(&mut self.settings.monitor_enabled, "Monitor:");
                    let prev_monitor = self.selected_monitor.clone();
                    egui::ComboBox::from_id_salt("monitor_device")
                        .selected_text(&self.selected_monitor)
                        .show_ui(ui, |ui| {
                            for name in &self.output_devices {
                                ui.selectable_value(
                                    &mut self.selected_monitor,
                                    name.clone(),
                                    name,
                                );
                            }
                        });
                    if self.settings.monitor_enabled != prev_mon_enabled {
                        // Toggle without reconnect — just flip the atomic.
                        self.engine.set_monitor_active(self.settings.monitor_enabled);
                        self.save_config();
                    }
                    if self.selected_monitor != prev_monitor {
                        // Device changed — need a full reconnect.
                        self.engine.change_devices(
                            Some(self.selected_input.clone()),
                            Some(self.selected_output.clone()),
                            if self.settings.monitor_enabled { Some(self.selected_monitor.clone()) } else { None },
                            self.settings.monitor_enabled,
                        );
                        self.save_config();
                    }
                });

                ui.add_space(6.0);

                // --- Peak meter bar ---------------------------------------------
                {
                    let bar_width = ui.available_width();
                    let bar_height = 18.0;
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(bar_width, bar_height),
                        egui::Sense::hover(),
                    );

                    // Background
                    ui.painter().rect_filled(rect, Rounding::same(2.0), Color32::from_gray(40));

                    // Filled portion: map dBFS [-60, 0] → [0, bar_width]
                    let db_min = -60.0_f32;
                    let db_max = 0.0_f32;
                    let frac = ((self.displayed_peak_db - db_min) / (db_max - db_min))
                        .clamp(0.0, 1.0);
                    let fill_w = bar_width * frac;

                    if fill_w > 0.0 {
                        let fill_color = if self.displayed_peak_db > -3.0 {
                            Color32::from_rgb(220, 50, 50)   // red: near 0 dBFS
                        } else if self.displayed_peak_db > -6.0 {
                            Color32::from_rgb(220, 180, 40)  // yellow: caution
                        } else {
                            Color32::from_rgb(60, 180, 80)   // green: healthy
                        };
                        let fill_rect = Rect::from_min_size(rect.min, Vec2::new(fill_w, bar_height));
                        ui.painter().rect_filled(fill_rect, Rounding::same(2.0), fill_color);
                    }

                    // Border
                    ui.painter().rect_stroke(rect, Rounding::same(2.0), Stroke::new(1.0, Color32::from_gray(80)));

                    // dB label
                    let label = if self.displayed_peak_db <= -119.0 {
                        "-inf dBFS".to_string()
                    } else {
                        format!("{:.1} dBFS", self.displayed_peak_db)
                    };
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        &label,
                        egui::FontId::proportional(11.0),
                        Color32::WHITE,
                    );
                }

                ui.add_space(6.0);

                // --- Quick-bypass row -------------------------------------------
                ui.label("Enabled:");
                ui.horizontal_wrapped(|ui| {
                    if ui.checkbox(&mut self.settings.gain_enabled,       "Gain").changed()       { settings_changed = true; }
                    if ui.checkbox(&mut self.settings.high_pass_enabled,  "HPF").changed()        { settings_changed = true; }
                    if ui.checkbox(&mut self.settings.gate_enabled,       "Gate").changed()       { settings_changed = true; }
                    if ui.checkbox(&mut self.settings.eq_enabled,         "EQ").changed()         { settings_changed = true; }
                    if ui.checkbox(&mut self.settings.compressor_enabled, "Comp").changed()       { settings_changed = true; }
                    if ui.checkbox(&mut self.settings.limiter_enabled,    "Limiter").changed()    { settings_changed = true; }
                });

                ui.add_space(4.0);

                // --- Gain -------------------------------------------------------
                egui::CollapsingHeader::new("Gain")
                    .default_open(true)
                    .show(ui, |ui| {
                        let r = ui.add(
                            egui::Slider::new(&mut self.settings.gain_db, -20.0..=20.0)
                                .text("Gain (dB)")
                                .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }
                    });

                // --- High-pass filter -------------------------------------------
                egui::CollapsingHeader::new("High-pass filter")
                    .default_open(true)
                    .show(ui, |ui| {
                        let r = ui.add(
                            egui::Slider::new(
                                &mut self.settings.high_pass_cutoff_hz,
                                20.0..=500.0,
                            )
                            .text("Cutoff (Hz)")
                            .logarithmic(true)
                            .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }
                    });

                // --- Gate -------------------------------------------------------
                egui::CollapsingHeader::new("Gate")
                    .default_open(false)
                    .show(ui, |ui| {
                        let r = ui.add(
                            egui::Slider::new(&mut self.settings.gate_threshold_db, -80.0..=0.0)
                                .text("Threshold (dB)")
                                .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }

                        let r = ui.add(
                            egui::Slider::new(&mut self.settings.gate_attack_ms, 0.1..=100.0)
                                .text("Attack (ms)")
                                .logarithmic(true)
                                .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }

                        let r = ui.add(
                            egui::Slider::new(&mut self.settings.gate_hold_ms, 0.0..=500.0)
                                .text("Hold (ms)")
                                .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }

                        let r = ui.add(
                            egui::Slider::new(&mut self.settings.gate_release_ms, 1.0..=1000.0)
                                .text("Release (ms)")
                                .logarithmic(true)
                                .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }
                    });

                // --- EQ ---------------------------------------------------------
                egui::CollapsingHeader::new("EQ")
                    .default_open(false)
                    .show(ui, |ui| {
                        let band_labels = ["Low shelf", "Low-mid peak", "High-mid peak", "High shelf"];
                        let has_q = [false, true, true, false];

                        for i in 0..4 {
                            ui.label(band_labels[i]);

                            let r = ui.add(
                                egui::Slider::new(
                                    &mut self.settings.eq_band_freq_hz[i],
                                    20.0..=20_000.0,
                                )
                                .text("Freq (Hz)")
                                .logarithmic(true)
                                .clamping(egui::SliderClamping::Always),
                            );
                            if r.changed() { settings_changed = true; }

                            let r = ui.add(
                                egui::Slider::new(
                                    &mut self.settings.eq_band_gain_db[i],
                                    -18.0..=18.0,
                                )
                                .text("Gain (dB)")
                                .clamping(egui::SliderClamping::Always),
                            );
                            if r.changed() { settings_changed = true; }

                            if has_q[i] {
                                let r = ui.add(
                                    egui::Slider::new(
                                        &mut self.settings.eq_band_q[i],
                                        0.1..=10.0,
                                    )
                                    .text("Q")
                                    .logarithmic(true)
                                    .clamping(egui::SliderClamping::Always),
                                );
                                if r.changed() { settings_changed = true; }
                            } else {
                                ui.add_enabled(
                                    false,
                                    egui::Slider::new(
                                        &mut self.settings.eq_band_q[i],
                                        0.1..=10.0,
                                    )
                                    .text("Q (shelf)"),
                                );
                            }

                            if i < 3 {
                                ui.separator();
                            }
                        }
                    });

                // --- Compressor -------------------------------------------------
                egui::CollapsingHeader::new("Compressor")
                    .default_open(false)
                    .show(ui, |ui| {
                        let r = ui.add(
                            egui::Slider::new(
                                &mut self.settings.compressor_threshold_db,
                                -60.0..=0.0,
                            )
                            .text("Threshold (dB)")
                            .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }

                        let r = ui.add(
                            egui::Slider::new(&mut self.settings.compressor_ratio, 1.0..=20.0)
                                .text("Ratio")
                                .logarithmic(true)
                                .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }

                        let r = ui.add(
                            egui::Slider::new(
                                &mut self.settings.compressor_attack_ms,
                                0.1..=200.0,
                            )
                            .text("Attack (ms)")
                            .logarithmic(true)
                            .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }

                        let r = ui.add(
                            egui::Slider::new(
                                &mut self.settings.compressor_release_ms,
                                1.0..=1000.0,
                            )
                            .text("Release (ms)")
                            .logarithmic(true)
                            .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }

                        let r = ui.add(
                            egui::Slider::new(
                                &mut self.settings.compressor_makeup_db,
                                -20.0..=20.0,
                            )
                            .text("Makeup (dB)")
                            .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }
                    });

                // --- Limiter ----------------------------------------------------
                egui::CollapsingHeader::new("Limiter")
                    .default_open(false)
                    .show(ui, |ui| {
                        let r = ui.add(
                            egui::Slider::new(&mut self.settings.limiter_ceiling_db, -20.0..=0.0)
                                .text("Ceiling (dB)")
                                .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }

                        let r = ui.add(
                            egui::Slider::new(&mut self.settings.limiter_release_ms, 1.0..=500.0)
                                .text("Release (ms)")
                                .logarithmic(true)
                                .clamping(egui::SliderClamping::Always),
                        );
                        if r.changed() { settings_changed = true; }
                    });

                ui.add_space(6.0);

                // --- Preset load / save -----------------------------------------
                ui.horizontal(|ui| {
                    if ui.button("Load preset").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("TOML preset", &["toml"])
                            .pick_file()
                        {
                            match Settings::load(&path) {
                                Ok(s) => {
                                    self.settings = s;
                                    settings_changed = true;
                                    self.status_message =
                                        format!("Loaded: {}", path.display());
                                }
                                Err(e) => {
                                    self.status_message = format!("Load error: {e}");
                                }
                            }
                        }
                    }

                    if ui.button("Save preset").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("TOML preset", &["toml"])
                            .set_file_name("voice.toml")
                            .save_file()
                        {
                            match self.settings.save(&path) {
                                Ok(()) => {
                                    self.status_message =
                                        format!("Saved: {}", path.display());
                                }
                                Err(e) => {
                                    self.status_message = format!("Save error: {e}");
                                }
                            }
                        }
                    }

                    // --- Start on startup toggle --------------------------------
                    ui.separator();
                    let al = make_auto_launch();
                    let mut start_on_boot = al.is_enabled().unwrap_or(false);
                    if ui.checkbox(&mut start_on_boot, "Start on login").changed() {
                        if start_on_boot { al.enable().ok(); } else { al.disable().ok(); }
                    }

                    if !self.status_message.is_empty() {
                        ui.label(&self.status_message);
                    }
                });
            });
        });

        // Push updated settings to the audio engine and persist if anything changed.
        if settings_changed {
            self.engine.update_settings(self.settings.clone());
            self.save_config();
        }
    }
}
