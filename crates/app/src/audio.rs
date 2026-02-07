use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Sender;
use log::{info, warn};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use voidmic_core::constants::{FRAME_SIZE, SAMPLE_RATE};
use voidmic_core::DenoiseState;
use voidmic_core::VoidProcessor;

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

    pub eq_enabled: Arc<AtomicBool>,
    pub agc_enabled: Arc<AtomicBool>,
    pub _agc_target: Arc<AtomicU32>, // Kept for potential GUI control
    pub bypass_enabled: Arc<AtomicBool>,
    pub jitter_ewma_us: Arc<AtomicU32>,
    pub gate_threshold: Arc<AtomicU32>,
    pub suppression_strength: Arc<AtomicU32>,
    pub dynamic_threshold_enabled: Arc<AtomicBool>,
    pub _spectrum_sender: Option<Sender<(Vec<f32>, Vec<f32>)>>,
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
        reference_device_name: Option<&str>,
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
        info!(
            "Using input device: {}",
            input_device.name().unwrap_or_default()
        );

        let output_device = if output_device_name == "default" {
            host.default_output_device()
                .context("No default output found")?
        } else {
            host.output_devices()?
                .find(|d| d.name().ok().as_deref() == Some(output_device_name))
                .context("Output device not found")?
        };
        info!(
            "Using output device: {}",
            output_device.name().unwrap_or_default()
        );

        // Resolve reference device for echo cancellation
        let reference_device = if echo_cancel_enabled {
            if let Some(ref_name) = reference_device_name {
                let dev = if ref_name == "default" {
                    host.default_input_device()
                } else {
                    match host.input_devices() {
                        Ok(mut devs) => devs.find(|d| d.name().ok().as_deref() == Some(ref_name)),
                        Err(e) => {
                            warn!("Failed to enumerate input devices for reference: {}", e);
                            None
                        }
                    }
                };
                if let Some(d) = &dev {
                    info!("Using reference device: {}", d.name().unwrap_or_default());
                }
                dev
            } else {
                None
            }
        } else {
            None
        };

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        // Latency management (100ms buffer)
        let buffer_size = (SAMPLE_RATE as usize) / 10;

        // Ring buffers
        let rb_in = HeapRb::<f32>::new(buffer_size);
        let (mut prod_in, mut cons_in) = rb_in.split();

        let rb_out = HeapRb::<f32>::new(buffer_size);
        let (mut prod_out, mut cons_out) = rb_out.split();

        // Reference ring buffer for echo cancellation
        let rb_ref = HeapRb::<f32>::new(buffer_size);
        let (mut prod_ref, mut cons_ref) = rb_ref.split();

        // Build reference capture stream if echo cancellation is enabled
        let reference_stream: Option<cpal::Stream> = if let Some(ref_dev) = &reference_device {
            match ref_dev.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let _ = prod_ref.push_slice(data);
                },
                |err| warn!("Reference input error: {}", err),
                None,
            ) {
                Ok(stream) => Some(stream),
                Err(e) => {
                    warn!("Failed to open reference device for echo cancellation: {}", e);
                    None
                }
            }
        } else {
            None
        };

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
        // Always pass real EQ params; eq_enabled atomic controls whether EQ runs
        let mut processor = VoidProcessor::new(
            1, // Mono for App
            vad_sensitivity,
            eq_params,
            agc_target_level,
            echo_cancel_enabled,
        );

        // Set initial state via atomics (live-updatable from GUI)
        processor
            .gate_threshold
            .store(gate_threshold.to_bits(), Ordering::Relaxed);
        processor
            .suppression_strength
            .store(suppression_strength.to_bits(), Ordering::Relaxed);
        processor
            .dynamic_threshold_enabled
            .store(dynamic_threshold_enabled, Ordering::Relaxed);
        processor.eq_enabled.store(eq_enabled, Ordering::Relaxed);
        processor.agc_enabled.store(agc_enabled, Ordering::Relaxed);
        processor
            .bypass_enabled
            .store(bypass_enabled, Ordering::Relaxed);
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
        let eq_enabled_atomic = processor.eq_enabled.clone();
        let agc_enabled_atomic = processor.agc_enabled.clone();
        let agc_target_atomic = processor.agc_target.clone();
        let bypass_enabled_atomic = processor.bypass_enabled.clone();
        let jitter_atomic = processor.jitter_ewma_us.clone();
        let gate_threshold_atomic = processor.gate_threshold.clone();
        let suppression_atomic = processor.suppression_strength.clone();
        let dynamic_threshold_atomic = processor.dynamic_threshold_enabled.clone();

        let is_running = Arc::new(AtomicBool::new(true));
        let run_flag = is_running.clone();

        let has_reference = echo_cancel_enabled && reference_stream.is_some();

        thread::Builder::new().name("voidmic-audio".into()).spawn(move || {
            let mut input_frame = [0.0f32; FRAME_SIZE];
            let mut output_frame = [0.0f32; FRAME_SIZE];
            let mut ref_frame = [0.0f32; FRAME_SIZE];

            // Jitter State - EWMA for smoother, more responsive display
            let mut last_loop_time = std::time::Instant::now();
            let mut jitter_ewma: f32 = 0.0;
            let mut frames_since_jitter_report = 0u32;

            loop {
                if !run_flag.load(Ordering::Relaxed) {
                    break;
                }

                // Process updates
                processor.process_updates();

                if cons_in.occupied_len() >= FRAME_SIZE {
                    // Jitter Calculation - skip obviously invalid deltas (e.g. system suspend)
                    let now = std::time::Instant::now();
                    let loop_delta = now.duration_since(last_loop_time).as_micros() as u32;
                    last_loop_time = now;

                    if loop_delta < 100_000 {
                        let expected = 10_000u32;
                        let jitter = loop_delta.abs_diff(expected) as f32;

                        // EWMA: alpha=0.1 gives ~10-frame smoothing
                        jitter_ewma = jitter_ewma * 0.9 + jitter * 0.1;
                    }

                    // Report to GUI every 50 frames (~500ms)
                    frames_since_jitter_report += 1;
                    if frames_since_jitter_report >= 50 {
                        processor
                            .jitter_ewma_us
                            .store(jitter_ewma as u32, Ordering::Relaxed);
                        frames_since_jitter_report = 0;
                    }

                    // Read Audio
                    cons_in.pop_slice(&mut input_frame);

                    // Read reference audio for echo cancellation
                    let ref_frames = if has_reference && cons_ref.occupied_len() >= FRAME_SIZE {
                        cons_ref.pop_slice(&mut ref_frame);
                        Some(&[&ref_frame[..]][..])
                    } else {
                        None
                    };

                    // Process Audio (read live values from atomics)
                    processor.process_frame(
                        &[&input_frame],
                        &mut [&mut output_frame],
                        ref_frames,
                        f32::from_bits(processor.suppression_strength.load(Ordering::Relaxed)),
                        f32::from_bits(processor.gate_threshold.load(Ordering::Relaxed)),
                        processor.dynamic_threshold_enabled.load(Ordering::Relaxed),
                    );

                    // Write Audio - retry briefly if output buffer is full
                    let mut retries = 0;
                    while prod_out.vacant_len() < FRAME_SIZE {
                        thread::yield_now();
                        retries += 1;
                        if retries > 100 {
                            break;
                        }
                    }
                    if prod_out.vacant_len() >= FRAME_SIZE {
                        prod_out.push_slice(&output_frame);
                    }
                } else {
                    thread::sleep(Duration::from_micros(200));
                }
            }
        }).context("Failed to spawn audio processing thread")?;

        input_stream.play()?;
        output_stream.play()?;
        if let Some(ref ref_stream) = reference_stream {
            ref_stream.play()?;
        }

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            _reference_stream: reference_stream,
            is_running,
            volume_level,
            calibration_mode,
            calibration_result,
            vad_sensitivity: vad_sensitivity_atomic,
            eq_low_gain: eq_low_atomic,
            eq_mid_gain: eq_mid_atomic,
            eq_high_gain: eq_high_atomic,
            eq_enabled: eq_enabled_atomic,
            agc_enabled: agc_enabled_atomic,
            _agc_target: agc_target_atomic,
            bypass_enabled: bypass_enabled_atomic,
            gate_threshold: gate_threshold_atomic,
            suppression_strength: suppression_atomic,
            dynamic_threshold_enabled: dynamic_threshold_atomic,
            _spectrum_sender: spectrum_sender,
            jitter_ewma_us: jitter_atomic,
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
    pub suppression_strength: Arc<AtomicU32>,
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
        let suppression_atomic = Arc::new(AtomicU32::new(suppression_strength.to_bits()));
        let suppression_for_thread = suppression_atomic.clone();

        thread::Builder::new().name("voidmic-output-filter".into()).spawn(move || {
            let mut denoise = DenoiseState::new();
            let mut input_frame = [0.0f32; FRAME_SIZE];
            let mut output_frame = [0.0f32; FRAME_SIZE];

            while run_flag.load(Ordering::Relaxed) {
                if cons_in.occupied_len() >= FRAME_SIZE {
                    cons_in.pop_slice(&mut input_frame);

                    // Denoise with RNNoise
                    denoise.process_frame(&mut output_frame, &input_frame);

                    // Blend based on suppression strength (live-updated from GUI)
                    let strength = f32::from_bits(suppression_for_thread.load(Ordering::Relaxed));
                    for i in 0..FRAME_SIZE {
                        output_frame[i] = input_frame[i] * (1.0 - strength)
                            + output_frame[i] * strength;
                    }

                    let mut retries = 0;
                    while prod_out.vacant_len() < FRAME_SIZE {
                        thread::yield_now();
                        retries += 1;
                        if retries > 100 {
                            break;
                        }
                    }
                    if prod_out.vacant_len() >= FRAME_SIZE {
                        prod_out.push_slice(&output_frame);
                    }
                } else {
                    thread::sleep(Duration::from_micros(500));
                }
            }
        }).context("Failed to spawn output filter thread")?;

        input_stream.play()?;
        output_stream.play()?;

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            is_running,
            suppression_strength: suppression_atomic,
        })
    }
}

impl Drop for OutputFilterEngine {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::Relaxed);
    }
}
