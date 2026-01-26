//! Echo cancellation module for VoidMic.
//!
//! Uses the aec3 crate (Rust port of WebRTC AEC3) for acoustic echo cancellation.

use aec3::voip::VoipAec3;

/// Sample rate for echo cancellation
const SAMPLE_RATE: usize = 48000;

/// Echo canceller wrapper
pub struct EchoCanceller {
    aec: VoipAec3,
}

impl EchoCanceller {
    /// Creates a new echo canceller.
    pub fn new() -> Self {
        // Create AEC3 with default settings (48kHz, 1 channel)
        let aec = VoipAec3::builder(SAMPLE_RATE, 1, 1)
            .build()
            .expect("Failed to create AEC3");
        Self { aec }
    }

    /// Processes a frame of audio with echo cancellation.
    /// 
    /// # Arguments
    /// * `mic_input` - The microphone input (may contain echo). Expected length: 480 (10ms at 48kHz)
    /// * `speaker_ref` - The reference signal from speakers. Expected length: 480
    /// 
    /// # Returns
    /// The echo-cancelled microphone signal
    pub fn process_frame(&mut self, mic_input: &[f32], speaker_ref: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0; mic_input.len()];
        
        // Process with AEC3
        // level_change = false (we don't track volume changes yet)
        if let Err(e) = self.aec.process(mic_input, Some(speaker_ref), false, &mut out) {
            eprintln!("AEC error: {:?}", e);
            return mic_input.to_vec(); // Fallback to raw input
        }
        
        out
    }
    
    /// Resets the echo canceller state.
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.aec = VoipAec3::builder(SAMPLE_RATE, 1, 1)
            .build()
            .expect("Failed to reset AEC3");
    }
}

impl Default for EchoCanceller {
    fn default() -> Self {
        Self::new()
    }
}
