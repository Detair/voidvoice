use crate::audio::OutputFilterEngine;
use eframe::egui;
use std::sync::atomic::Ordering;

use super::app::VoidMicApp;

impl VoidMicApp {
    /// Renders advanced features (output filter, echo cancellation, VAD, EQ, AGC, bypass, spectrum).
    pub(super) fn render_advanced_features(&mut self, ui: &mut egui::Ui) {
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
                if self.config.output_filter_enabled {
                    if self.engine.is_some() && self.output_filter_engine.is_none() {
                        match OutputFilterEngine::start(
                            &self.selected_reference,
                            &self.selected_output,
                            self.config.suppression_strength,
                        ) {
                            Ok(filter) => self.output_filter_engine = Some(filter),
                            Err(e) => {
                                log::error!("Output filter failed to start: {}", e);
                                self.status_msg = format!("Output filter error: {}", e);
                                self.config.output_filter_enabled = false;
                            }
                        }
                    }
                } else {
                    self.output_filter_engine = None;
                }
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
                if self.engine.is_some() {
                    let prev_echo = !self.config.echo_cancel_enabled;
                    self.stop_engine();
                    self.start_engine();
                    if self.engine.is_none() {
                        self.config.echo_cancel_enabled = prev_echo;
                    }
                }
            }
        });

        if self.config.echo_cancel_enabled || self.config.output_filter_enabled {
            ui.horizontal(|ui| {
                ui.label("Reference Input (Monitor):");
                let prev_ref = self.selected_reference.clone();
                egui::ComboBox::from_id_salt("ref_combo")
                    .selected_text(&self.selected_reference)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for dev in &self.input_devices {
                            let _ =
                                ui.selectable_value(&mut self.selected_reference, dev.clone(), dev);
                        }
                    });
                if self.selected_reference != prev_ref {
                    self.mark_config_dirty();
                }
                ui.label(egui::RichText::new("ℹ️ Select speaker monitor").size(10.0));
            });
        }

        ui.separator();

        // VAD Controls
        const VAD_MODES: &[(i32, &str, &str)] = &[
            (0, "Quality", "Quality (Likely Speech)"),
            (1, "Low Bitrate", "Low Bitrate"),
            (2, "Aggressive", "Aggressive"),
            (3, "Very Aggressive", "Very Aggressive"),
        ];
        ui.horizontal(|ui| {
            ui.label("VAD Sensitivity:");
            let current_label = VAD_MODES
                .iter()
                .find(|(v, _, _)| *v == self.config.vad_sensitivity)
                .map(|(_, _, full)| *full)
                .unwrap_or("Unknown");
            egui::ComboBox::from_id_salt("vad_combo")
                .selected_text(current_label)
                .show_ui(ui, |ui| {
                    for (value, label, _) in VAD_MODES {
                        if ui
                            .selectable_value(&mut self.config.vad_sensitivity, *value, *label)
                            .clicked()
                        {
                            self.mark_config_dirty();
                            if let Some(engine) = &self.engine {
                                engine
                                    .vad_sensitivity
                                    .store(self.config.vad_sensitivity as u32, Ordering::Relaxed);
                            }
                        }
                    }
                });
            ui.label(egui::RichText::new("ℹ️ WebRTC VAD").size(10.0))
                .on_hover_text("Voice Activity Detection - filters non-speech sounds");
        });

        ui.separator();

        // Equalizer Controls
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.config.eq_enabled, "Equalizer (3-Band)")
                .changed()
            {
                self.mark_config_dirty();
                if let Some(engine) = &self.engine {
                    engine.eq_enabled.store(self.config.eq_enabled, Ordering::Relaxed);
                }
            }
        });

        if self.config.eq_enabled {
            egui::Grid::new("eq_grid").num_columns(2).show(ui, |ui| {
                ui.label("Low (Bass):");
                if ui
                    .add(egui::Slider::new(&mut self.config.eq_low_gain, -10.0..=10.0).text("dB"))
                    .changed()
                {
                    self.mark_config_dirty();
                    if let Some(engine) = &self.engine {
                        engine
                            .eq_low_gain
                            .store(self.config.eq_low_gain.to_bits(), Ordering::Relaxed);
                    }
                }
                ui.end_row();

                ui.label("Mid (Voice):");
                if ui
                    .add(egui::Slider::new(&mut self.config.eq_mid_gain, -10.0..=10.0).text("dB"))
                    .changed()
                {
                    self.mark_config_dirty();
                    if let Some(engine) = &self.engine {
                        engine
                            .eq_mid_gain
                            .store(self.config.eq_mid_gain.to_bits(), Ordering::Relaxed);
                    }
                }
                ui.end_row();

                ui.label("High (Treble):");
                if ui
                    .add(egui::Slider::new(&mut self.config.eq_high_gain, -10.0..=10.0).text("dB"))
                    .changed()
                {
                    self.mark_config_dirty();
                    if let Some(engine) = &self.engine {
                        engine
                            .eq_high_gain
                            .store(self.config.eq_high_gain.to_bits(), Ordering::Relaxed);
                    }
                }
                ui.end_row();
            });
        }

        // AGC + Bypass
        ui.separator();

        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.config.agc_enabled, "Automatic Gain Control (AGC)")
                .on_hover_text("Normalizes volume to prevent clipping and boost quiet speech")
                .changed()
            {
                self.mark_config_dirty();
                if let Some(engine) = &self.engine {
                    engine
                        .agc_enabled
                        .store(self.config.agc_enabled, Ordering::Relaxed);
                }
            }
        });

        ui.add_space(5.0);

        // BIG BYPASS BUTTON
        let bypass_enabled = if let Some(engine) = &self.engine {
            engine.bypass_enabled.load(Ordering::Relaxed)
        } else {
            false
        };
        let bypass_text = if bypass_enabled {
            "🔴 BYPASSED (Raw Audio)"
        } else {
            "🟢 Processing Active"
        };
        if ui
            .add_sized(
                [ui.available_width(), 30.0],
                egui::Button::new(egui::RichText::new(bypass_text).strong().size(14.0)).fill(
                    if bypass_enabled {
                        egui::Color32::DARK_RED
                    } else {
                        egui::Color32::DARK_GREEN
                    },
                ),
            )
            .clicked()
        {
            if let Some(engine) = &self.engine {
                let current = engine.bypass_enabled.load(Ordering::Relaxed);
                engine.bypass_enabled.store(!current, Ordering::Relaxed);
            }
        }

        // Spectrum Visualizer
        if self.engine.is_some() {
            ui.add_space(10.0);
            ui.label("📊 Spectrum Analysis");
            self.render_spectrum(ui);

            // Jitter Monitor
            const JITTER_GOOD_US: u32 = 1000;
            const JITTER_WARN_US: u32 = 5000;
            let jitter = self
                .engine
                .as_ref()
                .unwrap()
                .jitter_ewma_us
                .load(Ordering::Relaxed);
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.label("Latency Health:");
                let color = if jitter < JITTER_GOOD_US {
                    egui::Color32::GREEN
                } else if jitter < JITTER_WARN_US {
                    egui::Color32::YELLOW
                } else {
                    egui::Color32::RED
                };
                ui.colored_label(color, format!("{} µs jitter", jitter))
                    .on_hover_text("< 1ms = excellent | 1-5ms = acceptable | > 5ms = may cause audio glitches");
            });
        }
    }
}
