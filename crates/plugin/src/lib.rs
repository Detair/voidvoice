use nih_plug::prelude::*;
use ringbuf::{Consumer, Producer, RingBuffer};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::num::NonZeroU32;
use voidmic_core::constants::{FRAME_SIZE, SAMPLE_RATE};
use voidmic_core::VoidProcessor;

struct VoidMicPlugin {
    params: Arc<VoidMicParams>,
    processor: Option<VoidProcessor>,
    
    // Ring Buffers (ringbuf 0.2.8 types)
    rb_in_prod: Option<Producer<f32>>,
    rb_in_cons: Option<Consumer<f32>>,
    rb_out_prod: Option<Producer<f32>>,
    rb_out_cons: Option<Consumer<f32>>,
}

#[derive(Params)]
struct VoidMicParams {
    #[id = "threshold"]
    pub gate_threshold: FloatParam,

    #[id = "suppression"]
    pub suppression: FloatParam,

    #[id = "bypass"]
    pub bypass: BoolParam,
    
    #[id = "agc"]
    pub agc_enabled: BoolParam,
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
        }
    }
}

impl Default for VoidMicParams {
    fn default() -> Self {
        Self {
            gate_threshold: FloatParam::new(
                "Gate Threshold",
                0.015,
                FloatRange::Linear { min: 0.000, max: 0.1 },
            )
            .with_step_size(0.001),
            
            suppression: FloatParam::new(
                "Suppression",
                1.0,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            ),

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

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        if buffer_config.sample_rate != SAMPLE_RATE as f32 {
            nih_log!("VoidMic only supports 48kHz currently. Host is using {:.0}Hz", buffer_config.sample_rate);
        }

        let processor = VoidProcessor::new(
            2, // Default VAD: Aggressive
            (0.0, 0.0, 0.0), // No EQ default
            0.7, // AGC Target
            false, // Echo Cancel disabled for plugin
        );

        self.processor = Some(processor);

        let buffer_size = FRAME_SIZE * 4;
        
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

        // Sync Parameters
        processor.bypass_enabled.store(self.params.bypass.value(), Ordering::Relaxed);
        processor.agc_enabled.store(self.params.agc_enabled.value(), Ordering::Relaxed);

        processor.process_updates();

        let channel_data = buffer.as_slice();
        let num_channels = channel_data.len();
        if num_channels == 0 {
             return ProcessStatus::Normal;
        }
        let num_samples = channel_data[0].len();
        
        // 1. Push Input to Ring Buffer
        if let Some(prod_in) = &mut self.rb_in_prod {
             for i in 0..num_samples {
                 let sample = if num_channels > 1 {
                     (channel_data[0][i] + channel_data[1][i]) * 0.5
                 } else {
                     channel_data[0][i]
                 };
                 
                 let _ = prod_in.push(sample);
             }
        }

        // 2. Process chunks if available
        if let (Some(cons_in), Some(prod_out)) = (&mut self.rb_in_cons, &mut self.rb_out_prod) {
            let mut input_frame = [0.0f32; FRAME_SIZE];
            let mut output_frame = [0.0f32; FRAME_SIZE];
            
            // ringbuf 0.2.8 len()
            while cons_in.len() >= FRAME_SIZE {
                for j in 0..FRAME_SIZE {
                     input_frame[j] = cons_in.pop().unwrap_or(0.0);
                }
                
                processor.process_frame(
                    &input_frame,
                    &mut output_frame,
                    None, 
                    self.params.suppression.value(),
                    self.params.gate_threshold.value(),
                    true, 
                );
                
                for j in 0..FRAME_SIZE {
                     let _ = prod_out.push(output_frame[j]);
                }
            }
        }

        // 3. Output from Ring Buffer
        if let Some(cons_out) = &mut self.rb_out_cons {
            for i in 0..num_samples {
                let sample = cons_out.pop().unwrap_or(0.0);
                
                if num_channels >= 1 {
                     channel_data[0][i] = sample;
                }
                if num_channels >= 2 {
                     channel_data[1][i] = sample;
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
