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
use crate::echo_cancel::EchoCanceller;


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

/// Tracks minimum RMS over a sliding window to estimate noise floor.
struct NoiseFloorTracker {
    window: Vec<f32>,
    window_size: usize,
    current_floor: f32,
}

impl NoiseFloorTracker {
    fn new(window_seconds: f32) -> Self {
        // Window size in frames (10ms frames)
        let window_size = (window_seconds * 100.0) as usize; // e.g., 3s = 300 frames
        Self {
            window: Vec::with_capacity(window_size),
            window_size,
            current_floor: 0.01, // Initial estimate
        }
    }
    
    fn update(&mut self, rms: f32) {
        self.window.push(rms);
        if self.window.len() > self.window_size {
            self.window.remove(0);
        }
        // Use 10th percentile as noise floor estimate (robust to speech)
        if self.window.len() >= 10 {
            let mut sorted: Vec<f32> = self.window.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let idx = sorted.len() / 10; // 10th percentile
            self.current_floor = sorted[idx];
        }
    }
    
    fn floor(&self) -> f32 {
        self.current_floor
    }
}

/// Audio processing engine that combines RNNoise denoising with a smart noise gate.
/// 
/// The engine runs in a separate thread and processes audio in real-time using:
/// 1. RNNoise for steady background noise removal
/// 2. Smart gate with attack/release for transient noise suppression
pub struct AudioEngine {
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    _reference_stream: Option<cpal::Stream>,
    is_running: Arc<AtomicBool>,
    pub volume_level: Arc<AtomicU32>,
    pub calibration_mode: Arc<AtomicBool>,
    pub calibration_result: Arc<AtomicU32>,
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
    /// let engine = AudioEngine::start("default", "VoidMic_Clean", Path::new("."), 0.015, 1.0, false, None)?;
    /// ```
    pub fn start(
        input_name: &str, 
        output_name: &str, 
        _model_dir: &Path, 
        gate_threshold: f32, 
        suppression_strength: f32,
        echo_cancel_enabled: bool,
        reference_device_name: Option<&str>,
        dynamic_threshold_enabled: bool
    ) -> Result<Self> {
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

        // Reference Stream Ring Buffer (for Echo Cancellation)
        let (mut prod_ref, mut cons_ref) = if echo_cancel_enabled {
            let rb_ref = HeapRb::<f32>::new(buffer_size);
            let (p, c) = rb_ref.split();
            (Some(p), Some(c))
        } else {
            (None, None)
        };


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
        
        let _reference_stream = if echo_cancel_enabled {
            // Setup reference stream (input from speaker monitor)
            let ref_name = reference_device_name.unwrap_or("default");
            
            // Try to find device
            let ref_device = if ref_name == "default" {
                 // Try to find default input? No.
                 // We require explicit selection or rely on system default input which is likely Mic.
                 // If echo cancel is enabled but no ref device, we try to capture default output monitor.
                 // On PulseAudio, this is tricky via cpal.
                 // For now, we assume user selects the monitor as an input device.
                 host.input_devices()?.find(|d| d.name().ok().as_deref() == Some(ref_name))
            } else {
                 host.input_devices()?.find(|d| d.name().ok().as_deref() == Some(ref_name))
            };
            
            if let Some(device) = ref_device {
                let stream = device.build_input_stream(
                    &config,
                    move |data: &[f32], _| {
                         if let Some(prod) = &mut prod_ref {
                             let _ = prod.push_slice(data);
                         }
                    },
                    |err| eprintln!("Reference input error: {}", err),
                    None,
                ).ok();
                stream
            } else {
                eprintln!("Reference device '{}' not found", ref_name);
                None
            }
        } else {
            None
        };
        
        if let Some(stream) = &_reference_stream {
             stream.play()?;
        }


        let is_running = Arc::new(AtomicBool::new(true));
        let run_flag = is_running.clone();
        let volume_level = Arc::new(AtomicU32::new(0));
        let volume_writer = volume_level.clone();
        let calibration_mode = Arc::new(AtomicBool::new(false));
        let calibration_mode_reader = calibration_mode.clone();
        let calibration_result = Arc::new(AtomicU32::new(0));
        let calibration_result_writer = calibration_result.clone();

        thread::spawn(move || {
            let mut denoise = DenoiseState::new();
            let mut echo_canceller = if echo_cancel_enabled {
                 Some(EchoCanceller::new())
            } else {
                 None
            };
            
            let mut input_frame = [0.0f32; FRAME_SIZE];
            let mut output_frame = [0.0f32; FRAME_SIZE];
            
            // Gate State Machine
            let mut gate_open = false;
            let mut samples_since_close = 0u32;  // For attack time
            let mut samples_since_open = 0u32;   // For release time
            let mut fade_position = 0u32;        // For smooth fade-out
            
            // Calibration state
            let mut calibration_samples: Vec<f32> = Vec::new();
            let calibration_duration_samples = SAMPLE_RATE * 3; // 3 seconds
            
            let attack_samples = (SAMPLE_RATE / 1000) * ATTACK_MS;
            let release_samples = (SAMPLE_RATE / 1000) * RELEASE_MS;
            let fade_samples = (SAMPLE_RATE / 1000) * FADE_MS;
            
            // Noise floor tracker for dynamic threshold
            let mut noise_floor_tracker = NoiseFloorTracker::new(3.0); // 3 second window

            while run_flag.load(Ordering::Relaxed) {
                if cons_in.occupied_len() >= FRAME_SIZE {
                    cons_in.pop_slice(&mut input_frame);

                     // 1. Echo Cancellation
                    if let (Some(cons), Some(aec)) = (&mut cons_ref, &mut echo_canceller) {
                        // We need reference samples to match input frame
                        if cons.occupied_len() >= FRAME_SIZE {
                             // Read reference frame
                             let mut ref_frame = [0.0f32; FRAME_SIZE];
                             cons.pop_slice(&mut ref_frame);
                             
                             // Process through AEC
                             let processed = aec.process_frame(&input_frame, &ref_frame);
                             input_frame.copy_from_slice(&processed);
                        } else {
                            // Underrun
                        }
                    }

                    // 2. Denoise (RNNoise)
                    denoise.process_frame(&mut output_frame, &input_frame);
                    
                    // 1b. Blend raw and denoised based on suppression strength
                    // strength=1.0 means full suppression, strength=0.0 means raw audio
                    for i in 0..FRAME_SIZE {
                        output_frame[i] = input_frame[i] * (1.0 - suppression_strength) 
                                        + output_frame[i] * suppression_strength;
                    }
                    
                    // 2. Smart Gate Logic with Attack/Release
                    // Calculate RMS of the *denoised* signal
                    let sum: f32 = output_frame.iter().map(|x| x * x).sum();
                    let rms = (sum / FRAME_SIZE as f32).sqrt();
                    volume_writer.store(rms.to_bits(), Ordering::Relaxed);

                    // Calibration mode: collect RMS samples
                    if calibration_mode_reader.load(Ordering::Relaxed) {
                        calibration_samples.push(rms);
                        if calibration_samples.len() >= (calibration_duration_samples / FRAME_SIZE as u32) as usize {
                            // Calculate suggested threshold: max RMS + 20% margin
                            let max_rms = calibration_samples.iter().cloned().fold(0.0f32, f32::max);
                            let suggested = (max_rms * 1.2).max(0.005); // minimum 0.005
                            calibration_result_writer.store(suggested.to_bits(), Ordering::Relaxed);
                            calibration_mode_reader.store(false, Ordering::Relaxed);
                            calibration_samples.clear();
                        }
                    }

                    // Gate decision with hysteresis
                    // Dynamic threshold: floor * 1.5 + margin, clamped to safe range
                    let effective_threshold = if dynamic_threshold_enabled {
                        noise_floor_tracker.update(rms);
                        let dynamic = noise_floor_tracker.floor() * 1.5 + 0.003;
                        dynamic.clamp(0.005, 0.08)
                    } else {
                        gate_threshold
                    };
                    
                    if rms > effective_threshold {
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
             _reference_stream,
            is_running,
            volume_level,
            calibration_mode,
            calibration_result,
        })
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::Relaxed);
    }
}

/// Output filter engine for speaker/headphone denoising.
/// 
/// Captures audio from a source (e.g., application output) and applies RNNoise
/// before sending to the actual speakers. Introduces ~100ms latency.
pub struct OutputFilterEngine {
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    is_running: Arc<AtomicBool>,
}

impl OutputFilterEngine {
    /// Starts the output filter engine.
    /// 
    /// # Arguments
    /// * `source_name` - Name of the source to filter (e.g., application output monitor)
    /// * `sink_name` - Name of the sink to output filtered audio to (e.g., speakers)
    /// * `suppression_strength` - Strength of noise suppression (0.0-1.0)
    pub fn start(source_name: &str, sink_name: &str, suppression_strength: f32) -> Result<Self> {
        let host = cpal::default_host();
        
        // Use monitor source as input (captures what apps are playing)
        let input_device = if source_name == "default" {
            host.default_input_device().context("No default input found")?
        } else {
            host.input_devices()?.find(|d| d.name().ok().as_deref() == Some(source_name))
                .context("Source device not found")?
        };

        let output_device = if sink_name == "default" {
            host.default_output_device().context("No default output found")?
        } else {
            host.output_devices()?.find(|d| d.name().ok().as_deref() == Some(sink_name))
                .context("Output device not found")?
        };

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        // Use larger buffer for output filtering (100ms acceptable latency)
        let buffer_size = (SAMPLE_RATE as usize) / 5; // 200ms buffer
        
        let rb_in = HeapRb::<f32>::new(buffer_size);
        let (mut prod_in, mut cons_in) = rb_in.split();

        let rb_out = HeapRb::<f32>::new(buffer_size);
        let (mut prod_out, mut cons_out) = rb_out.split();

        let input_stream = input_device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                let _ = prod_in.push_slice(data);
            },
            |err| eprintln!("Output filter input error: {}", err),
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
            |err| eprintln!("Output filter output error: {}", err),
            None,
        )?;

        let is_running = Arc::new(AtomicBool::new(true));
        let run_flag = is_running.clone();

        thread::spawn(move || {
            let mut denoise = DenoiseState::new();
            let mut input_frame = [0.0f32; FRAME_SIZE];
            let mut output_frame = [0.0f32; FRAME_SIZE];

            while run_flag.load(Ordering::Relaxed) {
                if cons_in.occupied_len() >= FRAME_SIZE {
                    cons_in.pop_slice(&mut input_frame);

                    // Denoise with RNNoise
                    denoise.process_frame(&mut output_frame, &input_frame);
                    
                    // Blend based on suppression strength
                    for i in 0..FRAME_SIZE {
                        output_frame[i] = input_frame[i] * (1.0 - suppression_strength) 
                                        + output_frame[i] * suppression_strength;
                    }

                    while prod_out.vacant_len() < FRAME_SIZE {
                        thread::sleep(Duration::from_micros(500));
                    }
                    prod_out.push_slice(&output_frame);
                } else {
                    thread::sleep(Duration::from_micros(500));
                }
            }
        });

        input_stream.play()?;
        output_stream.play()?;

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            is_running,
        })
    }
}

impl Drop for OutputFilterEngine {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::Relaxed);
    }
}