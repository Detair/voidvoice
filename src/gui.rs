use eframe::egui;
use cpal::traits::{DeviceTrait, HostTrait};
use crate::audio::AudioEngine;
use crate::config::AppConfig;
use std::process::Command;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tray_icon::{TrayIconBuilder, TrayIcon, menu::{Menu, MenuItem, MenuEvent, MenuId}};
use tray_icon::Icon;
use crate::autostart;
use crate::updater::{self, UpdateInfo};
use crate::virtual_device;
use crate::pulse_info;

/// Runs the VoidMic GUI application.
/// 
/// # Arguments
/// * `model_path` - Path to the model directory (currently unused as RNNoise weights are embedded)
/// 
/// # Returns
/// Result indicating success or failure of the GUI application
pub fn run_gui(model_path: PathBuf) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([450.0, 450.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "VoidMic",
        options,
        Box::new(|cc| {
            setup_custom_style(&cc.egui_ctx);
            Box::new(VoidMicApp::new(model_path))
        }),
    )
}

fn setup_custom_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = egui::Color32::from_rgb(20, 20, 25);
    visuals.panel_fill = egui::Color32::from_rgb(20, 20, 25);
    ctx.set_visuals(visuals);
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
    tray_icon: Option<TrayIcon>, // Keep alive
    is_quitting: bool,
    is_calibrating: bool,
    update_receiver: Option<std::sync::mpsc::Receiver<Option<UpdateInfo>>>,
    update_info: Option<UpdateInfo>,
    virtual_sink_module_id: Option<u32>,
    connected_apps: Vec<String>,
    last_app_refresh: std::time::Instant,
}

const QUIT_ID: &str = "quit";
const SHOW_ID: &str = "show";

impl VoidMicApp {
    fn new(model_path: PathBuf) -> Self {
        // Tray Setup
        let tray_menu = Menu::new();
        let show_item = MenuItem::with_id(SHOW_ID, "Show/Hide", true, None);
        let quit_item = MenuItem::with_id(QUIT_ID, "Quit", true, None);
        let _ = tray_menu.append_items(&[&show_item, &quit_item]);

        let icon = generate_icon();
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("VoidMic")
            .with_icon(icon)
            .build()
            .ok();
        
        // Start async update check
        let update_receiver = Some(updater::check_for_updates_async());
        
        let config = AppConfig::load();
        let (inputs, outputs) = get_devices();
        
        let default_in = if inputs.contains(&config.last_input) {
            config.last_input.clone()
        } else {
            inputs.first().cloned().unwrap_or_else(|| "default".to_string())
        };

        let default_out = if outputs.contains(&config.last_output) {
            config.last_output.clone()
        } else {
            outputs.first().cloned().unwrap_or_else(|| "default".to_string())
        };

        Self {
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
            last_app_refresh: std::time::Instant::now(),
        }
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
}

impl eframe::App for VoidMicApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
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
            }
        }

        // Handle Close Request (Minimize to Tray)
        if ctx.input(|i| i.viewport().close_requested()) {
            if !self.is_quitting {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                // Cancel close by consuming the event? 
                // eframe doesn't have a specific "cancel close" but if we don't propagate or if we set Visible(false),
                // wait, close_requested means the OS sent a close signal.
                // In eframe, we need to return `false` in `on_close_request` hook maybe?
                // Or use `ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose)` if available?
                // `CancelClose` is not a standard command in some versions of eframe/egui.
                // However, we can use `frame.close()` only if we want to close.
                // Actually, `close_requested` is just info. We need to tell the viewport what to do.
                // In `eframe` 0.26, simply *not* calling close, and setting visible false works?
                // NO, we need to instruct the viewport to IGNORE the close.
                // `ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose)` exists in newer eframe.
                // Let's assume we can just set visibility to false.
                // BUT if we don't handle it, the window might close.
                // Let's try `ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose)` which is standard for "prevent default close".
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
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
            let mut dismiss_update = false;
            if let Some(ref update) = self.update_info {
                let version = update.version.clone();
                let url = update.download_url.clone();
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::GOLD, format!("🎉 Update available: {}", version));
                    if ui.small_button("Download").clicked() {
                        let _ = open::that(&url);
                    }
                    if ui.small_button("✕").clicked() {
                        dismiss_update = true;
                    }
                });
                ui.separator();
            }
            if dismiss_update {
                self.update_info = None;
            }
            
            ui.heading("VoidMic 🌌");
            ui.label(egui::RichText::new("Hybrid Noise Reduction (RNNoise + Gate)").size(10.0).weak());
            ui.separator();
            ui.add_space(10.0);

            // Meter with proper dB scaling
            let volume = if let Some(engine) = &self.engine {
                f32::from_bits(engine.volume_level.load(Ordering::Relaxed))
            } else {
                0.0
            };
            
            // Convert to dB for better visualization: dB = 20 * log10(amplitude)
            // Range: -60dB (silence) to 0dB (full scale)
            let db = if volume > 0.0001 {
                20.0 * volume.log10()
            } else {
                -60.0
            };
            
            // Normalize -60dB to 0dB range into 0.0 to 1.0 for progress bar
            let bar_len = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            
            let color = if volume > self.config.gate_threshold { egui::Color32::GREEN } else { egui::Color32::DARK_GRAY };
            
            ui.add(egui::ProgressBar::new(bar_len).fill(color).text(format!("{:.1} dB", db)));
            ui.label(egui::RichText::new("Green = Transmitting (Above Gate)").size(10.0));

            ui.add_space(20.0);

            // Selectors
            egui::Grid::new("device_grid").striped(true).show(ui, |ui| {
                ui.label("Microphone:");
                egui::ComboBox::from_id_source("input_combo")
                    .selected_text(&self.selected_input)
                    .width(250.0)
                    .show_ui(ui, |ui| {
                        let mut changed = false;
                        for dev in &self.input_devices {
                            if ui.selectable_value(&mut self.selected_input, dev.clone(), dev).changed() {
                                changed = true;
                            }
                        }
                        if changed { self.mark_config_dirty(); }
                    });
                ui.end_row();

                ui.label("Output Sink:");
                egui::ComboBox::from_id_source("output_combo")
                    .selected_text(&self.selected_output)
                    .width(250.0)
                    .show_ui(ui, |ui| {
                         let mut changed = false;
                        for dev in &self.output_devices {
                            if ui.selectable_value(&mut self.selected_output, dev.clone(), dev).changed() {
                                changed = true;
                            }
                        }
                        if changed { self.mark_config_dirty(); }
                    });
                ui.end_row();
            });

            ui.add_space(20.0);

            // Threshold Controls
            ui.horizontal(|ui| {
                ui.label("Gate Threshold:");
                let slider = egui::Slider::new(&mut self.config.gate_threshold, 0.005..=0.05)
                    .text("")
                    .fixed_decimals(3);
                if ui.add(slider).changed() {
                    self.mark_config_dirty();
                }
                
                let calibrate_enabled = self.engine.is_some() && !self.is_calibrating;
                if ui.add_enabled(calibrate_enabled, egui::Button::new("🎯 Calibrate")).clicked() {
                    if let Some(engine) = &self.engine {
                        engine.calibration_mode.store(true, Ordering::Relaxed);
                        self.is_calibrating = true;
                        self.status_msg = "Calibrating... stay quiet for 3 seconds".to_string();
                    }
                }
            });
            
            // Suppression Strength slider
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

            // Check calibration result
            if self.is_calibrating {
                if let Some(engine) = &self.engine {
                    if !engine.calibration_mode.load(Ordering::Relaxed) {
                        // Calibration finished
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
                    
                    match AudioEngine::start(&self.selected_input, &self.selected_output, &self.model_path, self.config.gate_threshold, self.config.suppression_strength) {
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
                     } else {
                         if let Err(e) = autostart::disable_autostart() {
                             self.status_msg = format!("Autostart error: {}", e);
                         } else {
                             self.status_msg = "Autostart disabled".to_string();
                         }
                     }
                     self.save_config_now();
                 }
            });
        });
    }
}

fn get_devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let inputs = host.input_devices().map(|devs| {
        devs.map(|d| d.name().unwrap_or("Unknown".to_string())).collect()
    }).unwrap_or_default();
    
    let outputs = host.output_devices().map(|devs| {
        devs.map(|d| d.name().unwrap_or("Unknown".to_string())).collect()
    }).unwrap_or_default();

    (inputs, outputs)
}

fn install_virtual_cable() -> Result<String, String> {
    if cfg!(target_os = "linux") {
        // Check if module is already loaded
        let check = Command::new("pactl")
            .args(&["list", "short", "sinks"])
            .output()
            .map_err(|e| format!("Failed to check sinks: {}. Is PulseAudio/PipeWire installed?", e))?;
        
        let output_str = String::from_utf8_lossy(&check.stdout);
        if output_str.contains("VoidMic_Clean") {
            return Ok("Virtual sink 'VoidMic_Clean' already exists.".to_string());
        }
        
        // Load the module
        let result = Command::new("pactl")
            .args(&[
                "load-module", 
                "module-null-sink", 
                "sink_name=VoidMic_Clean", 
                "sink_properties=device.description=VoidMic_Clean"
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
            if dx*dx + dy*dy < 14*14 {
                 rgba.extend_from_slice(&[0, 255, 0, 255]);
            } else {
                 rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Icon::from_rgba(rgba, width, height).unwrap_or_else(|_| Icon::from_rgba(vec![0; 32*32*4], 32, 32).unwrap())
}
