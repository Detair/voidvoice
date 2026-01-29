//! Shared constants for VoidMic audio processing.

/// Sample rate used throughout VoidMic (48kHz)
pub const SAMPLE_RATE: u32 = 48000;

/// Frame size in samples (10ms at 48kHz = 480 samples)
pub const FRAME_SIZE: usize = 480;
