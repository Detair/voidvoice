//! Frame adapter for bridging variable-size host buffers to fixed-size processor frames.
//!
//! Encapsulates the ring buffer plumbing shared between plugin frontends (VST3, CLAP, LV2).

use crate::constants::FRAME_SIZE;
use crate::processor::VoidProcessor;
use ringbuf::traits::{Consumer, Observer, Producer};
use ringbuf::HeapRb;

/// Bridges variable-size audio buffers from plugin hosts to fixed-size
/// `FRAME_SIZE` stereo frames expected by `VoidProcessor`.
///
/// Internally uses two ring buffers (input and output) to accumulate/drain
/// samples without blocking.
pub struct FrameAdapter {
    rb_in: HeapRb<f32>,
    rb_out: HeapRb<f32>,
    left_in: [f32; FRAME_SIZE],
    right_in: [f32; FRAME_SIZE],
    left_out: [f32; FRAME_SIZE],
    right_out: [f32; FRAME_SIZE],
}

impl Default for FrameAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameAdapter {
    /// Creates a new adapter with ring buffers sized for the given channel count.
    pub fn new() -> Self {
        let buffer_size = FRAME_SIZE * 4 * 2; // Always stereo
        Self {
            rb_in: HeapRb::<f32>::new(buffer_size),
            rb_out: HeapRb::<f32>::new(buffer_size),
            left_in: [0.0; FRAME_SIZE],
            right_in: [0.0; FRAME_SIZE],
            left_out: [0.0; FRAME_SIZE],
            right_out: [0.0; FRAME_SIZE],
        }
    }

    /// Pushes interleaved stereo sample pairs into the input ring buffer.
    pub fn push_stereo_interleaved(&mut self, left: &[f32], right: &[f32]) {
        let len = left.len().min(right.len());
        for i in 0..len {
            let _ = self.rb_in.try_push(left[i]);
            let _ = self.rb_in.try_push(right[i]);
        }
    }

    /// Pushes mono samples, duplicating each to both stereo channels.
    pub fn push_mono(&mut self, mono: &[f32]) {
        for &sample in mono {
            let _ = self.rb_in.try_push(sample);
            let _ = self.rb_in.try_push(sample);
        }
    }

    /// Processes all complete stereo frames available in the input buffer
    /// through the given `VoidProcessor`, pushing results to the output buffer.
    pub fn process_available(
        &mut self,
        processor: &mut VoidProcessor,
        suppression: f32,
        threshold: f32,
        dynamic_threshold: bool,
    ) {
        // Need 2 * FRAME_SIZE samples for a full stereo frame
        while self.rb_in.occupied_len() >= FRAME_SIZE * 2 {
            for j in 0..FRAME_SIZE {
                self.left_in[j] = self.rb_in.try_pop().unwrap_or(0.0);
                self.right_in[j] = self.rb_in.try_pop().unwrap_or(0.0);
            }

            processor.process_frame(
                &[&self.left_in, &self.right_in],
                &mut [&mut self.left_out, &mut self.right_out],
                None,
                suppression,
                threshold,
                dynamic_threshold,
            );

            for j in 0..FRAME_SIZE {
                let _ = self.rb_out.try_push(self.left_out[j]);
                let _ = self.rb_out.try_push(self.right_out[j]);
            }
        }
    }

    /// Pops processed stereo output samples. Returns the number of sample pairs written.
    pub fn pop_stereo(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        let len = left.len().min(right.len());
        let mut count = 0;
        for i in 0..len {
            if self.rb_out.occupied_len() >= 2 {
                left[i] = self.rb_out.try_pop().unwrap_or(0.0);
                right[i] = self.rb_out.try_pop().unwrap_or(0.0);
                count += 1;
            } else {
                left[i] = 0.0;
                right[i] = 0.0;
            }
        }
        count
    }

    /// Pops processed output as mono (averages L+R). Returns number of samples written.
    pub fn pop_mono(&mut self, out: &mut [f32]) -> usize {
        let mut count = 0;
        for sample in out.iter_mut() {
            if self.rb_out.occupied_len() >= 2 {
                let l = self.rb_out.try_pop().unwrap_or(0.0);
                let r = self.rb_out.try_pop().unwrap_or(0.0);
                *sample = (l + r) * 0.5;
                count += 1;
            } else {
                *sample = 0.0;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_pop_roundtrip() {
        let mut adapter = FrameAdapter::new();
        let mut processor = VoidProcessor::new(2, 2, (0.0, 0.0, 0.0), 0.7, false);

        // Push a full stereo frame
        let left = [0.0f32; FRAME_SIZE];
        let right = [0.0f32; FRAME_SIZE];
        adapter.push_stereo_interleaved(&left, &right);

        // Process it
        adapter.process_available(&mut processor, 1.0, 0.015, false);

        // Pop it back
        let mut out_l = [0.0f32; FRAME_SIZE];
        let mut out_r = [0.0f32; FRAME_SIZE];
        let count = adapter.pop_stereo(&mut out_l, &mut out_r);
        assert_eq!(count, FRAME_SIZE);
    }

    #[test]
    fn test_mono_duplication() {
        let mut adapter = FrameAdapter::new();
        let mono = [0.5f32; 4];
        adapter.push_mono(&mono);
        // Should have 8 samples in rb_in (4 pairs)
        assert_eq!(adapter.rb_in.occupied_len(), 8);
    }

    #[test]
    fn test_partial_frame_does_not_process() {
        let mut adapter = FrameAdapter::new();
        let mut processor = VoidProcessor::new(2, 2, (0.0, 0.0, 0.0), 0.7, false);

        // Push less than a full frame
        let partial = [0.1f32; FRAME_SIZE / 2];
        adapter.push_stereo_interleaved(&partial, &partial);

        // Process — should not produce output since not enough for a frame
        adapter.process_available(&mut processor, 1.0, 0.015, false);
        assert_eq!(adapter.rb_out.occupied_len(), 0);
    }
}
