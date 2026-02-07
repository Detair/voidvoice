//! Echo cancellation module for VoidMic.
//!
//! Uses the aec3 crate (Rust port of WebRTC AEC3) for acoustic echo cancellation.

use crate::constants::{FRAME_SIZE, SAMPLE_RATE};
use aec3::voip::VoipAec3;

/// Echo canceller wrapper
pub struct EchoCanceller {
    aec: VoipAec3,
    output_buffer: [f32; FRAME_SIZE], // Pre-allocated to avoid heap allocation
}

impl EchoCanceller {
    /// Creates a new echo canceller. Returns None if AEC3 initialization fails.
    pub fn new() -> Option<Self> {
        let aec = VoipAec3::builder(SAMPLE_RATE as usize, 1, 1)
            .build()
            .ok()?;
        Some(Self {
            aec,
            output_buffer: [0.0; FRAME_SIZE],
        })
    }

    /// Processes a frame of audio with echo cancellation.
    ///
    /// # Arguments
    /// * `mic_input` - The microphone input (may contain echo). Expected length: 480 (10ms at 48kHz)
    /// * `speaker_ref` - The reference signal from speakers. Expected length: 480
    /// * `output` - Output buffer to write echo-cancelled signal to
    ///
    /// # Returns
    /// `true` if processing succeeded, `false` if fallback to raw input was used
    pub fn process_frame(&mut self, mic_input: &[f32], speaker_ref: &[f32], output: &mut [f32]) -> bool {
        // Clear output buffer
        self.output_buffer.fill(0.0);

        // Process with AEC3
        // level_change = false (we don't track volume changes yet)
        if let Err(e) = self
            .aec
            .process(mic_input, Some(speaker_ref), false, &mut self.output_buffer)
        {
            log::warn!("AEC error: {:?}", e);
            output.copy_from_slice(mic_input); // Fallback to raw input
            return false;
        }

        output.copy_from_slice(&self.output_buffer);
        true
    }

    /// Resets the echo canceller state. Returns false if re-initialization fails.
    #[allow(dead_code)]
    pub fn reset(&mut self) -> bool {
        match VoipAec3::builder(SAMPLE_RATE as usize, 1, 1).build() {
            Ok(aec) => {
                self.aec = aec;
                true
            }
            Err(_) => false,
        }
    }
}


