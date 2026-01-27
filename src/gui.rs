use crate::audio::{AudioEngine, OutputFilterEngine};
use crate::autostart;
use crate::config::AppConfig;
use crate::pulse_info;
use crate::updater::{self, UpdateInfo};
use crate::virtual_device;
use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;
use tray_icon::Icon;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIcon, TrayIconBuilder,
};

/// Runs the VoidMic GUI application.
///
/// # Arguments
/// * `model_path` - Path to the model directory (currently unused as RNNoise weights are embedded)
///
/// # Returns
/// Result indicating success or failure of the GUI application
pub fn run_gui(model_path: PathBuf) -> eframe::Result<()> {
    // Load config early to determine if we should start minimized
    let config = AppConfig::load();
    let start_minimized = config.start_minimized;
    let dark_mode = config.dark_mode;

    // Build viewport with saved position if available
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([450.0, 450.0])
        .with_resizable(false)
        .with_visible(!start_minimized);

    if let (Some(x), Some(y)) = (config.window_x, config.window_y) {
        viewport = viewport.with_position([x, y]);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "VoidMic",
        options,
        Box::new(move |cc| {
            setup_custom_style(&cc.egui_ctx, dark_mode);
            Box::new(VoidMicApp::new_with_config(model_path, config))
        }),
    )
}

fn setup_custom_style(ctx: &egui::Context, dark_mode: bool) {
    if dark_mode {
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = egui::Color32::from_rgb(20, 20, 25);
        visuals.panel_fill = egui::Color32::from_rgb(20, 20, 25);
        ctx.set_visuals(visuals);
    } else {
        ctx.set_visuals(egui::Visuals::light());
    }
}

struct VoidMicApp {
    input_devices: Vec<String>,
    output_devices: Vec<String>,
    selected_input: String,
    selected_output: String,
    engine: Option<AudioEngine>,
    status_msg: String,
    model_path: PathBuf,
    config: AppConfig,
    config_dirty: bool,
    #[allow(dead_code)] // Kept alive for tray icon
    tray_icon: Option<TrayIcon>,
    is_quitting: bool,
    is_calibrating: bool,
    update_receiver: Option<std::sync::mpsc::Receiver<Option<UpdateInfo>>>,
    update_info: Option<UpdateInfo>,
    virtual_sink_module_id: Option<u32>,
    connected_apps: Vec<String>,
    last_app_refresh: std::time::Instant,
    // Output Filter (Speaker Denoising)
    output_filter_engine: Option<OutputFilterEngine>,
    // Echo Cancellation
    selected_reference: String,
}

const QUIT_ID: &str = "quit";
const SHOW_ID: &str = "show";
const TOGGLE_ID: &str = "toggle";

impl VoidMicApp {
    fn new_with_config(model_path: PathBuf, config: AppConfig) -> Self {
        // Tray Setup
        let tray_menu = Menu::new();
        let toggle_item = MenuItem::with_id(TOGGLE_ID, "Enable", true, None);
        let show_item = MenuItem::with_id(SHOW_ID, "Show/Hide", true, None);
        let quit_item = MenuItem::with_id(QUIT_ID, "Quit", true, None);
        let _ = tray_menu.append_items(&[&toggle_item, &show_item, &quit_item]);

        let icon = generate_icon();
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("VoidMic")
            .with_icon(icon)
            .build()
            .ok();

        // Start async update check
        let update_receiver = Some(updater::check_for_updates_async());

        let (inputs, outputs) = get_devices();

        let default_in = if inputs.contains(&config.last_input) {
            config.last_input.clone()
        } else {
            inputs
                .first()
                .cloned()
                .unwrap_or_else(|| "default".to_string())
        };

        let default_out = if outputs.contains(&config.last_output) {
            config.last_output.clone()
        } else {
            outputs
                .first()
                .cloned()
                .unwrap_or_else(|| "default".to_string())
        };

        let default_ref = inputs
            .first()
            .cloned()
            .unwrap_or_else(|| "default".to_string());

        let auto_start = config.auto_start_processing;

        let mut app = Self {
            input_devices: inputs,
            output_devices: outputs,
            selected_input: default_in,
            selected_output: default_out,
            engine: None,
            status_msg: "Ready".to_string(),
            model_path,
            config,
            config_dirty: false,
            tray_icon,
            is_quitting: false,
            is_calibrating: false,
            update_receiver,
            update_info: None,
            virtual_sink_module_id: None,
            connected_apps: Vec::new(),
            output_filter_engine: None,
            last_app_refresh: std::time::Instant::now(),
            selected_reference: default_ref,
        };

        // Auto-start processing if enabled
        if auto_start {
            app.start_engine();
        }

        app
    }

    fn start_engine(&mut self) {
        if self.engine.is_some() {
            return; // Already running
        }

        match AudioEngine::start(
            &self.selected_input,
            &self.selected_output,
            &self.model_path,
            self.config.gate_threshold,
            self.config.suppression_strength,
            self.config.dynamic_threshold_enabled,
            None,
            self.config.echo_cancel_enabled,
        ) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.status_msg = "Running".to_string();
            }
            Err(e) => {
                self.status_msg = format!("Error: {}", e);
                log::error!("Failed to start engine: {}", e);
            }
        }
    }

    fn stop_engine(&mut self) {
        self.engine = None;
        self.status_msg = "Stopped".to_string();
    }

    fn mark_config_dirty(&mut self) {
        self.config_dirty = true;
    }

    fn save_config(&mut self) {
        if self.config_dirty {
            self.config.last_input = self.selected_input.clone();
            self.config.last_output = self.selected_output.clone();
            // gate_threshold is already in config from slider updates
            self.config.save();
            self.config_dirty = false;
        }
    }

    fn save_config_now(&mut self) {
        self.config.last_input = self.selected_input.clone();
        self.config.last_output = self.selected_output.clone();
        self.config.save();
    }

    /// Renders the update banner at the top of the UI.
    /// Returns true if the update was dismissed.
    fn render_update_banner(&mut self, ui: &mut egui::Ui) -> bool {
        let mut dismiss = false;
        if let Some(ref update) = self.update_info {
            let version = update.version.clone();
            let url = update.download_url.clone();
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::GOLD,
                    format!("🎉 Update available: {}", version),
                );
                if ui.small_button("Download").clicked() {
                    let _ = open::that(&url);
                }
                if ui.small_button("✕").clicked() {
                    dismiss = true;
                }
            });
            ui.separator();
        }
        dismiss
    }

    /// Renders the volume meter with dB scaling.
    fn render_volume_meter(&self, ui: &mut egui::Ui) {
        let volume = if let Some(engine) = &self.engine {
            f32::from_bits(engine.volume_level.load(Ordering::Relaxed))
        } else {
            0.0
        };

        let db = if volume > 0.0001 {
            20.0 * volume.log10()
        } else {
            -60.0
        };
        let bar_len = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let color = if volume > self.config.gate_threshold {
            egui::Color32::GREEN
        } else {
            egui::Color32::DARK_GRAY
        };

        ui.add(
            egui::ProgressBar::new(bar_len)
                .fill(color)
                .text(format!("{:.1} dB", db)),
        );
        ui.label(egui::RichText::new("Green = Transmitting (Above Gate)").size(10.0));
    }

    /// Renders the device selection dropdowns.
    fn render_device_selectors(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("device_grid").striped(true).show(ui, |ui| {
            ui.label("Microphone:");
            egui::ComboBox::from_id_source("input_combo")
                .selected_text(&self.selected_input)
                .width(250.0)
                .show_ui(ui, |ui| {
                    let mut changed = false;
                    for dev in &self.input_devices {
                        if ui
                            .selectable_value(&mut self.selected_input, dev.clone(), dev)
                            .changed()
                        {
                            changed = true;
                        }
                    }
                    if changed {
                        self.mark_config_dirty();
                    }
                });
            ui.end_row();

            ui.label("Output Sink:");
            egui::ComboBox::from_id_source("output_combo")
                .selected_text(&self.selected_output)
                .width(250.0)
                .show_ui(ui, |ui| {
                    let mut changed = false;
                    for dev in &self.output_devices {
                        if ui
                            .selectable_value(&mut self.selected_output, dev.clone(), dev)
                            .changed()
                        {
                            changed = true;
                        }
                    }
                    if changed {
                        self.mark_config_dirty();
                    }
                });
            ui.end_row();
        });
    }

    /// Renders the threshold and suppression controls.
    fn render_threshold_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.config.dynamic_threshold_enabled, "Auto-Gate")
                .changed()
            {
                self.mark_config_dirty();
            }

            ui.add_enabled_ui(!self.config.dynamic_threshold_enabled, |ui| {
                ui.label("Gate Threshold:");
                let slider = egui::Slider::new(&mut self.config.gate_threshold, 0.005..=0.05)
                    .text("")
                    .fixed_decimals(3);
                if ui.add(slider).changed() {
                    self.mark_config_dirty();
                }
            });

            let calibrate_enabled = self.engine.is_some()
                && !self.is_calibrating
                && !self.config.dynamic_threshold_enabled;
            if ui
                .add_enabled(calibrate_enabled, egui::Button::new("🎯 Calibrate"))
                .clicked()
            {
                if let Some(engine) = &self.engine {
                    engine.calibration_mode.store(true, Ordering::Relaxed);
                    self.is_calibrating = true;
                    self.status_msg = "Calibrating... stay quiet for 3 seconds".to_string();
                }
            }
        });

        ui.horizontal(|ui| {
            ui.label("Suppression:");
            let pct = (self.config.suppression_strength * 100.0) as i32;
            let slider = egui::Slider::new(&mut self.config.suppression_strength, 0.0..=1.0)
                .text(format!("{}%", pct))
                .fixed_decimals(0);
            if ui.add(slider).changed() {
                self.mark_config_dirty();
            }
        });
    }

    /// Checks and handles calibration results.
    fn check_calibration_result(&mut self) {
        if self.is_calibrating {
            if let Some(engine) = &self.engine {
                if !engine.calibration_mode.load(Ordering::Relaxed) {
                    let result = f32::from_bits(engine.calibration_result.load(Ordering::Relaxed));
                    if result > 0.0 {
                        self.config.gate_threshold = result;
                        self.save_config_now();
                        self.status_msg = format!("Calibrated! Threshold set to {:.3}", result);
                    }
                    self.is_calibrating = false;
                }
            }
        }
    }

    /// Renders advanced features (output filter, echo cancellation).
    fn render_advanced_features(&mut self, ui: &mut egui::Ui) {
        ui.heading("Advanced Features");

        ui.horizontal(|ui| {
            if ui
                .checkbox(
                    &mut self.config.output_filter_enabled,
                    "Filter Output (Speaker Denoising)",
                )
                .changed()
            {
                self.mark_config_dirty();
            }
            ui.label(
                egui::RichText::new("⚠️ ~100ms latency")
                    .size(10.0)
                    .color(egui::Color32::YELLOW),
            );
        });

        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.config.echo_cancel_enabled, "Echo Cancellation")
                .changed()
            {
                self.mark_config_dirty();
            }
        });

        if self.config.echo_cancel_enabled || self.config.output_filter_enabled {
            ui.horizontal(|ui| {
                ui.label("Reference Input (Monitor):");
                egui::ComboBox::from_id_source("ref_combo")
                    .selected_text(&self.selected_reference)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for dev in &self.input_devices {
                            let _ =
                                ui.selectable_value(&mut self.selected_reference, dev.clone(), dev);
                        }
                    });
                ui.label(egui::RichText::new("ℹ️ Select speaker monitor").size(10.0));
            });
        }
    }
}

impl eframe::App for VoidMicApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle Tray Events
        if let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id.0 == QUIT_ID {
                self.is_quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if event.id.0 == SHOW_ID {
                // Toggle visibility or just show
                // We can't easily check current visibility state from here without tracking it,
                // but we can just force visible for now or toggle logic if we track it.
                // For simplicity, let's just ensure it is visible and focused.
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else if event.id.0 == TOGGLE_ID {
                // Toggle engine on/off
                if self.engine.is_some() {
                    self.stop_engine();
                    // Update tray tooltip
                    if let Some(ref tray) = self.tray_icon {
                        let _ = tray.set_tooltip(Some("VoidMic - Disabled"));
                    }
                } else {
                    self.start_engine();
                    // Update tray tooltip
                    if let Some(ref tray) = self.tray_icon {
                        let _ = tray.set_tooltip(Some("VoidMic - Active"));
                    }
                }
            }
        }

        // Handle Close Request (Minimize to Tray)
        if ctx.input(|i| i.viewport().close_requested()) && !self.is_quitting {
            // Save window position before hiding
            if let Some(pos) = ctx.input(|i| i.viewport().outer_rect).map(|r| r.min) {
                self.config.window_x = Some(pos.x);
                self.config.window_y = Some(pos.y);
                self.save_config_now();
            }

            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }

        ctx.request_repaint();

        // Check for update result from async receiver
        if let Some(ref rx) = self.update_receiver {
            if let Ok(update) = rx.try_recv() {
                self.update_info = update;
                self.update_receiver = None; // Consumed
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // Update banner at top
            if self.render_update_banner(ui) {
                self.update_info = None;
            }

            ui.heading("VoidMic 🌌");
            ui.label(egui::RichText::new("Hybrid Noise Reduction (RNNoise + Gate)").size(10.0).weak());
            ui.separator();
            ui.add_space(10.0);

            // Volume meter
            self.render_volume_meter(ui);

            ui.add_space(20.0);

            // Device selectors
            self.render_device_selectors(ui);

            ui.add_space(20.0);

            // Threshold and suppression controls
            self.render_threshold_controls(ui);

            // Check calibration result
            self.check_calibration_result();

            // Advanced Features
            ui.add_space(10.0);
            self.render_advanced_features(ui);

            ui.add_space(10.0);

            // Connected Apps display (refresh every 2 seconds)
            #[cfg(target_os = "linux")]
            {
                if self.engine.is_some() && self.last_app_refresh.elapsed().as_secs() >= 2 {
                    self.connected_apps = pulse_info::get_connected_apps()
                        .into_iter()
                        .map(|a| a.name)
                        .collect();
                    self.last_app_refresh = std::time::Instant::now();
                }

                if !self.connected_apps.is_empty() {
                    ui.add_space(10.0);
                    egui::CollapsingHeader::new(format!("📱 Connected Apps ({})", self.connected_apps.len()))
                        .default_open(true)
                        .show(ui, |ui| {
                            for app in &self.connected_apps {
                                ui.label(format!("  • {}", app));
                            }
                        });
                }
            }

            let is_running = self.engine.is_some();
            let btn_text = if is_running { "STOP ENGINE" } else { "ACTIVATE VOIDMIC" };

            let btn = ui.add_sized([ui.available_width(), 50.0], egui::Button::new(
                egui::RichText::new(btn_text).size(18.0).strong()
            ));

            if btn.clicked() {
                if is_running {
                    self.engine = None;
                    self.output_filter_engine = None;
                    self.status_msg = "Stopped".to_string();
                } else {
                    self.status_msg = "Initializing Hybrid Engine...".to_string();

                    // Auto-create virtual sink on Linux
                    #[cfg(target_os = "linux")]
                    {
                        if self.virtual_sink_module_id.is_none() {
                            match virtual_device::create_virtual_sink() {
                                Ok(device) => {
                                    self.virtual_sink_module_id = Some(device.module_id);
                                    // Refresh device list and auto-select virtual sink
                                    let (inputs, outputs) = get_devices();
                                    self.input_devices = inputs;
                                    self.output_devices = outputs.clone();
                                    // Auto-select virtual sink as output if available
                                    if outputs.iter().any(|d| d.contains("VoidMic_Clean")) {
                                        self.selected_output = outputs.iter()
                                            .find(|d| d.contains("VoidMic_Clean"))
                                            .cloned()
                                            .unwrap_or(self.selected_output.clone());
                                    }
                                    self.status_msg = "Virtual sink created".to_string();
                                }
                                Err(e) => {
                                    self.status_msg = format!("Virtual sink warning: {}", e);
                                    // Continue anyway, user may have manually set up
                                }
                            }
                        }
                    }


                    // Start Output Filter if enabled
                    if self.config.output_filter_enabled {
                         match OutputFilterEngine::start(&self.selected_reference, &self.selected_output, self.config.suppression_strength) {
                             Ok(engine) => self.output_filter_engine = Some(engine),
                             Err(e) => {
                                 self.status_msg = format!("Output Filter Error: {}", e);
                                 // Don't abort main engine?
                             }
                         }
                    }

                    match AudioEngine::start(
                        &self.selected_input,
                        &self.selected_output,
                        &self.model_path,
                        self.config.gate_threshold,
                        self.config.suppression_strength,
                        self.config.echo_cancel_enabled,
                        if self.config.echo_cancel_enabled { Some(&self.selected_reference) } else { None },
                        self.config.dynamic_threshold_enabled
                    ) {
                        Ok(engine) => {
                            self.engine = Some(engine);
                            self.status_msg = "Active (RNNoise + Gate)".to_string();
                            self.save_config();
                        }
                        Err(e) => {
                            // Provide actionable error messages
                            let error_str = e.to_string();
                            self.status_msg = if error_str.contains("No default") {
                                "Error: No audio device found. Check your system settings.".to_string()
                            } else if error_str.contains("not found") {
                                "Error: Selected device not found. Try refreshing or selecting another device.".to_string()
                            } else if error_str.contains("permission") || error_str.contains("access") {
                                "Error: Permission denied. Check audio device permissions.".to_string()
                            } else if error_str.contains("in use") || error_str.contains("busy") {
                                "Error: Device is busy. Close other audio applications.".to_string()
                            } else {
                                format!("Error: {}", e)
                            };
                        }
                    }
                }
            }

            ui.add_space(10.0);
            ui.label(format!("Status: {}", self.status_msg));

             ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                 ui.horizontal(|ui| {
                    if ui.button("🛠️ Install Virtual Cable").clicked() {
                        match install_virtual_cable() {
                            Ok(msg) => {
                                self.status_msg = msg;
                                // Refresh device list
                                let (inputs, outputs) = get_devices();
                                self.input_devices = inputs;
                                self.output_devices = outputs;
                            }
                            Err(e) => {
                                self.status_msg = format!("Virtual Cable Error: {}", e);
                            }
                        }
                    }
                 });
                 ui.separator();

                 // Start on Boot checkbox
                 let mut start_on_boot = self.config.start_on_boot;
                 if ui.checkbox(&mut start_on_boot, "Start on Boot").changed() {
                     self.config.start_on_boot = start_on_boot;
                     if start_on_boot {
                         if let Err(e) = autostart::enable_autostart() {
                             self.status_msg = format!("Autostart error: {}", e);
                             self.config.start_on_boot = false;
                         } else {
                             self.status_msg = "Autostart enabled".to_string();
                         }
                     } else if let Err(e) = autostart::disable_autostart() {
                         self.status_msg = format!("Autostart error: {}", e);
                     } else {
                         self.status_msg = "Autostart disabled".to_string();
                     }
                     self.save_config_now();
                 }

                 // Start Minimized checkbox
                 let mut start_minimized = self.config.start_minimized;
                 if ui.checkbox(&mut start_minimized, "Start Minimized to Tray").changed() {
                     self.config.start_minimized = start_minimized;
                     self.save_config_now();
                 }

                 // Auto-Start Processing checkbox
                 let mut auto_start = self.config.auto_start_processing;
                 if ui.checkbox(&mut auto_start, "Auto-Start Processing").changed() {
                     self.config.auto_start_processing = auto_start;
                     self.save_config_now();
                 }

                 // Dark Mode checkbox
                 let mut dark_mode = self.config.dark_mode;
                 if ui.checkbox(&mut dark_mode, "Dark Mode").changed() {
                     self.config.dark_mode = dark_mode;
                     self.save_config_now();
                     // Apply theme change immediately
                     if dark_mode {
                         let mut visuals = egui::Visuals::dark();
                         visuals.window_fill = egui::Color32::from_rgb(20, 20, 25);
                         visuals.panel_fill = egui::Color32::from_rgb(20, 20, 25);
                         ui.ctx().set_visuals(visuals);
                     } else {
                         ui.ctx().set_visuals(egui::Visuals::light());
                     }
                 }
            });
        });
    }
}

fn get_devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let inputs = host
        .input_devices()
        .map(|devs| {
            devs.map(|d| d.name().unwrap_or("Unknown".to_string()))
                .collect()
        })
        .unwrap_or_default();

    let outputs = host
        .output_devices()
        .map(|devs| {
            devs.map(|d| d.name().unwrap_or("Unknown".to_string()))
                .collect()
        })
        .unwrap_or_default();

    (inputs, outputs)
}

fn install_virtual_cable() -> Result<String, String> {
    if cfg!(target_os = "linux") {
        // Check if module is already loaded
        let check = Command::new("pactl")
            .args(["list", "short", "sinks"])
            .output()
            .map_err(|e| {
                format!(
                    "Failed to check sinks: {}. Is PulseAudio/PipeWire installed?",
                    e
                )
            })?;

        let output_str = String::from_utf8_lossy(&check.stdout);
        if output_str.contains("VoidMic_Clean") {
            return Ok("Virtual sink 'VoidMic_Clean' already exists.".to_string());
        }

        // Load the module
        let result = Command::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                "sink_name=VoidMic_Clean",
                "sink_properties=device.description=VoidMic_Clean",
            ])
            .output()
            .map_err(|e| format!("Failed to create sink: {}", e))?;

        if result.status.success() {
            Ok("Virtual sink 'VoidMic_Clean' created! Select 'Monitor of VoidMic_Clean' in your apps.".to_string())
        } else {
            let stderr = String::from_utf8_lossy(&result.stderr);
            Err(format!("pactl failed: {}", stderr))
        }
    } else if cfg!(target_os = "windows") {
        open::that("https://vb-audio.com/Cable/")
            .map_err(|e| format!("Failed to open browser: {}", e))?;
        Ok("Opening VB-Cable download page...".to_string())
    } else if cfg!(target_os = "macos") {
        open::that("https://github.com/ExistentialAudio/BlackHole")
            .map_err(|e| format!("Failed to open browser: {}", e))?;
        Ok("Opening BlackHole download page...".to_string())
    } else {
        Err("Unsupported platform".to_string())
    }
}
fn generate_icon() -> Icon {
    let width = 32;
    let height = 32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            // Simple green circle
            let dx = (x as i32) - 16;
            let dy = (y as i32) - 16;
            if dx * dx + dy * dy < 14 * 14 {
                rgba.extend_from_slice(&[0, 255, 0, 255]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Icon::from_rgba(rgba, width, height)
        .unwrap_or_else(|_| Icon::from_rgba(vec![0; 32 * 32 * 4], 32, 32).unwrap())
}
