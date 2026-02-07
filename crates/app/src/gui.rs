use crate::audio::{AudioEngine, OutputFilterEngine};
use crate::autostart;
use crate::config::AppConfig;
use crate::pulse_info;
use crate::updater::{self, UpdateInfo};
use crate::virtual_device;
use cpal::traits::{DeviceTrait, HostTrait};
use crossbeam_channel::Receiver;
use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;
use tray_icon::Icon;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIcon, TrayIconBuilder,
};
use voidmic_ui::{theme, visualizer, widgets};

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
            theme::setup_custom_style(&cc.egui_ctx, dark_mode);
            Ok(Box::new(VoidMicApp::new_with_config(model_path, config)))
        }),
    )
}

struct Preset {
    name: &'static str,
    gate_threshold: f32,
    suppression_strength: f32,
    dynamic_threshold_enabled: bool,
}

const PRESETS: &[Preset] = &[
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

#[derive(PartialEq)]
enum WizardStep {
    Welcome,
    SelectMic,
    SelectOutput,
    Calibration,
    Finish,
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
    virtual_sink_cached: bool,
    last_sink_check: std::time::Instant,
    // Output Filter (Speaker Denoising)
    output_filter_engine: Option<OutputFilterEngine>,
    // Echo Cancellation
    selected_reference: String,
    // Global Hotkeys
    #[allow(dead_code)] // Manager must be kept alive
    hotkey_manager: Option<GlobalHotKeyManager>,
    hotkey_id: Option<u32>,
    // Wizard State
    show_wizard: bool,
    wizard_step: WizardStep,
    // Phase 6
    spectrum_receiver: Option<Receiver<(Vec<f32>, Vec<f32>)>>,
    last_spectrum_data: (Vec<f32>, Vec<f32>), // Cache for rendering
    // Track mini mode resize so we only send the command once
    mini_mode_resized: bool,
    // Periodic auto-save for dirty config
    last_config_save: std::time::Instant,
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

        let icon = load_icon();
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

        let default_ref = if !config.last_reference.is_empty() && inputs.contains(&config.last_reference) {
            config.last_reference.clone()
        } else {
            inputs
                .first()
                .cloned()
                .unwrap_or_else(|| "default".to_string())
        };

        let auto_start = config.auto_start_processing;
        let show_wizard = config.first_run;

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
            virtual_sink_cached: false,
            last_sink_check: std::time::Instant::now() - std::time::Duration::from_secs(5),
            selected_reference: default_ref,
            hotkey_manager: match GlobalHotKeyManager::new() {
                Ok(m) => Some(m),
                Err(e) => {
                    log::warn!("Failed to initialize global hotkey manager: {:?}", e);
                    None
                }
            },
            hotkey_id: None,
            show_wizard,
            wizard_step: WizardStep::Welcome,
            spectrum_receiver: None,
            last_spectrum_data: (Vec::new(), Vec::new()),
            mini_mode_resized: false,
            last_config_save: std::time::Instant::now(),
        };

        // Register Hotkey
        if let Some(ref manager) = app.hotkey_manager {
            if let Ok(hotkey) = app.config.toggle_hotkey.parse::<HotKey>() {
                if manager.register(hotkey).is_ok() {
                    app.hotkey_id = Some(hotkey.id());
                } else {
                    log::warn!("Failed to register hotkey: {}", app.config.toggle_hotkey);
                }
            }
        }

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

    fn stop_engine(&mut self) {
        self.engine = None;
        self.output_filter_engine = None;
        self.status_msg = "Stopped".to_string();
    }

    fn toggle_engine(&mut self) {
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

    fn mark_config_dirty(&mut self) {
        self.config_dirty = true;
    }

    fn save_config(&mut self) {
        if self.config_dirty {
            self.config.last_input = self.selected_input.clone();
            self.config.last_output = self.selected_output.clone();
            self.config.last_reference = self.selected_reference.clone();
            // gate_threshold is already in config from slider updates
            self.config.save();
            self.config_dirty = false;
        }
    }

    fn save_config_now(&mut self) {
        self.config.last_input = self.selected_input.clone();
        self.config.last_output = self.selected_output.clone();
        self.config.last_reference = self.selected_reference.clone();
        self.config.save();
    }

    fn apply_preset(&mut self, preset_name: &str) {
        if let Some(preset) = PRESETS.iter().find(|p| p.name == preset_name) {
            self.config.gate_threshold = preset.gate_threshold;
            self.config.suppression_strength = preset.suppression_strength;
            self.config.dynamic_threshold_enabled = preset.dynamic_threshold_enabled;
            // Echo cancel is user preference, not preset
            // Output filter is user preference
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

    /// Renders the volume meter with dB scaling and threshold marker.
    fn render_volume_meter(&self, ui: &mut egui::Ui) {
        let volume = if let Some(engine) = &self.engine {
            f32::from_bits(engine.volume_level.load(Ordering::Relaxed))
        } else {
            0.0
        };

        widgets::render_volume_meter(ui, volume, self.config.gate_threshold);
    }

    /// Renders the device selection dropdowns.
    fn render_device_selectors(&mut self, ui: &mut egui::Ui) {
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

        // One-Click Setup Section (cache pactl check, refresh every 5 seconds)
        if self.last_sink_check.elapsed().as_secs() >= 5 {
            self.virtual_sink_cached = virtual_device::virtual_sink_exists();
            self.last_sink_check = std::time::Instant::now();
        }
        ui.horizontal(|ui| {
            let sink_exists = self.virtual_sink_cached;

            if sink_exists {
                ui.colored_label(egui::Color32::GREEN, "✔ Virtual Mic Active");
                if ui.button("Destroy").clicked() {
                    // Best effort cleanup
                    if let Some(id) = self.virtual_sink_module_id {
                        let _ = virtual_device::destroy_virtual_sink(id);
                    } else {
                        let _ = virtual_device::destroy_virtual_sink(0);
                    }
                    self.virtual_sink_module_id = None;
                    // Refresh device list to remove it
                    let (inputs, outputs) = get_devices();
                    self.input_devices = inputs;
                    self.output_devices = outputs;
                }

                // Hint for usage
                ui.label(egui::RichText::new("ℹ️ Select 'VoidMic_Clean' in Discord").size(10.0));
            } else {
                if ui
                    .button("✨ Create Virtual Mic")
                    .on_hover_text("Creates a virtual device for Discord/Zoom")
                    .clicked()
                {
                    match virtual_device::create_virtual_sink() {
                        Ok(device) => {
                            self.virtual_sink_module_id = Some(device.module_id);

                            // Refresh devices
                            let (inputs, outputs) = get_devices();
                            self.input_devices = inputs;
                            self.output_devices = outputs;

                            // Auto-select the new sink
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
            }
        });
    }

    /// Renders the threshold and suppression controls.
    fn render_threshold_controls(&mut self, ui: &mut egui::Ui) {
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

    /// Checks and handles calibration results.
    fn check_calibration_result(&mut self) {
        if self.is_calibrating {
            if let Some(engine) = &self.engine {
                if !engine.calibration_mode.load(Ordering::Relaxed) {
                    let result = f32::from_bits(engine.calibration_result.load(Ordering::Relaxed));
                    if result > 0.0 {
                        self.config.gate_threshold = result;
                        engine.gate_threshold.store(result.to_bits(), Ordering::Relaxed);
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
                // Start/stop output filter engine on toggle
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
                // Echo cancel requires engine restart to reconfigure streams
                if self.engine.is_some() {
                    let prev_echo = !self.config.echo_cancel_enabled;
                    self.stop_engine();
                    self.start_engine();
                    if self.engine.is_none() {
                        // Restart failed - revert toggle so UI matches reality
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

        // Phase 4: Pro Audio (AGC + Bypass)
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
        // Spectrum Visualizer (Phase 6)
        if self.engine.is_some() {
            ui.add_space(10.0);
            ui.label("📊 Spectrum Analysis");
            self.render_spectrum(ui);

            // Jitter Monitor (Phase 6)
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

    fn render_spectrum(&mut self, ui: &mut egui::Ui) {
        // Receive new data
        if let Some(rx) = &self.spectrum_receiver {
            // Drain channel to get latest
            while let Ok(data) = rx.try_recv() {
                self.last_spectrum_data = data;
            }
        }

        let (in_data, out_data) = &self.last_spectrum_data;
        visualizer::render_spectrum(ui, in_data, out_data);
    }

    fn render_mini(&mut self, ctx: &egui::Context) -> bool {
        let mut expanded = false;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.label("🌌 VoidMic");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("⛶").on_hover_text("Expand").clicked() {
                            self.config.mini_mode = false;
                            self.mark_config_dirty();
                            expanded = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                                [450.0, 450.0].into(),
                            ));
                        }
                    });
                });

                ui.separator();

                // Status
                let active = self.engine.is_some();
                ui.colored_label(
                    if active {
                        egui::Color32::GREEN
                    } else {
                        egui::Color32::RED
                    },
                    if active { "Active" } else { "Inactive" },
                );

                ui.add_space(5.0);

                // Bypass Button (Big)
                let bypass_enabled = if let Some(engine) = &self.engine {
                    engine.bypass_enabled.load(Ordering::Relaxed)
                } else {
                    false
                };

                let btn_color = if bypass_enabled {
                    egui::Color32::DARK_RED
                } else {
                    egui::Color32::DARK_GREEN
                };
                let btn_text = if bypass_enabled {
                    "Stopped"
                } else {
                    "Processing"
                };

                if ui
                    .add_sized([80.0, 30.0], egui::Button::new(btn_text).fill(btn_color))
                    .clicked()
                {
                    if let Some(engine) = &self.engine {
                        let current = engine.bypass_enabled.load(Ordering::Relaxed);
                        engine.bypass_enabled.store(!current, Ordering::Relaxed);
                    }
                }

                ui.add_space(5.0);
                self.render_volume_meter(ui);
            });
        });
        expanded
    }

    fn render_wizard(&mut self, ctx: &egui::Context) {
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
                self.toggle_engine();
            }
        }

        // Handle Global Hotkeys
        if let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if let Some(id) = self.hotkey_id {
                if event.id == id && event.state == global_hotkey::HotKeyState::Released {
                    self.toggle_engine();
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

        // Only repaint at 30fps when engine is running (for meters/visualizer)
        // Otherwise use a slow poll rate for tray events and hotkeys
        if self.engine.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // Auto-save dirty config every 5 seconds to prevent data loss on crash
        if self.config_dirty && self.last_config_save.elapsed().as_secs() >= 5 {
            self.save_config();
            self.last_config_save = std::time::Instant::now();
        }

        // Check for update result from async receiver
        if let Some(ref rx) = self.update_receiver {
            if let Ok(update) = rx.try_recv() {
                self.update_info = update;
                self.update_receiver = None; // Consumed
            }
        }

        if self.show_wizard {
            self.render_wizard(ctx);
            return;
        }

        if self.config.mini_mode {
            if self.render_mini(ctx) {
                // Expanding back to full size
                self.mini_mode_resized = false;
            } else if !self.mini_mode_resized {
                // Shrink window once on transition to mini mode
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize([150.0, 150.0].into()));
                self.mini_mode_resized = true;
            }
            return;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // Update banner at top
            if self.render_update_banner(ui) {
                self.update_info = None;
            }

            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {

            ui.heading("VoidMic 🌌");
            ui.horizontal(|ui| {
                 ui.label(egui::RichText::new("Hybrid Noise Reduction").size(10.0).weak());
                 ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                     if ui.button("➖").on_hover_text("Compact Mode").clicked() {
                         self.config.mini_mode = true;
                         self.mini_mode_resized = false;
                         self.mark_config_dirty();
                     }
                 });
            });
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
                self.toggle_engine();
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
                     theme::setup_custom_style(ui.ctx(), dark_mode);
                 }

                 ui.add_space(5.0);
                 ui.horizontal(|ui| {
                     ui.label("Global Hotkey:");
                     ui.code(self.config.toggle_hotkey.as_str());
                     ui.label(egui::RichText::new("ℹ️ Edit in config.json").size(10.0));
                 });
            });
            }); // ScrollArea
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
fn load_icon() -> Icon {
    let icon_bytes = include_bytes!("../assets/icon_32.png");
    let image = image::load_from_memory(icon_bytes)
        .expect("Failed to load icon asset")
        .into_rgba8();
    let (width, height) = image.dimensions();
    let rgba = image.into_raw();
    Icon::from_rgba(rgba, width, height)
        .unwrap_or_else(|_| Icon::from_rgba(vec![0; 32 * 32 * 4], 32, 32).unwrap())
}
