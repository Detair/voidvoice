use crossbeam_channel::Receiver;
use nih_plug::prelude::*;
use nih_plug_egui::{create_egui_editor, widgets, EguiState};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use voidmic_core::constants::SAMPLE_RATE;
use voidmic_core::{FrameAdapter, VoidProcessor};
use voidmic_ui::{theme, visualizer, widgets as ui_widgets};

struct VoidMicPlugin {
    params: Arc<VoidMicParams>,

    // Audio Processing State
    processor: Option<VoidProcessor>,
    adapter: Option<FrameAdapter>,

    // GUI Data Bridging
    volume_level: Arc<AtomicU32>,
    spectrum_receiver: Option<Receiver<(Vec<f32>, Vec<f32>)>>,
}

#[derive(Params)]
struct VoidMicParams {
    #[persist = "editor-state"]
    editor_state: Arc<EguiState>,

    #[id = "threshold"]
    pub gate_threshold: FloatParam,

    #[id = "suppression"]
    pub suppression: FloatParam,

    #[id = "bypass"]
    pub bypass: BoolParam,

    #[id = "agc"]
    pub agc_enabled: BoolParam,
}

struct GuiData {
    params: Arc<VoidMicParams>,
    volume_level: Arc<AtomicU32>,
    spectrum_receiver: Option<Receiver<(Vec<f32>, Vec<f32>)>>,
    last_spectrum_data: (Vec<f32>, Vec<f32>),
}

impl Default for VoidMicPlugin {
    fn default() -> Self {
        Self {
            params: Arc::new(VoidMicParams::default()),
            processor: None,
            adapter: None,
            volume_level: Arc::new(AtomicU32::new(0)),
            spectrum_receiver: None,
        }
    }
}

impl Default for VoidMicParams {
    fn default() -> Self {
        Self {
            editor_state: EguiState::from_size(450, 450),
            gate_threshold: FloatParam::new(
                "Gate Threshold",
                0.015,
                FloatRange::Linear {
                    min: 0.005,
                    max: 0.05,
                },
            )
            .with_step_size(0.001)
            .with_unit(""),

            suppression: FloatParam::new(
                "Suppression",
                1.0,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            )
            .with_unit("%"),

            bypass: BoolParam::new("Bypass", false),
            agc_enabled: BoolParam::new("AGC", false),
        }
    }
}

impl Plugin for VoidMicPlugin {
    const NAME: &'static str = "VoidMic";
    const VENDOR: &'static str = "Detair";
    const URL: &'static str = "https://github.com/Detair/voidvoice";
    const EMAIL: &'static str = "";

    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
    ];

    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        let gui_data = GuiData {
            params: self.params.clone(),
            volume_level: self.volume_level.clone(),
            spectrum_receiver: self.spectrum_receiver.clone(),
            last_spectrum_data: (Vec::new(), Vec::new()),
        };

        create_egui_editor(
            self.params.editor_state.clone(),
            gui_data,
            |egui_ctx, _| {
                theme::setup_custom_style(egui_ctx, true);
            },
            move |egui_ctx, setter, state| {

                egui::CentralPanel::default().show(egui_ctx, |ui| {
                    ui.heading("VoidMic Plugin");
                    ui.separator();

                    let params = &state.params;

                    // Bypass & AGC
                    ui.horizontal(|ui| {
                        ui.label("Bypass:");
                        ui.add(widgets::ParamSlider::for_param(&params.bypass, setter));
                    });

                    ui.add_space(10.0);

                    // Controls
                    ui.label("Gate Threshold:");
                    ui.add(widgets::ParamSlider::for_param(
                        &params.gate_threshold,
                        setter,
                    ));

                    ui.label("Suppression:");
                    ui.add(widgets::ParamSlider::for_param(&params.suppression, setter));

                    ui.separator();

                    // Volume Meter
                    let vol = f32::from_bits(state.volume_level.load(Ordering::Relaxed));
                    let thresh = params.gate_threshold.value();
                    ui_widgets::render_volume_meter(ui, vol, thresh);

                    // Visualizer
                    ui.add_space(10.0);
                    ui.label("Spectrum:");

                    if let Some(rx) = &state.spectrum_receiver {
                        while let Ok(data) = rx.try_recv() {
                            state.last_spectrum_data = data;
                        }
                    }
                    visualizer::render_spectrum(
                        ui,
                        &state.last_spectrum_data.0,
                        &state.last_spectrum_data.1,
                    );
                });
            },
        )
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        if buffer_config.sample_rate != SAMPLE_RATE as f32 {
            nih_log!(
                "VoidMic requires 48kHz sample rate. Host is using {:.0}Hz. Plugin initialization rejected.",
                buffer_config.sample_rate
            );
            return false;
        }

        // process() always interleaves to stereo internally, so the processor
        // and ring buffers must always be sized for 2 channels regardless of layout.
        let (tx, rx) = crossbeam_channel::bounded(2);
        self.spectrum_receiver = Some(rx);

        let mut processor = VoidProcessor::new(
            2, // Always stereo: process() duplicates mono to stereo
            2, // VAD Sensitivity (Aggressive)
            (0.0, 0.0, 0.0),
            0.7,
            false,
        );
        processor.spectrum_sender = Some(tx);

        self.volume_level = processor.volume_level.clone();
        self.processor = Some(processor);
        self.adapter = Some(FrameAdapter::new());

        true
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let processor = match self.processor.as_mut() {
            Some(p) => p,
            None => return ProcessStatus::Normal,
        };
        let adapter = match self.adapter.as_mut() {
            Some(a) => a,
            None => return ProcessStatus::Normal,
        };

        processor
            .bypass_enabled
            .store(self.params.bypass.value(), Ordering::Relaxed);
        processor
            .agc_enabled
            .store(self.params.agc_enabled.value(), Ordering::Relaxed);

        processor.process_updates();

        let channel_data = buffer.as_slice();
        let num_channels = channel_data.len();
        if num_channels == 0 {
            return ProcessStatus::Normal;
        }
        let num_samples = channel_data[0].len();

        // 1. Push Input
        if num_channels == 2 {
            adapter.push_stereo_interleaved(&channel_data[0][..num_samples], &channel_data[1][..num_samples]);
        } else if num_channels == 1 {
            adapter.push_mono(&channel_data[0][..num_samples]);
        }

        // 2. Process available frames
        adapter.process_available(
            processor,
            self.params.suppression.value(),
            self.params.gate_threshold.value(),
            true,
        );

        // 3. Output
        if num_channels == 1 {
            adapter.pop_mono(&mut channel_data[0][..num_samples]);
        } else {
            // Split borrows: we need mutable references to two different slices
            let (left_slice, rest) = channel_data.split_at_mut(1);
            adapter.pop_stereo(
                &mut left_slice[0][..num_samples],
                &mut rest[0][..num_samples],
            );
        }

        ProcessStatus::Normal
    }
}

impl ClapPlugin for VoidMicPlugin {
    const CLAP_ID: &'static str = "com.detair.voidmic";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Hybrid AI Noise Reduction");
    const CLAP_MANUAL_URL: Option<&'static str> = Some("https://github.com/Detair/voidvoice");
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::AudioEffect, ClapFeature::Utility];
}

impl Vst3Plugin for VoidMicPlugin {
    const VST3_CLASS_ID: [u8; 16] = *b"DetairVoidMicV01";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Dynamics];
}

nih_export_clap!(VoidMicPlugin);
nih_export_vst3!(VoidMicPlugin);
