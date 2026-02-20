use lv2::prelude::*;
use std::sync::atomic::Ordering;
use voidmic_core::constants::SAMPLE_RATE;
use voidmic_core::{FrameAdapter, VoidProcessor};

#[derive(PortCollection)]
struct VoidMicPorts {
    input_l: InputPort<Audio>,
    input_r: InputPort<Audio>,
    output_l: OutputPort<Audio>,
    output_r: OutputPort<Audio>,
    threshold: InputPort<Control>,
    suppression: InputPort<Control>,
    bypass: InputPort<Control>,
}

#[uri("https://github.com/Detair/voidvoice/lv2/voidmic")]
struct VoidMic {
    processor: VoidProcessor,
    adapter: FrameAdapter,
}

// Safety: LV2 hosts guarantee that Plugin::run() is called from a single audio thread.
// The VoidMic struct is never actually shared across threads — the Sync bound is a
// requirement of the lv2::Plugin trait but not exercised at runtime.
unsafe impl Sync for VoidMic {}

impl Plugin for VoidMic {
    type Ports = VoidMicPorts;
    type InitFeatures = ();
    type AudioFeatures = ();

    fn new(_info: &PluginInfo, _features: &mut ()) -> Option<Self> {
        // Validate sample rate - VoidMic requires 48kHz
        if _info.sample_rate() as u32 != SAMPLE_RATE {
            eprintln!(
                "VoidMic LV2: requires {}Hz sample rate, host is using {}Hz",
                SAMPLE_RATE,
                _info.sample_rate()
            );
            return None;
        }

        let processor = VoidProcessor::new(
            2,               // Channels: Stereo
            2,               // VAD sensitivity: Aggressive
            (0.0, 0.0, 0.0), // No EQ default
            0.7,             // AGC Target
            false,           // Echo Cancel disabled
        );

        Some(Self {
            processor,
            adapter: FrameAdapter::new(),
        })
    }

    fn run(&mut self, ports: &mut VoidMicPorts, _features: &mut (), _sample_count: u32) {
        // 1. Sync Parameters
        let threshold = *ports.threshold;
        let suppression = *ports.suppression;
        let bypass = *ports.bypass > 0.5;

        self.processor
            .bypass_enabled
            .store(bypass, Ordering::Relaxed);
        self.processor.process_updates();

        // 2. Push Input
        let input_l: Vec<f32> = ports.input_l.iter().copied().collect();
        let input_r: Vec<f32> = ports.input_r.iter().copied().collect();
        self.adapter.push_stereo_interleaved(&input_l, &input_r);

        // 3. Process available frames
        self.adapter.process_available(
            &mut self.processor,
            suppression,
            threshold,
            false, // Use explicit threshold from control port, not dynamic
        );

        // 4. Fill Output
        let num_samples = ports.output_l.len();
        let mut out_l = vec![0.0f32; num_samples];
        let mut out_r = vec![0.0f32; num_samples];
        self.adapter.pop_stereo(&mut out_l, &mut out_r);

        for (dst, src) in ports.output_l.iter_mut().zip(out_l.iter()) {
            *dst = *src;
        }
        for (dst, src) in ports.output_r.iter_mut().zip(out_r.iter()) {
            *dst = *src;
        }
    }
}

lv2_descriptors!(VoidMic);
