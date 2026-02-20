use crate::audio::{AudioEngine, OutputFilterEngine};
use crate::config::AppConfig;
use crate::updater::{self, UpdateInfo};
use crossbeam_channel::Receiver;
use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use std::sync::atomic::Ordering;
use tray_icon::TrayIcon;
use voidmic_ui::{theme, visualizer, widgets};


use super::devices::get_devices;
use super::tray::{load_icon, QUIT_ID, SHOW_ID, TOGGLE_ID};
use super::wizard::WizardStep;

/// Runs the VoidMic GUI application.
///
/// # Arguments
/// * `model_path` - Path to the model directory (currently unused as RNNoise weights are embedded)
///
/// # Returns
/// Result indicating success or failure of the GUI application
pub fn run_gui() -> eframe::Result<()> {
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
            Ok(Box::new(VoidMicApp::new_with_config(config)))
        }),
    )
}

pub(super) struct VoidMicApp {
    pub(super) input_devices: Vec<String>,
    pub(super) output_devices: Vec<String>,
    pub(super) selected_input: String,
    pub(super) selected_output: String,
    pub(super) engine: Option<AudioEngine>,
    pub(super) status_msg: String,
    pub(super) config: AppConfig,
    pub(super) config_dirty: bool,
    #[allow(dead_code)] // Kept alive for tray icon
    pub(super) tray_icon: Option<TrayIcon>,
    pub(super) is_quitting: bool,
    pub(super) is_calibrating: bool,
    pub(super) update_receiver: Option<std::sync::mpsc::Receiver<Option<UpdateInfo>>>,
    pub(super) update_info: Option<UpdateInfo>,
    pub(super) virtual_sink_module_id: Option<u32>,
    pub(super) connected_apps: Vec<String>,
    pub(super) last_app_refresh: std::time::Instant,
    pub(super) virtual_sink_cached: bool,
    pub(super) last_sink_check: std::time::Instant,
    // Output Filter (Speaker Denoising)
    pub(super) output_filter_engine: Option<OutputFilterEngine>,
    // Echo Cancellation
    pub(super) selected_reference: String,
    // Global Hotkeys
    #[allow(dead_code)] // Manager must be kept alive
    pub(super) hotkey_manager: Option<GlobalHotKeyManager>,
    pub(super) hotkey_id: Option<u32>,
    // Wizard State
    pub(super) show_wizard: bool,
    pub(super) wizard_step: WizardStep,
    // Phase 6
    pub(super) spectrum_receiver: Option<Receiver<(Vec<f32>, Vec<f32>)>>,
    pub(super) last_spectrum_data: (Vec<f32>, Vec<f32>),
    // Track mini mode resize so we only send the command once
    pub(super) mini_mode_resized: bool,
    // Periodic auto-save for dirty config
    pub(super) last_config_save: std::time::Instant,
}

impl VoidMicApp {
    pub(super) fn new_with_config(config: AppConfig) -> Self {
        // Tray Setup
        let tray_menu = tray_icon::menu::Menu::new();
        let toggle_item =
            tray_icon::menu::MenuItem::with_id(TOGGLE_ID, "Enable", true, None);
        let show_item =
            tray_icon::menu::MenuItem::with_id(SHOW_ID, "Show/Hide", true, None);
        let quit_item =
            tray_icon::menu::MenuItem::with_id(QUIT_ID, "Quit", true, None);
        let _ = tray_menu.append_items(&[&toggle_item, &show_item, &quit_item]);

        let icon = load_icon();
        let tray_icon = tray_icon::TrayIconBuilder::new()
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

    pub(super) fn mark_config_dirty(&mut self) {
        self.config_dirty = true;
    }

    pub(super) fn save_config(&mut self) {
        if self.config_dirty {
            self.config.last_input = self.selected_input.clone();
            self.config.last_output = self.selected_output.clone();
            self.config.last_reference = self.selected_reference.clone();
            self.config.save();
            self.config_dirty = false;
        }
    }

    pub(super) fn save_config_now(&mut self) {
        self.config.last_input = self.selected_input.clone();
        self.config.last_output = self.selected_output.clone();
        self.config.last_reference = self.selected_reference.clone();
        self.config.save();
    }

    /// Renders the update banner at the top of the UI.
    /// Returns true if the update was dismissed.
    pub(super) fn render_update_banner(&mut self, ui: &mut egui::Ui) -> bool {
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
    pub(super) fn render_volume_meter(&self, ui: &mut egui::Ui) {
        let volume = if let Some(engine) = &self.engine {
            f32::from_bits(engine.volume_level.load(Ordering::Relaxed))
        } else {
            0.0
        };
        widgets::render_volume_meter(ui, volume, self.config.gate_threshold);
    }

    pub(super) fn render_spectrum(&mut self, ui: &mut egui::Ui) {
        // Receive new data
        if let Some(rx) = &self.spectrum_receiver {
            while let Ok(data) = rx.try_recv() {
                self.last_spectrum_data = data;
            }
        }
        let (in_data, out_data) = &self.last_spectrum_data;
        visualizer::render_spectrum(ui, in_data, out_data);
    }

    /// Checks and handles calibration results.
    pub(super) fn check_calibration_result(&mut self) {
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

                // Bypass Button
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
}

impl eframe::App for VoidMicApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle Tray Events
        if let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if event.id.0 == QUIT_ID {
                self.is_quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if event.id.0 == SHOW_ID {
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
            if let Some(pos) = ctx.input(|i| i.viewport().outer_rect).map(|r| r.min) {
                self.config.window_x = Some(pos.x);
                self.config.window_y = Some(pos.y);
                self.save_config_now();
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }

        // Repaint rate
        if self.engine.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // Auto-save dirty config
        if self.config_dirty && self.last_config_save.elapsed().as_secs() >= 5 {
            self.save_config();
            self.last_config_save = std::time::Instant::now();
        }

        // Check for update result
        if let Some(ref rx) = self.update_receiver {
            if let Ok(update) = rx.try_recv() {
                self.update_info = update;
                self.update_receiver = None;
            }
        }

        if self.show_wizard {
            self.render_wizard(ctx);
            return;
        }

        if self.config.mini_mode {
            if self.render_mini(ctx) {
                self.mini_mode_resized = false;
            } else if !self.mini_mode_resized {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize([150.0, 150.0].into()));
                self.mini_mode_resized = true;
            }
            return;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
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
                self.check_calibration_result();

                // Advanced Features
                ui.add_space(10.0);
                self.render_advanced_features(ui);
                ui.add_space(10.0);

                // Connected Apps display
                #[cfg(target_os = "linux")]
                {
                    if self.engine.is_some() && self.last_app_refresh.elapsed().as_secs() >= 2 {
                        self.connected_apps = crate::pulse_info::get_connected_apps()
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
                            match super::devices::install_virtual_cable() {
                                Ok(msg) => {
                                    self.status_msg = msg;
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

                    // Start on Boot
                    let mut start_on_boot = self.config.start_on_boot;
                    if ui.checkbox(&mut start_on_boot, "Start on Boot").changed() {
                        self.config.start_on_boot = start_on_boot;
                        if start_on_boot {
                            if let Err(e) = crate::autostart::enable_autostart() {
                                self.status_msg = format!("Autostart error: {}", e);
                                self.config.start_on_boot = false;
                            } else {
                                self.status_msg = "Autostart enabled".to_string();
                            }
                        } else if let Err(e) = crate::autostart::disable_autostart() {
                            self.status_msg = format!("Autostart error: {}", e);
                        } else {
                            self.status_msg = "Autostart disabled".to_string();
                        }
                        self.save_config_now();
                    }

                    // Start Minimized
                    let mut start_minimized = self.config.start_minimized;
                    if ui.checkbox(&mut start_minimized, "Start Minimized to Tray").changed() {
                        self.config.start_minimized = start_minimized;
                        self.save_config_now();
                    }

                    // Auto-Start Processing
                    let mut auto_start = self.config.auto_start_processing;
                    if ui.checkbox(&mut auto_start, "Auto-Start Processing").changed() {
                        self.config.auto_start_processing = auto_start;
                        self.save_config_now();
                    }

                    // Dark Mode
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
