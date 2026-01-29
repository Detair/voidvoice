use lv2::prelude::*;
use ringbuf::{Consumer, Producer, RingBuffer};
use std::sync::atomic::Ordering;
use voidmic_core::constants::FRAME_SIZE;
use voidmic_core::VoidProcessor;

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
    rb_in_prod: Producer<f32>,
    rb_in_cons: Consumer<f32>,
    rb_out_prod: Producer<f32>,
    rb_out_cons: Consumer<f32>,
}

impl Plugin for VoidMic {
    type Ports = VoidMicPorts;
    type InitFeatures = ();
    type AudioFeatures = ();

    fn new(_info: &PluginInfo, _features: &mut ()) -> Option<Self> {
        let processor = VoidProcessor::new(
            2,               // Channels: Stereo
            2,               // VAD sensitivity: Aggressive
            (0.0, 0.0, 0.0), // No EQ default
            0.7,             // AGC Target
            false,           // Echo Cancel disabled
        );

        let buffer_size = FRAME_SIZE * 4 * 2;
        let rb_in = RingBuffer::<f32>::new(buffer_size);
        let (prod_in, cons_in) = rb_in.split();
        let rb_out = RingBuffer::<f32>::new(buffer_size);
        let (prod_out, cons_out) = rb_out.split();

        Some(Self {
            processor,
            rb_in_prod: prod_in,
            rb_in_cons: cons_in,
            rb_out_prod: prod_out,
            rb_out_cons: cons_out,
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
        let input_l = ports.input_l.iter();
        let input_r = ports.input_r.iter();

        for (l, r) in input_l.zip(input_r) {
            let _ = self.rb_in_prod.push(*l);
            let _ = self.rb_in_prod.push(*r);
        }

        // 3. Process Blocks
        let mut left_in = [0.0f32; FRAME_SIZE];
        let mut right_in = [0.0f32; FRAME_SIZE];
        let mut left_out = [0.0f32; FRAME_SIZE];
        let mut right_out = [0.0f32; FRAME_SIZE];

        while self.rb_in_cons.len() >= FRAME_SIZE * 2 {
            for j in 0..FRAME_SIZE {
                left_in[j] = self.rb_in_cons.pop().unwrap_or(0.0);
                right_in[j] = self.rb_in_cons.pop().unwrap_or(0.0);
            }

            self.processor.process_frame(
                &[&left_in, &right_in],
                &mut [&mut left_out, &mut right_out],
                None,
                suppression,
                threshold,
                true,
            );

            for j in 0..FRAME_SIZE {
                let _ = self.rb_out_prod.push(left_out[j]);
                let _ = self.rb_out_prod.push(right_out[j]);
            }
        }

        // 4. Fill Output
        let output_l = ports.output_l.iter_mut();
        let output_r = ports.output_r.iter_mut();

        for (l, r) in output_l.zip(output_r) {
            if self.rb_out_cons.len() >= 2 {
                *l = self.rb_out_cons.pop().unwrap_or(0.0);
                *r = self.rb_out_cons.pop().unwrap_or(0.0);
            } else {
                *l = 0.0;
                *r = 0.0;
            }
        }
    }
}

lv2_descriptors!(VoidMic);
