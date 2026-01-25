use eframe::egui;
use cpal::traits::{DeviceTrait, HostTrait};
use crate::audio::AudioEngine;
use crate::config::AppConfig;
use std::process::Command;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

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
    config_dirty: bool,  // Track if config needs saving
}

impl VoidMicApp {
    fn new(model_path: PathBuf) -> Self {
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
        }
    }

    fn mark_config_dirty(&mut self) {
        self.config_dirty = true;
    }

    fn save_config(&mut self) {
        if self.config_dirty {
            self.config.last_input = self.selected_input.clone();
            self.config.last_output = self.selected_output.clone();
            self.config.save();
            self.config_dirty = false;
        }
    }
}

impl eframe::App for VoidMicApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint();

        egui::CentralPanel::default().show(ctx, |ui| {
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
            
            let color = if volume > 0.015 { egui::Color32::GREEN } else { egui::Color32::DARK_GRAY };
            
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
                    match AudioEngine::start(&self.selected_input, &self.selected_output, &self.model_path) {
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