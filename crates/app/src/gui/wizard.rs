use eframe::egui;
use std::sync::atomic::Ordering;

use super::app::VoidMicApp;


#[derive(PartialEq)]
pub(super) enum WizardStep {
    Welcome,
    SelectMic,
    SelectOutput,
    Calibration,
    Finish,
}

impl VoidMicApp {
    pub(super) fn render_wizard(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.heading("✨ Welcome to VoidMic ✨");
                ui.add_space(10.0);
                ui.label("Let's get your audio set up for crystal clear communication.");
                ui.add_space(20.0);
                ui.separator();
                ui.add_space(20.0);

                match self.wizard_step {
                    WizardStep::Welcome => {
                        ui.label("VoidMic uses AI to remove background noise from your microphone.");
                        ui.label("This short wizard will help you select your devices and calibrate the noise gate.");
                        ui.add_space(40.0);
                        if ui.button("Get Started ➡").clicked() {
                            self.wizard_step = WizardStep::SelectMic;
                        }
                    }
                    WizardStep::SelectMic => {
                        ui.heading("🎤 Select Microphone");
                        ui.add_space(10.0);
                        ui.label("Choose the microphone you want to clean up:");
                        let mut changed = false;
                        egui::ComboBox::from_id_salt("wizard_mic")
                            .selected_text(&self.selected_input)
                            .width(250.0)
                            .show_ui(ui, |ui| {
                                for dev in &self.input_devices {
                                    if ui.selectable_value(&mut self.selected_input, dev.clone(), dev).changed() {
                                        changed = true;
                                    }
                                }
                            });
                        if changed { self.mark_config_dirty(); }

                        ui.add_space(40.0);
                        if ui.button("Next ➡").clicked() {
                            self.wizard_step = WizardStep::SelectOutput;
                        }
                    }
                    WizardStep::SelectOutput => {
                        ui.heading("🔊 Select Output");
                        ui.add_space(10.0);
                        ui.label("Choose where you want to hear the processed audio (or your speakers):");
                        let mut changed = false;
                        egui::ComboBox::from_id_salt("wizard_out")
                            .selected_text(&self.selected_output)
                            .width(250.0)
                            .show_ui(ui, |ui| {
                                for dev in &self.output_devices {
                                    if ui.selectable_value(&mut self.selected_output, dev.clone(), dev).changed() {
                                        changed = true;
                                    }
                                }
                            });
                        if changed { self.mark_config_dirty(); }

                        ui.add_space(40.0);
                        ui.horizontal(|ui| {
                            if ui.button("⬅ Back").clicked() { self.wizard_step = WizardStep::SelectMic; }
                            if ui.button("Next ➡").clicked() { self.wizard_step = WizardStep::Calibration; }
                        });
                    }
                    WizardStep::Calibration => {
                        ui.heading("🎛️ Calibration");
                        ui.add_space(10.0);
                        ui.label("Stay quiet for 3 seconds to measure background noise.");

                        self.render_volume_meter(ui);

                        ui.add_space(20.0);

                        let calibrate_enabled = self.engine.is_some() && !self.is_calibrating;

                        if self.engine.is_none() {
                            if ui.button("▶ Start Audio Engine").clicked() {
                                self.start_engine();
                            }
                        } else if ui.add_enabled(calibrate_enabled, egui::Button::new("🎯 Start Calibration")).clicked() {
                            if let Some(engine) = &self.engine {
                                engine.calibration_mode.store(true, Ordering::Relaxed);
                                self.is_calibrating = true;
                                self.status_msg = "Calibrating... stay quiet".to_string();
                            }
                        }

                        ui.label(format!("Status: {}", self.status_msg));
                        self.check_calibration_result();

                        ui.add_space(40.0);
                        ui.horizontal(|ui| {
                            if ui.button("⬅ Back").clicked() { self.wizard_step = WizardStep::SelectOutput; }
                            if ui.button("Finish ✅").clicked() { self.wizard_step = WizardStep::Finish; }
                        });
                    }
                    WizardStep::Finish => {
                        ui.heading("🎉 All Set!");
                        ui.label("VoidMic is ready to use.");
                        ui.add_space(20.0);
                        if ui.button("Open Main Interface").clicked() {
                            self.config.first_run = false;
                            self.show_wizard = false;
                            self.save_config_now();
                        }
                    }
                }
            });
        });
    }
}
