use crate::audio::{AudioEngine, OutputFilterEngine};
use crate::virtual_device;


use super::app::VoidMicApp;
use super::devices::get_devices;

impl VoidMicApp {
    pub(super) fn start_engine(&mut self) {
        if self.engine.is_some() {
            return;
        }

        self.status_msg = "Initializing Hybrid Engine...".to_string();

        // Auto-create virtual sink on Linux
        #[cfg(target_os = "linux")]
        {
            if self.virtual_sink_module_id.is_none() {
                match virtual_device::create_virtual_sink() {
                    Ok(device) => {
                        self.virtual_sink_module_id = Some(device.module_id);
                        let (inputs, outputs) = get_devices();
                        self.input_devices = inputs;
                        self.output_devices = outputs.clone();
                        if let Some(sink) = outputs.iter().find(|d| d.contains("VoidMic_Clean")) {
                            self.selected_output = sink.clone();
                        }
                    }
                    Err(e) => {
                        self.status_msg = format!("Virtual sink warning: {}", e);
                    }
                }
            }
        }

        let (tx, rx) = crossbeam_channel::bounded(2);

        match AudioEngine::start(
            &self.selected_input,
            &self.selected_output,
            &self.model_path,
            self.config.gate_threshold,
            self.config.suppression_strength,
            self.config.echo_cancel_enabled,
            if self.config.echo_cancel_enabled { Some(self.selected_reference.as_str()) } else { None },
            self.config.dynamic_threshold_enabled,
            self.config.vad_sensitivity,
            self.config.eq_enabled,
            (
                self.config.eq_low_gain,
                self.config.eq_mid_gain,
                self.config.eq_high_gain,
            ),
            self.config.agc_enabled,
            self.config.agc_target_level,
            false,
            Some(tx),
        ) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.spectrum_receiver = Some(rx);
                self.status_msg = "Active (RNNoise + Gate)".to_string();
                self.save_config();

                // Start output filter AFTER main engine succeeds
                if self.config.output_filter_enabled {
                    match OutputFilterEngine::start(
                        &self.selected_reference,
                        &self.selected_output,
                        self.config.suppression_strength,
                    ) {
                        Ok(filter) => self.output_filter_engine = Some(filter),
                        Err(e) => {
                            log::error!("Output filter failed to start: {}", e);
                            self.status_msg = format!("Active (output filter error: {})", e);
                            self.config.output_filter_enabled = false;
                        }
                    }
                }
            }
            Err(e) => {
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
                log::error!("Failed to start engine: {}", e);
            }
        }
    }

    pub(super) fn stop_engine(&mut self) {
        self.engine = None;
        self.output_filter_engine = None;
        self.status_msg = "Stopped".to_string();
    }

    pub(super) fn toggle_engine(&mut self) {
        if self.engine.is_some() {
            self.stop_engine();
            if let Some(ref tray) = self.tray_icon {
                let _ = tray.set_tooltip(Some("VoidMic - Disabled"));
            }
        } else {
            self.start_engine();
            if let Some(ref tray) = self.tray_icon {
                let _ = tray.set_tooltip(Some("VoidMic - Active"));
            }
        }
    }
}
