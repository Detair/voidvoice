use eframe::egui;
use std::sync::atomic::Ordering;

use super::app::VoidMicApp;

pub(super) struct Preset {
    pub name: &'static str,
    gate_threshold: f32,
    suppression_strength: f32,
    dynamic_threshold_enabled: bool,
}

pub(super) const PRESETS: &[Preset] = &[
    Preset {
        name: "Standard",
        gate_threshold: 0.015,
        suppression_strength: 1.0,
        dynamic_threshold_enabled: true,
    },
    Preset {
        name: "Gaming",
        gate_threshold: 0.030,
        suppression_strength: 1.0,
        dynamic_threshold_enabled: true,
    },
    Preset {
        name: "Podcast",
        gate_threshold: 0.008,
        suppression_strength: 0.6,
        dynamic_threshold_enabled: true,
    },
    Preset {
        name: "Noisy Office",
        gate_threshold: 0.020,
        suppression_strength: 1.0,
        dynamic_threshold_enabled: true,
    },
    Preset {
        name: "Music",
        gate_threshold: 0.002,
        suppression_strength: 0.3,
        dynamic_threshold_enabled: false,
    },
];

impl VoidMicApp {
    pub(super) fn apply_preset(&mut self, preset_name: &str) {
        if let Some(preset) = PRESETS.iter().find(|p| p.name == preset_name) {
            self.config.gate_threshold = preset.gate_threshold;
            self.config.suppression_strength = preset.suppression_strength;
            self.config.dynamic_threshold_enabled = preset.dynamic_threshold_enabled;
            self.config.preset = preset_name.to_string();
            self.save_config_now();

            // Update running engine immediately
            if let Some(engine) = &self.engine {
                engine.gate_threshold.store(self.config.gate_threshold.to_bits(), Ordering::Relaxed);
                engine.suppression_strength.store(self.config.suppression_strength.to_bits(), Ordering::Relaxed);
                engine.dynamic_threshold_enabled.store(self.config.dynamic_threshold_enabled, Ordering::Relaxed);
            }
        }
    }

    /// Renders the threshold and suppression controls.
    pub(super) fn render_threshold_controls(&mut self, ui: &mut egui::Ui) {
        // Presets Dropdown
        ui.horizontal(|ui| {
            ui.label("Preset:");
            egui::ComboBox::from_id_salt("preset_combo")
                .selected_text(&self.config.preset)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.config.preset == "Custom", "Custom")
                        .clicked()
                    {
                        self.config.preset = "Custom".to_string();
                        self.save_config_now();
                    }
                    ui.separator();
                    for preset in PRESETS {
                        if ui
                            .selectable_label(self.config.preset == preset.name, preset.name)
                            .clicked()
                        {
                            self.apply_preset(preset.name);
                        }
                    }
                });
        });

        ui.add_space(5.0);

        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.config.dynamic_threshold_enabled, "Auto-Gate")
                .on_hover_text("Automatically adjusts gate based on ambient noise floor")
                .changed()
            {
                self.config.preset = "Custom".to_string();
                self.mark_config_dirty();
                if let Some(engine) = &self.engine {
                    engine.dynamic_threshold_enabled.store(self.config.dynamic_threshold_enabled, Ordering::Relaxed);
                }
            }

            ui.add_enabled_ui(!self.config.dynamic_threshold_enabled, |ui| {
                ui.label("Gate Threshold:");
                let slider = egui::Slider::new(&mut self.config.gate_threshold, 0.005..=0.05)
                    .text("")
                    .fixed_decimals(3);
                if ui.add(slider).changed() {
                    self.config.preset = "Custom".to_string();
                    self.mark_config_dirty();
                    if let Some(engine) = &self.engine {
                        engine.gate_threshold.store(self.config.gate_threshold.to_bits(), Ordering::Relaxed);
                    }
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
                self.config.preset = "Custom".to_string();
                self.mark_config_dirty();
                if let Some(engine) = &self.engine {
                    engine.suppression_strength.store(self.config.suppression_strength.to_bits(), Ordering::Relaxed);
                }
                if let Some(filter) = &self.output_filter_engine {
                    filter.suppression_strength.store(self.config.suppression_strength.to_bits(), Ordering::Relaxed);
                }
            }
        });
    }
}
