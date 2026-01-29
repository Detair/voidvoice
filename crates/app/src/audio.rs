use voidmic_core::constants::{FRAME_SIZE, SAMPLE_RATE};
use voidmic_core::VoidProcessor;
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use log::{info, warn};
use voidmic_core::DenoiseState;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use crossbeam_channel::Sender;

// Gate timing constants (all in milliseconds)

/// Audio processing engine that combines RNNoise denoising with a smart noise gate.
///
/// The engine runs in a separate thread and processes audio in real-time using VoidProcessor.
pub struct AudioEngine {
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    _reference_stream: Option<cpal::Stream>,
    is_running: Arc<AtomicBool>,
    
    // Shared state for GUI communication
    pub volume_level: Arc<AtomicU32>,
    pub calibration_mode: Arc<AtomicBool>,
    pub calibration_result: Arc<AtomicU32>,
    
    pub vad_sensitivity: Arc<AtomicU32>,
    pub eq_low_gain: Arc<AtomicU32>,
    pub eq_mid_gain: Arc<AtomicU32>,
    pub eq_high_gain: Arc<AtomicU32>,
    
    pub agc_enabled: Arc<AtomicBool>,
    pub agc_target: Arc<AtomicU32>,
    pub bypass_enabled: Arc<AtomicBool>,
    pub jitter_max_us: Arc<AtomicU32>,
    pub spectrum_sender: Option<Sender<(Vec<f32>, Vec<f32>)>>,
}

impl AudioEngine {
    /// Starts the audio engine.
    pub fn start(
        input_device_name: &str,
        output_device_name: &str,
        _model_path: &Path, // Kept for API compatibility
        gate_threshold: f32,
        suppression_strength: f32,
        echo_cancel_enabled: bool,
        _reference_device_name: Option<&str>,
        dynamic_threshold_enabled: bool,
        vad_sensitivity: i32,
        eq_enabled: bool,
        eq_params: (f32, f32, f32), // Low, Mid, High gains in dB
        agc_enabled: bool,
        agc_target_level: f32,
        bypass_enabled: bool,
        spectrum_sender: Option<Sender<(Vec<f32>, Vec<f32>)>>,
    ) -> Result<Self> {
        let host = cpal::default_host();
        info!("Audio host: {}", host.id().name());

        let input_device = if input_device_name == "default" {
            host.default_input_device()
                .context("No default input found")?
        } else {
            host.input_devices()?
                .find(|d| d.name().ok().as_deref() == Some(input_device_name))
                .context("Input device not found")?
        };
        info!("Using input device: {}", input_device.name().unwrap_or_default());

        let output_device = if output_device_name == "default" {
            host.default_output_device()
                .context("No default output found")?
        } else {
            host.output_devices()?
                .find(|d| d.name().ok().as_deref() == Some(output_device_name))
                .context("Output device not found")?
        };
        info!("Using output device: {}", output_device.name().unwrap_or_default());

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        // Latency management (100ms buffer)
        let buffer_size = (SAMPLE_RATE as usize) / 10;
        let latency_frames = buffer_size; // For clarity, though unused

        // Ring buffers
        let rb_in = HeapRb::<f32>::new(buffer_size);
        let (mut prod_in, mut cons_in) = rb_in.split();

        let rb_out = HeapRb::<f32>::new(buffer_size);
        let (mut prod_out, mut cons_out) = rb_out.split();
        
        let _reference_stream: Option<cpal::Stream> = None;

        let input_stream = input_device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                let _ = prod_in.push_slice(data);
            },
            |err| warn!("Input error: {}", err),
            None,
        )?;

        let output_stream = output_device.build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                let read = cons_out.pop_slice(data);
                if read < data.len() {
                    for sample in data.iter_mut().skip(read) {
                        *sample = 0.0;
                    }
                }
            },
            |err| warn!("Output error: {}", err),
            None,
        )?;

        // Initialize Processor
        let mut processor = VoidProcessor::new(
            vad_sensitivity,
            if eq_enabled { eq_params } else { (0.0, 0.0, 0.0) },
            agc_target_level,
            echo_cancel_enabled,
        );

        // Set initial state
        // processor.gate_threshold is passed in process_frame, we don't set it here explicitly?
        // Wait, processor doesn't store threshold. It's passed in process_frame.
        // But we need it for the thread loop?
        // In the thread loop: start with the `gate_threshold` argument.
        processor.agc_enabled.store(agc_enabled, Ordering::Relaxed);
        processor.bypass_enabled.store(bypass_enabled, Ordering::Relaxed);
        if let Some(sender) = spectrum_sender.clone() {
            processor.spectrum_sender = Some(sender);
        }

        // Extract Atomics for GUI
        let volume_level = processor.volume_level.clone();
        let calibration_mode = processor.calibration_mode.clone();
        let calibration_result = processor.calibration_result.clone();
        let vad_sensitivity_atomic = processor.vad_sensitivity.clone();
        let eq_low_atomic = processor.eq_low_gain.clone();
        let eq_mid_atomic = processor.eq_mid_gain.clone();
        let eq_high_atomic = processor.eq_high_gain.clone();
        let agc_enabled_atomic = processor.agc_enabled.clone();
        let agc_target_atomic = processor.agc_target.clone();
        let bypass_enabled_atomic = processor.bypass_enabled.clone();
        let jitter_atomic = processor.jitter_max_us.clone();

        let is_running = Arc::new(AtomicBool::new(true));
        let run_flag = is_running.clone();

        thread::spawn(move || {
            let mut input_frame = [0.0f32; FRAME_SIZE];
            let mut output_frame = [0.0f32; FRAME_SIZE];
            
            // Jitter State
            let mut last_loop_time = std::time::Instant::now();
            let mut jitter_accum = 0;
            let mut frames_since_jitter_reset = 0;

            loop {
                if !run_flag.load(Ordering::Relaxed) {
                    break;
                }

                // Process updates
                processor.process_updates();

                if cons_in.occupied_len() >= FRAME_SIZE {
                    // Jitter Calculation
                    let now = std::time::Instant::now();
                    let loop_delta = now.duration_since(last_loop_time).as_micros() as u32;
                    last_loop_time = now;

                    let expected = 10_000;
                    let jitter = if loop_delta > expected {
                        loop_delta - expected
                    } else {
                        expected - loop_delta
                    };
                    
                    if jitter > jitter_accum {
                         jitter_accum = jitter;
                    }
                    
                    frames_since_jitter_reset += 1;
                    if frames_since_jitter_reset >= 100 {
                         processor.jitter_max_us.store(jitter_accum, Ordering::Relaxed);
                         jitter_accum = 0;
                         frames_since_jitter_reset = 0;
                    }

                    // Read Audio
                    cons_in.pop_slice(&mut input_frame);
                    
                    // Process Audio
                    processor.process_frame(
                        &input_frame,
                        &mut output_frame,
                        None, // No reference frame yet
                        suppression_strength,
                        gate_threshold,
                        dynamic_threshold_enabled,
                    );

                    // Write Audio
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
            _reference_stream: None,
            is_running,
            volume_level,
            calibration_mode,
            calibration_result,
            vad_sensitivity: vad_sensitivity_atomic,
            eq_low_gain: eq_low_atomic,
            eq_mid_gain: eq_mid_atomic,
            eq_high_gain: eq_high_atomic,
            agc_enabled: agc_enabled_atomic,
            agc_target: agc_target_atomic,
            bypass_enabled: bypass_enabled_atomic,
            spectrum_sender, 
            jitter_max_us: jitter_atomic,
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
            host.default_input_device()
                .context("No default input found")?
        } else {
            host.input_devices()?
                .find(|d| d.name().ok().as_deref() == Some(source_name))
                .context("Source device not found")?
        };

        let output_device = if sink_name == "default" {
            host.default_output_device()
                .context("No default output found")?
        } else {
            host.output_devices()?
                .find(|d| d.name().ok().as_deref() == Some(sink_name))
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
            |err| warn!("Output filter input error: {}", err),
            None,
        )?;

        let output_stream = output_device.build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                let read = cons_out.pop_slice(data);
                if read < data.len() {
                    for sample in data.iter_mut().skip(read) {
                        *sample = 0.0;
                    }
                }
            },
            |err| warn!("Output filter output error: {}", err),
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
