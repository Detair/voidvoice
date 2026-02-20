use crate::virtual_device;
use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui;
use std::process::Command;

use super::app::VoidMicApp;

impl VoidMicApp {
    /// Renders the device selection dropdowns.
    pub(super) fn render_device_selectors(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("device_grid").striped(true).show(ui, |ui| {
            ui.label("Microphone:");
            egui::ComboBox::from_id_salt("input_combo")
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
            egui::ComboBox::from_id_salt("output_combo")
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

        ui.add_space(10.0);

        // One-Click Setup Section
        if self.last_sink_check.elapsed().as_secs() >= 5 {
            self.virtual_sink_cached = virtual_device::virtual_sink_exists();
            self.last_sink_check = std::time::Instant::now();
        }
        ui.horizontal(|ui| {
            let sink_exists = self.virtual_sink_cached;

            if sink_exists {
                ui.colored_label(egui::Color32::GREEN, "✔ Virtual Mic Active");
                if ui.button("Destroy").clicked() {
                    if let Some(id) = self.virtual_sink_module_id {
                        let _ = virtual_device::destroy_virtual_sink(id);
                    } else {
                        let _ = virtual_device::destroy_virtual_sink(0);
                    }
                    self.virtual_sink_module_id = None;
                    let (inputs, outputs) = get_devices();
                    self.input_devices = inputs;
                    self.output_devices = outputs;
                }
                ui.label(egui::RichText::new("ℹ️ Select 'VoidMic_Clean' in Discord").size(10.0));
            } else if ui
                .button("✨ Create Virtual Mic")
                .on_hover_text("Creates a virtual device for Discord/Zoom")
                .clicked()
            {
                match virtual_device::create_virtual_sink() {
                    Ok(device) => {
                        self.virtual_sink_module_id = Some(device.module_id);
                        let (inputs, outputs) = get_devices();
                        self.input_devices = inputs;
                        self.output_devices = outputs;
                        if self.output_devices.contains(&device.sink_name) {
                            self.selected_output = device.sink_name;
                            self.mark_config_dirty();
                        }
                        self.status_msg = "Virtual Mic Created!".to_string();
                    }
                    Err(e) => {
                        self.status_msg = format!("Failed to create sink: {}", e);
                    }
                }
            }
        });
    }
}

pub(super) fn get_devices() -> (Vec<String>, Vec<String>) {
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

pub(super) fn install_virtual_cable() -> Result<String, String> {
    if cfg!(target_os = "linux") {
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
