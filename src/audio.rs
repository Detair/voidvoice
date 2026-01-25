use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split, Observer};
use ringbuf::HeapRb;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::path::Path;
use nnnoiseless::DenoiseState;

const SAMPLE_RATE: u32 = 48000;
const FRAME_SIZE: usize = 480; // 10ms frames (480 samples at 48kHz)

// Gate Threshold: -36dB in linear amplitude
// Derived from testing with mechanical keyboards and sim racing gear.
// Typical speech RMS: 0.02-0.15, keyboard clicks (post-RNNoise): 0.005-0.012
// Setting threshold at 0.015 provides good separation while avoiding false triggers.
const GATE_THRESHOLD: f32 = 0.015;

// Gate timing constants (all in milliseconds)
const ATTACK_MS: u32 = 5;      // Fast attack to avoid clipping first syllable
const RELEASE_MS: u32 = 200;   // Hold gate open 200ms after speech stops (natural pauses)
const FADE_MS: u32 = 10;       // Fade duration to eliminate clicks/pops

/// Audio processing engine that combines RNNoise denoising with a smart noise gate.
/// 
/// The engine runs in a separate thread and processes audio in real-time using:
/// 1. RNNoise for steady background noise removal
/// 2. Smart gate with attack/release for transient noise suppression
pub struct AudioEngine {
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    is_running: Arc<AtomicBool>,
    pub volume_level: Arc<AtomicU32>,
}

impl AudioEngine {
    /// Starts the audio processing engine.
    /// 
    /// # Arguments
    /// * `input_name` - Name of the input device ("default" for system default)
    /// * `output_name` - Name of the output device ("default" for system default)
    /// * `_model_dir` - Unused (kept for API compatibility; RNNoise weights are embedded)
    /// 
    /// # Returns
    /// Result containing the AudioEngine instance or an error
    /// 
    /// # Example
    /// ```no_run
    /// let engine = AudioEngine::start("default", "VoidMic_Clean", Path::new("."))?;
    /// ```
    pub fn start(input_name: &str, output_name: &str, _model_dir: &Path) -> Result<Self> {
        let host = cpal::default_host();
        
        let input_device = if input_name == "default" {
            host.default_input_device().context("No default input found")?
        } else {
            host.input_devices()?.find(|d| d.name().ok().as_deref() == Some(input_name))
                .context("Input device not found")?
        };

        let output_device = if output_name == "default" {
            host.default_output_device().context("No default output found")?
        } else {
            host.output_devices()?.find(|d| d.name().ok().as_deref() == Some(output_name))
                .context("Output device not found")?
        };

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        // Use 100ms buffers for low latency (critical for gaming)
        // 4800 samples = 100ms at 48kHz
        let buffer_size = (SAMPLE_RATE as usize) / 10;
        
        let rb_in = HeapRb::<f32>::new(buffer_size);
        let (mut prod_in, mut cons_in) = rb_in.split();

        let rb_out = HeapRb::<f32>::new(buffer_size);
        let (mut prod_out, mut cons_out) = rb_out.split();

        let input_stream = input_device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                let _ = prod_in.push_slice(data);
            },
            |err| eprintln!("Input error: {}", err),
            None,
        )?;

        let output_stream = output_device.build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                let read = cons_out.pop_slice(data);
                if read < data.len() {
                   for i in read..data.len() {
                       data[i] = 0.0;
                   }
                }
            },
            |err| eprintln!("Output error: {}", err),
            None,
        )?;

        let is_running = Arc::new(AtomicBool::new(true));
        let run_flag = is_running.clone();
        let volume_level = Arc::new(AtomicU32::new(0));
        let volume_writer = volume_level.clone();

        thread::spawn(move || {
            let mut denoise = DenoiseState::new();
            let mut input_frame = [0.0f32; FRAME_SIZE];
            let mut output_frame = [0.0f32; FRAME_SIZE];
            
            // Gate State Machine
            let mut gate_open = false;
            let mut samples_since_close = 0u32;  // For attack time
            let mut samples_since_open = 0u32;   // For release time
            let mut fade_position = 0u32;        // For smooth fade-out
            
            let attack_samples = (SAMPLE_RATE / 1000) * ATTACK_MS;
            let release_samples = (SAMPLE_RATE / 1000) * RELEASE_MS;
            let fade_samples = (SAMPLE_RATE / 1000) * FADE_MS;

            while run_flag.load(Ordering::Relaxed) {
                if cons_in.occupied_len() >= FRAME_SIZE {
                    cons_in.pop_slice(&mut input_frame);

                    // 1. Denoise (RNNoise)
                    denoise.process_frame(&mut output_frame, &input_frame);
                    
                    // 2. Smart Gate Logic with Attack/Release
                    // Calculate RMS of the *denoised* signal
                    let sum: f32 = output_frame.iter().map(|x| x * x).sum();
                    let rms = (sum / FRAME_SIZE as f32).sqrt();
                    volume_writer.store(rms.to_bits(), Ordering::Relaxed);

                    // Gate decision with hysteresis
                    if rms > GATE_THRESHOLD {
                        samples_since_close += FRAME_SIZE as u32;
                        
                        // Open gate after attack time (prevents clipping first syllable)
                        if samples_since_close >= attack_samples {
                            gate_open = true;
                            samples_since_open = 0;
                            fade_position = 0; // Reset fade
                        }
                    } else {
                        samples_since_close = 0;
                        
                        if gate_open {
                            samples_since_open += FRAME_SIZE as u32;
                            
                            // Close gate after release time
                            if samples_since_open > release_samples {
                                gate_open = false;
                            }
                        }
                    }

                    // 3. Apply Gate with Smooth Fade-Out
                    if !gate_open {
                        // Smooth fade-out to eliminate clicks/pops
                        for (_i, sample) in output_frame.iter_mut().enumerate() {
                            if fade_position < fade_samples {
                                // Linear fade (could use cosine for smoother curve)
                                let fade_gain = 1.0 - (fade_position as f32 / fade_samples as f32);
                                *sample *= fade_gain;
                                fade_position += 1;
                            } else {
                                *sample = 0.0;
                            }
                        }
                    } else {
                        fade_position = 0; // Reset fade when gate is open
                    }

                    while prod_out.vacant_len() < FRAME_SIZE {
                        thread::sleep(Duration::from_micros(500));
                    }
                    prod_out.push_slice(&output_frame);
                } else {
                    thread::sleep(Duration::from_micros(200));
                }
            }
        });

        input_stream.play()?;
        output_stream.play()?;

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            is_running,
            volume_level,
        })
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::Relaxed);
    }
}