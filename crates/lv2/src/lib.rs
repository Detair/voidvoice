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

        // 2. Push Input (stack-allocated, avoids heap per-callback)
        let num_samples = ports.input_l.len();
        let mut input_l = [0.0f32; 8192];
        let mut input_r = [0.0f32; 8192];
        let n = num_samples.min(8192);
        input_l[..n].copy_from_slice(&ports.input_l[..n]);
        input_r[..n].copy_from_slice(&ports.input_r[..n]);
        self.adapter.push_stereo_interleaved(&input_l[..n], &input_r[..n]);

        // 3. Process available frames
        self.adapter.process_available(
            &mut self.processor,
            suppression,
            threshold,
            false,
        );

        // 4. Fill Output
        let mut out_l = [0.0f32; 8192];
        let mut out_r = [0.0f32; 8192];
        self.adapter.pop_stereo(&mut out_l[..n], &mut out_r[..n]);

        ports.output_l[..n].copy_from_slice(&out_l[..n]);
        ports.output_r[..n].copy_from_slice(&out_r[..n]);
    }
}

lv2_descriptors!(VoidMic);
