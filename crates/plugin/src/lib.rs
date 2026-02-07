use crossbeam_channel::Receiver;
use nih_plug::prelude::*;
use nih_plug_egui::{create_egui_editor, widgets, EguiState};
use ringbuf::traits::{Consumer, Observer, Producer};
use ringbuf::HeapRb;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use voidmic_core::constants::{FRAME_SIZE, SAMPLE_RATE};
use voidmic_core::VoidProcessor;
use voidmic_ui::{theme, visualizer, widgets as ui_widgets};

struct VoidMicPlugin {
    params: Arc<VoidMicParams>,
    // editor_state removed as it is in params

    // Audio Processing State
    processor: Option<VoidProcessor>,

    // Ring Buffers (Audio I/O)
    rb_in: Option<HeapRb<f32>>,
    rb_out: Option<HeapRb<f32>>,

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
            rb_in: None,
            rb_out: None,
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

        let buffer_size = FRAME_SIZE * 4 * 2; // Always stereo

        // Ringbuf 0.4
        self.rb_in = Some(HeapRb::<f32>::new(buffer_size));
        self.rb_out = Some(HeapRb::<f32>::new(buffer_size));

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

        // 1. Push Input (Interleaved)
        if let Some(rb_in) = &mut self.rb_in {
            for i in 0..num_samples {
                if num_channels == 2 {
                    let _ = rb_in.try_push(channel_data[0][i]);
                    let _ = rb_in.try_push(channel_data[1][i]);
                } else if num_channels == 1 {
                    // Duplicate Mono to Stereo
                    let val = channel_data[0][i];
                    let _ = rb_in.try_push(val);
                    let _ = rb_in.try_push(val);
                }
            }
        }

        // 2. Process chunks
        if let (Some(rb_in), Some(rb_out)) = (&mut self.rb_in, &mut self.rb_out) {
            let mut left_in = [0.0f32; FRAME_SIZE];
            let mut right_in = [0.0f32; FRAME_SIZE];
            let mut left_out = [0.0f32; FRAME_SIZE];
            let mut right_out = [0.0f32; FRAME_SIZE];

            // Need 2 * FRAME_SIZE samples for a full stereo frame
            while rb_in.occupied_len() >= FRAME_SIZE * 2 {
                for j in 0..FRAME_SIZE {
                    left_in[j] = rb_in.try_pop().unwrap_or(0.0);
                    right_in[j] = rb_in.try_pop().unwrap_or(0.0);
                }

                processor.process_frame(
                    &[&left_in, &right_in],
                    &mut [&mut left_out, &mut right_out],
                    None,
                    self.params.suppression.value(),
                    self.params.gate_threshold.value(),
                    true,
                );

                for j in 0..FRAME_SIZE {
                    let _ = rb_out.try_push(left_out[j]);
                    let _ = rb_out.try_push(right_out[j]);
                }
            }
        }

        // 3. Output
        if let Some(rb_out) = &mut self.rb_out {
            for i in 0..num_samples {
                if rb_out.occupied_len() >= 2 {
                    let l = rb_out.try_pop().unwrap_or(0.0);
                    let r = rb_out.try_pop().unwrap_or(0.0);

                    if num_channels == 1 {
                        channel_data[0][i] = (l + r) * 0.5;
                    } else {
                        channel_data[0][i] = l;
                        channel_data[1][i] = r;
                    }
                }
            }
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
