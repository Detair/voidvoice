use nih_plug::prelude::*;
use nih_plug_egui::{create_egui_editor, EguiState, widgets};
use ringbuf::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::num::NonZeroU32;
use voidmic_core::constants::{FRAME_SIZE, SAMPLE_RATE};
use voidmic_core::VoidProcessor;
use crossbeam_channel::Receiver;
use voidmic_ui::{theme, visualizer, widgets as ui_widgets};

struct VoidMicPlugin {
    params: Arc<VoidMicParams>,
    // editor_state removed as it is in params
    
    // Audio Processing State
    processor: Option<VoidProcessor>,
    
    // Ring Buffers (Audio I/O)
    rb_in_prod: Option<Producer<f32>>,
    rb_in_cons: Option<Consumer<f32>>,
    rb_out_prod: Option<Producer<f32>>,
    rb_out_cons: Option<Consumer<f32>>,

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
            rb_in_prod: None,
            rb_in_cons: None,
            rb_out_prod: None,
            rb_out_cons: None,
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
                FloatRange::Linear { min: 0.005, max: 0.05 },
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
    const EMAIL: &'static str = "user@example.com";

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
            |_, _| {},
            move |egui_ctx, setter, state| {
                theme::setup_custom_style(egui_ctx, true);
                
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
                     ui.add(widgets::ParamSlider::for_param(&params.gate_threshold, setter));
                     
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
                     visualizer::render_spectrum(ui, &state.last_spectrum_data.0, &state.last_spectrum_data.1);
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
            nih_log!("VoidMic only supports 48kHz currently. Host is using {:.0}Hz", buffer_config.sample_rate);
        }

        let (tx, rx) = crossbeam_channel::bounded(2);
        self.spectrum_receiver = Some(rx);

        let processor = VoidProcessor::new(
            2, 
            2, // VAD Sensitivity (Aggressive)
            (0.0, 0.0, 0.0), 
            0.7,
            false,
        );
        
        let mut processor = processor;
        processor.spectrum_sender = Some(tx);
        
        self.volume_level = processor.volume_level.clone();
        self.processor = Some(processor);

        let buffer_size = FRAME_SIZE * 4 * 2; // * 2 for Stereo
        
        // Ringbuf 0.2.8
        let rb_in = RingBuffer::<f32>::new(buffer_size);
        let (prod_in, cons_in) = rb_in.split();
        self.rb_in_prod = Some(prod_in);
        self.rb_in_cons = Some(cons_in);

        let rb_out = RingBuffer::<f32>::new(buffer_size);
        let (prod_out, cons_out) = rb_out.split();
        self.rb_out_prod = Some(prod_out);
        self.rb_out_cons = Some(cons_out);

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

        processor.bypass_enabled.store(self.params.bypass.value(), Ordering::Relaxed);
        processor.agc_enabled.store(self.params.agc_enabled.value(), Ordering::Relaxed);
        
        processor.process_updates();

        let channel_data = buffer.as_slice();
        let num_channels = channel_data.len();
        if num_channels == 0 {
             return ProcessStatus::Normal;
        }
        let num_samples = channel_data[0].len();
        
        // 1. Push Input (Interleaved)
        if let Some(prod_in) = &mut self.rb_in_prod {
             for i in 0..num_samples {
                 if num_channels == 2 {
                     let _ = prod_in.push(channel_data[0][i]);
                     let _ = prod_in.push(channel_data[1][i]);
                 } else if num_channels == 1 {
                     // Duplicate Mono to Stereo
                     let val = channel_data[0][i];
                     let _ = prod_in.push(val);
                     let _ = prod_in.push(val);
                 }
             }
        }

        // 2. Process chunks
        if let (Some(cons_in), Some(prod_out)) = (&mut self.rb_in_cons, &mut self.rb_out_prod) {
            let mut left_in = [0.0f32; FRAME_SIZE];
            let mut right_in = [0.0f32; FRAME_SIZE];
            let mut left_out = [0.0f32; FRAME_SIZE];
            let mut right_out = [0.0f32; FRAME_SIZE];
            
            // Need 2 * FRAME_SIZE samples for a full stereo frame
            while cons_in.len() >= FRAME_SIZE * 2 {
                for j in 0..FRAME_SIZE {
                     left_in[j] = cons_in.pop().unwrap_or(0.0);
                     right_in[j] = cons_in.pop().unwrap_or(0.0);
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
                     let _ = prod_out.push(left_out[j]);
                     let _ = prod_out.push(right_out[j]);
                }
            }
        }

        // 3. Output
        if let Some(cons_out) = &mut self.rb_out_cons {
            for i in 0..num_samples {
                if cons_out.len() >= 2 {
                    let l = cons_out.pop().unwrap_or(0.0);
                    let r = cons_out.pop().unwrap_or(0.0);
                    
                    if num_channels >= 1 { channel_data[0][i] = l; } // If output is mono, take left? Or mix? Left is safer.
                    if num_channels >= 2 { channel_data[1][i] = r; }
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
