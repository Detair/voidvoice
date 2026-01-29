use crate::constants::{FRAME_SIZE, SAMPLE_RATE};
use crate::echo_cancel::EchoCanceller;
use anyhow::{anyhow, Result};
use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type};
use crossbeam_channel::Sender;
use nnnoiseless::DenoiseState;
use spectrum_analyzer::scaling::divide_by_N_sqrt;
use spectrum_analyzer::windows::hann_window;
use spectrum_analyzer::{samples_fft_to_spectrum, FrequencyLimit};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use webrtc_vad::{Vad, VadMode};

// Gate timing constants (all in milliseconds)
const ATTACK_MS: u32 = 5;
const RELEASE_MS: u32 = 200;
const FADE_MS: u32 = 10;

/// Tracks minimum RMS over a sliding window to estimate noise floor.
/// Uses a fixed-size ring buffer to avoid allocations.
pub struct NoiseFloorTracker {
    window: [f32; 300], // Fixed 3s @ 100 frames/sec
    write_idx: usize,
    count: usize,
    current_floor: f32,
}

impl NoiseFloorTracker {
    pub fn new(_window_seconds: f32) -> Self {
        Self {
            window: [0.0; 300],
            write_idx: 0,
            count: 0,
            current_floor: 0.01,
        }
    }

    pub fn update(&mut self, rms: f32) {
        self.window[self.write_idx] = rms;
        self.write_idx = (self.write_idx + 1) % 300;
        if self.count < 300 {
            self.count += 1;
        }

        // Find 10th percentile without allocation
        // Simple approach: track running minimum with decay
        if self.count >= 10 {
            // Find minimum in recent samples (last 30 = ~300ms)
            let start = if self.count >= 30 {
                self.write_idx.wrapping_sub(30) % 300
            } else {
                0
            };
            let mut min_val = f32::MAX;
            for i in 0..30.min(self.count) {
                let idx = (start + i) % 300;
                if self.window[idx] < min_val && self.window[idx] > 0.0001 {
                    min_val = self.window[idx];
                }
            }
            if min_val < f32::MAX {
                // Smooth transition
                self.current_floor = self.current_floor * 0.95 + min_val * 0.05;
            }
        }
    }

    pub fn floor(&self) -> f32 {
        self.current_floor
    }
}

/// Three-band Equalizer using Biquad filters
pub struct ThreeBandEq {
    low_shelf: DirectForm2Transposed<f32>,
    peaking: DirectForm2Transposed<f32>,
    high_shelf: DirectForm2Transposed<f32>,
}

impl ThreeBandEq {
    pub fn new(low_gain_db: f32, mid_gain_db: f32, high_gain_db: f32) -> Result<Self> {
        let fs = SAMPLE_RATE.hz();

        // Low Shelf: 200 Hz
        let low_coeffs =
            Coefficients::<f32>::from_params(Type::LowShelf(low_gain_db), fs, 200.0.hz(), 0.707)
                .map_err(|e| anyhow!("Failed to create low shelf filter: {:?}", e))?;

        // Peaking: 1000 Hz
        let mid_coeffs =
            Coefficients::<f32>::from_params(Type::PeakingEQ(mid_gain_db), fs, 1000.0.hz(), 1.0)
                .map_err(|e| anyhow!("Failed to create peaking filter: {:?}", e))?;

        // High Shelf: 4000 Hz
        let high_coeffs =
            Coefficients::<f32>::from_params(Type::HighShelf(high_gain_db), fs, 4000.0.hz(), 0.707)
                .map_err(|e| anyhow!("Failed to create high shelf filter: {:?}", e))?;

        Ok(Self {
            low_shelf: DirectForm2Transposed::<f32>::new(low_coeffs),
            peaking: DirectForm2Transposed::<f32>::new(mid_coeffs),
            high_shelf: DirectForm2Transposed::<f32>::new(high_coeffs),
        })
    }

    pub fn process(&mut self, sample: f32) -> f32 {
        let l = self.low_shelf.run(sample);
        let m = self.peaking.run(l);
        self.high_shelf.run(m)
    }

    pub fn update_gains(
        &mut self,
        low_gain_db: f32,
        mid_gain_db: f32,
        high_gain_db: f32,
    ) -> Result<()> {
        let fs = SAMPLE_RATE.hz();

        let low_coeffs =
            Coefficients::<f32>::from_params(Type::LowShelf(low_gain_db), fs, 200.0.hz(), 0.707)
                .map_err(|e| anyhow!("Failed to update low shelf: {:?}", e))?;
        self.low_shelf.update_coefficients(low_coeffs);

        let mid_coeffs =
            Coefficients::<f32>::from_params(Type::PeakingEQ(mid_gain_db), fs, 1000.0.hz(), 1.0)
                .map_err(|e| anyhow!("Failed to update peaking: {:?}", e))?;
        self.peaking.update_coefficients(mid_coeffs);

        let high_coeffs =
            Coefficients::<f32>::from_params(Type::HighShelf(high_gain_db), fs, 4000.0.hz(), 0.707)
                .map_err(|e| anyhow!("Failed to update high shelf: {:?}", e))?;
        self.high_shelf.update_coefficients(high_coeffs);
        Ok(())
    }
}

/// Simple lookahead limiter for Automatic Gain Control (AGC)
pub struct LookaheadLimiter {
    pub target_level: f32,
    current_gain: f32,
    attack_coeff: f32,
    release_coeff: f32,
}

impl LookaheadLimiter {
    pub fn new(target_level: f32) -> Self {
        Self {
            target_level,
            current_gain: 1.0,
            attack_coeff: 0.1,
            release_coeff: 0.005,
        }
    }

    pub fn process_frame(&mut self, frames: &mut [&mut [f32]]) {
        if frames.is_empty() {
            return;
        }

        // Calculate max RMS across all channels for linked limiting
        let frame_len = frames[0].len();
        let mut sum_sq = 0.0;
        // Average energy across all channels first? Or max energy?
        // Standard "Link" usually takes the max level of any channel.

        for k in 0..frame_len {
            let mut sample_max = 0.0f32;
            for channel in frames.iter() {
                sample_max = sample_max.max(channel[k].abs());
            }
            sum_sq += sample_max * sample_max;
        }
        let max_rms = (sum_sq / frame_len as f32).sqrt();

        if max_rms > 0.0001 {
            let error = self.target_level / max_rms;
            let target_gain = if error < 1.0 { error } else { error.min(3.0) };

            if target_gain < self.current_gain {
                self.current_gain += (target_gain - self.current_gain) * self.attack_coeff;
            } else {
                self.current_gain += (target_gain - self.current_gain) * self.release_coeff;
            }
        } else {
            if self.current_gain > 1.0 {
                self.current_gain -= 0.001;
            }
        }

        // Apply gain to all channels
        for channel in frames.iter_mut() {
            for sample in channel.iter_mut() {
                let val = *sample * self.current_gain;
                *sample = val.clamp(-0.99, 0.99);
            }
        }
    }
}

pub enum BypassState {
    Active,
    Bypassed,
    FadingOut,
    FadingIn,
}

pub struct VoidProcessor {
    denoise: Vec<Box<DenoiseState<'static>>>,
    echo_canceller: Vec<EchoCanceller>,
    eq: Vec<ThreeBandEq>,
    agc_limiter: LookaheadLimiter,
    noise_floor_tracker: NoiseFloorTracker,
    vad: Vad,
    channels: usize,

    // State
    gate_open: bool,
    samples_since_close: u32,
    samples_since_open: u32,
    fade_position: u32,
    bypass_state: BypassState,
    crossfade_pos: u32,
    calibration_samples: Vec<f32>,

    // Current Settings (Locally cached to avoid atomic load every sample)
    current_vad_mode: i32,
    current_eq_low: f32,
    current_eq_mid: f32,
    current_eq_high: f32,

    // Shared Atomics (Control Interface)
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

    // Pre-allocated spectrum buffers (avoid allocations in audio thread)
    spectrum_in_buf: Vec<f32>,
    spectrum_out_buf: Vec<f32>,
}

// Safety: VoidProcessor owns all its raw pointers (Vad, EchoCanceller) and is moved to a single thread.
// It is not shared between threads (except for the Atomics, which are thread-safe).
unsafe impl Send for VoidProcessor {}
unsafe impl Sync for VoidProcessor {}

impl VoidProcessor {
    pub fn new(
        channels: usize,
        vad_sensitivity: i32,
        eq_params: (f32, f32, f32),
        agc_target_level: f32,
        echo_cancel_enabled: bool,
    ) -> Self {
        let vad = Vad::new_with_rate_and_mode(
            webrtc_vad::SampleRate::Rate48kHz,
            match vad_sensitivity {
                0 => VadMode::Quality,
                1 => VadMode::LowBitrate,
                2 => VadMode::Aggressive,
                _ => VadMode::VeryAggressive,
            },
        );

        let mut denoise = Vec::with_capacity(channels);
        let mut echo_canceller = Vec::with_capacity(channels);
        let mut eq = Vec::with_capacity(channels);

        for _ in 0..channels {
            denoise.push(DenoiseState::new());
            if echo_cancel_enabled {
                echo_canceller.push(EchoCanceller::new());
            }
            if let Ok(e) = ThreeBandEq::new(eq_params.0, eq_params.1, eq_params.2) {
                eq.push(e);
            }
        }

        Self {
            denoise,
            echo_canceller,
            eq,
            agc_limiter: LookaheadLimiter::new(agc_target_level),
            noise_floor_tracker: NoiseFloorTracker::new(3.0),
            vad,
            channels,

            gate_open: false,
            samples_since_close: 0,
            samples_since_open: 0,
            fade_position: 0,
            bypass_state: BypassState::Active,
            crossfade_pos: 0,
            calibration_samples: Vec::with_capacity(300), // Pre-alloc for ~3s calibration

            current_vad_mode: vad_sensitivity,
            current_eq_low: eq_params.0,
            current_eq_mid: eq_params.1,
            current_eq_high: eq_params.2,

            volume_level: Arc::new(AtomicU32::new(0)),
            calibration_mode: Arc::new(AtomicBool::new(false)),
            calibration_result: Arc::new(AtomicU32::new(0)),
            vad_sensitivity: Arc::new(AtomicU32::new(vad_sensitivity as u32)),
            eq_low_gain: Arc::new(AtomicU32::new(eq_params.0.to_bits())),
            eq_mid_gain: Arc::new(AtomicU32::new(eq_params.1.to_bits())),
            eq_high_gain: Arc::new(AtomicU32::new(eq_params.2.to_bits())),
            agc_enabled: Arc::new(AtomicBool::new(false)),
            agc_target: Arc::new(AtomicU32::new(agc_target_level.to_bits())),
            bypass_enabled: Arc::new(AtomicBool::new(false)),
            jitter_max_us: Arc::new(AtomicU32::new(0)),
            spectrum_sender: None,
            // Pre-allocate spectrum buffers (FRAME_SIZE/2 bins typical for FFT)
            spectrum_in_buf: Vec::with_capacity(FRAME_SIZE / 2),
            spectrum_out_buf: Vec::with_capacity(FRAME_SIZE / 2),
        }
    }

    pub fn process_updates(&mut self) {
        // Check for settings updates
        let new_vad = self.vad_sensitivity.load(Ordering::Relaxed) as i32;
        if new_vad != self.current_vad_mode {
            self.current_vad_mode = new_vad;
            let vad_mode = match self.current_vad_mode {
                0 => VadMode::Quality,
                1 => VadMode::LowBitrate,
                2 => VadMode::Aggressive,
                _ => VadMode::VeryAggressive,
            };
            self.vad = Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate48kHz, vad_mode);
        }

        if !self.eq.is_empty() {
            let new_low = f32::from_bits(self.eq_low_gain.load(Ordering::Relaxed));
            let new_mid = f32::from_bits(self.eq_mid_gain.load(Ordering::Relaxed));
            let new_high = f32::from_bits(self.eq_high_gain.load(Ordering::Relaxed));

            if (new_low - self.current_eq_low).abs() > 0.01
                || (new_mid - self.current_eq_mid).abs() > 0.01
                || (new_high - self.current_eq_high).abs() > 0.01
            {
                self.current_eq_low = new_low;
                self.current_eq_mid = new_mid;
                self.current_eq_high = new_high;
                for eq_instance in &mut self.eq {
                    let _ = eq_instance.update_gains(new_low, new_mid, new_high);
                }
            }
        }

        // Check Bypass Toggle
        let bypass_requested = self.bypass_enabled.load(Ordering::Relaxed);
        match self.bypass_state {
            BypassState::Active if bypass_requested => {
                self.bypass_state = BypassState::FadingOut;
                self.crossfade_pos = 0;
            }
            BypassState::Bypassed if !bypass_requested => {
                self.bypass_state = BypassState::FadingIn;
                self.crossfade_pos = 0;
            }
            _ => {}
        }

        // Check AGC settings
        let target_bits = self.agc_target.load(Ordering::Relaxed);
        let new_target = f32::from_bits(target_bits);
        if (new_target - self.agc_limiter.target_level).abs() > 0.01 {
            self.agc_limiter.target_level = new_target;
        }
    }

    pub fn process_frame(
        &mut self,
        input_frames: &[&[f32]],
        output_frames: &mut [&mut [f32]],
        ref_frames: Option<&[&[f32]]>,
        suppression_strength: f32,
        gate_threshold: f32,
        dynamic_threshold_enabled: bool,
    ) {
        let channels = self.channels;
        assert_eq!(input_frames.len(), channels);
        assert_eq!(output_frames.len(), channels);

        let mut mono_mix = [0.0f32; FRAME_SIZE];

        // 1. Process Per-Channel Logic (Echo Cancel, Denoise)
        for i in 0..channels {
            let input_ch = input_frames[i];
            let output_ch = &mut output_frames[i];

            // Convert input to temp buffer for processing
            let mut temp_input = [0.0f32; FRAME_SIZE];
            temp_input.copy_from_slice(input_ch);

            // A. Echo Cancellation
            if let Some(aec_instance) = self.echo_canceller.get_mut(i) {
                if let Some(refs) = ref_frames {
                    // Try to match channel, or use channel 0 if fewer refs
                    if let Some(ref_ch) = refs.get(i).or(refs.get(0)) {
                        let processed = aec_instance.process_frame(&temp_input, ref_ch);
                        temp_input.copy_from_slice(&processed);
                    }
                }
            }

            // B. Denoise (RNNoise)
            if let Some(denoise_instance) = self.denoise.get_mut(i) {
                denoise_instance.process_frame(output_ch, &temp_input);
            }

            // C. Blend (Suppression Strength)
            for j in 0..FRAME_SIZE {
                output_ch[j] = temp_input[j] * (1.0 - suppression_strength)
                    + output_ch[j] * suppression_strength;

                // Accumulate to Mono Mix for Gate/VAD analysis
                mono_mix[j] += output_ch[j];
            }
        }

        // 2. Normalize Mono Mix
        let norm_factor = 1.0 / (channels as f32);
        for sample in mono_mix.iter_mut() {
            *sample *= norm_factor;
        }

        // 3. Linked Gate Analysis (Runs on Mono Mix)
        // Handle Bypass Logic High Level
        let crossfade_len = 480; // 10ms

        match self.bypass_state {
            BypassState::Bypassed => {
                for i in 0..channels {
                    output_frames[i].copy_from_slice(input_frames[i]);
                }
            }
            _ => {
                // Analysis
                let sum: f32 = mono_mix.iter().map(|x| x * x).sum();
                let rms = (sum / FRAME_SIZE as f32).sqrt();
                self.volume_level.store(rms.to_bits(), Ordering::Relaxed);

                // Calibration mode
                if self.calibration_mode.load(Ordering::Relaxed) {
                    self.calibration_samples.push(rms);
                    let calibration_duration_samples = SAMPLE_RATE * 3;
                    if self.calibration_samples.len()
                        >= (calibration_duration_samples / FRAME_SIZE as u32) as usize
                    {
                        let max_rms = self
                            .calibration_samples
                            .iter()
                            .cloned()
                            .fold(0.0f32, f32::max);
                        let suggested = (max_rms * 1.2).max(0.005);
                        self.calibration_result
                            .store(suggested.to_bits(), Ordering::Relaxed);
                        self.calibration_mode.store(false, Ordering::Relaxed);
                        self.calibration_samples.clear();
                    }
                }

                // Gate decision
                let effective_threshold = if dynamic_threshold_enabled {
                    self.noise_floor_tracker.update(rms);
                    let dynamic = self.noise_floor_tracker.floor() * 1.5 + 0.003;
                    dynamic.clamp(0.005, 0.08)
                } else {
                    gate_threshold
                };

                let mut vad_buffer = [0i16; FRAME_SIZE];
                for i in 0..FRAME_SIZE {
                    vad_buffer[i] = (mono_mix[i] * 32767.0).clamp(-32768.0, 32767.0) as i16;
                }
                let is_speech = self.vad.is_voice_segment(&vad_buffer).unwrap_or(false);

                let attack_samples = (SAMPLE_RATE / 1000) * ATTACK_MS;
                let release_samples = (SAMPLE_RATE / 1000) * RELEASE_MS;
                let fade_samples = (SAMPLE_RATE / 1000) * FADE_MS;

                if rms > effective_threshold || is_speech {
                    self.samples_since_close += FRAME_SIZE as u32;
                    if self.samples_since_close >= attack_samples {
                        self.gate_open = true;
                        self.samples_since_open = 0;
                        self.fade_position = 0;
                    }
                } else {
                    self.samples_since_close = 0;
                    if self.gate_open {
                        self.samples_since_open += FRAME_SIZE as u32;
                        if self.samples_since_open > release_samples {
                            self.gate_open = false;
                        }
                    }
                }

                // 4. Apply Gate & EQ & AGC to ALL channels
                for i in 0..channels {
                    let output_ch = &mut output_frames[i];

                    // Gate
                    if !self.gate_open {
                        let mut local_fade = self.fade_position;
                        for sample in output_ch.iter_mut() {
                            if local_fade < fade_samples {
                                let fade_gain = 1.0 - (local_fade as f32 / fade_samples as f32);
                                *sample *= fade_gain;
                                local_fade += 1;
                            } else {
                                *sample = 0.0;
                            }
                        }
                    }

                    // Equalizer
                    if let Some(eq) = self.eq.get_mut(i) {
                        for sample in output_ch.iter_mut() {
                            *sample = eq.process(*sample);
                        }
                    }
                }

                // Update global fade position once
                if !self.gate_open {
                    if self.fade_position < fade_samples {
                        self.fade_position += FRAME_SIZE as u32; // This is rough, per-sample fade below is better
                    }
                } else {
                    self.fade_position = 0;
                }

                // AGC (Linked)
                if self.agc_enabled.load(Ordering::Relaxed) {
                    self.agc_limiter.process_frame(output_frames);
                }
            }
        }

        // Apply Crossfade transitions
        let mut t_start = self.crossfade_pos;
        match self.bypass_state {
            BypassState::FadingOut => {
                for j in 0..FRAME_SIZE {
                    let t = t_start as f32 / crossfade_len as f32;
                    let gain_wet = (t * std::f32::consts::PI / 2.0).cos();
                    let gain_dry = (t * std::f32::consts::PI / 2.0).sin();

                    for i in 0..channels {
                        output_frames[i][j] =
                            output_frames[i][j] * gain_wet + input_frames[i][j] * gain_dry;
                    }
                    if t_start < crossfade_len {
                        t_start += 1;
                    }
                }
                self.crossfade_pos = t_start;
                if self.crossfade_pos >= crossfade_len {
                    self.bypass_state = BypassState::Bypassed;
                }
            }
            BypassState::FadingIn => {
                for j in 0..FRAME_SIZE {
                    let t = t_start as f32 / crossfade_len as f32;
                    let gain_dry = (t * std::f32::consts::PI / 2.0).cos();
                    let gain_wet = (t * std::f32::consts::PI / 2.0).sin();

                    for i in 0..channels {
                        output_frames[i][j] =
                            output_frames[i][j] * gain_wet + input_frames[i][j] * gain_dry;
                    }

                    if t_start < crossfade_len {
                        t_start += 1;
                    }
                }
                self.crossfade_pos = t_start;
                if self.crossfade_pos >= crossfade_len {
                    self.bypass_state = BypassState::Active;
                }
            }
            _ => {}
        }

        // Spectrum Analysis (On Mono Mix) - uses pre-allocated buffers
        if let Some(sender) = &self.spectrum_sender {
            // Need Input Mono Mix too
            let mut input_mono = [0.0f32; FRAME_SIZE];
            for j in 0..FRAME_SIZE {
                for i in 0..channels {
                    input_mono[j] += input_frames[i][j];
                }
                input_mono[j] *= norm_factor;
            }

            let window_in = hann_window(&input_mono);
            let input_spectrum = samples_fft_to_spectrum(
                &window_in,
                SAMPLE_RATE,
                FrequencyLimit::Range(20.0, 20_000.0),
                Some(&divide_by_N_sqrt),
            )
            .ok();

            let window_out = hann_window(&mono_mix);
            let output_spectrum = samples_fft_to_spectrum(
                &window_out,
                SAMPLE_RATE,
                FrequencyLimit::Range(20.0, 20_000.0),
                Some(&divide_by_N_sqrt),
            )
            .ok();

            if let (Some(in_spec), Some(out_spec)) = (input_spectrum, output_spectrum) {
                // Reuse pre-allocated buffers
                self.spectrum_in_buf.clear();
                self.spectrum_out_buf.clear();

                for (_, val) in in_spec.data().iter() {
                    self.spectrum_in_buf.push(val.val());
                }
                for (_, val) in out_spec.data().iter() {
                    self.spectrum_out_buf.push(val.val());
                }

                // Clone to send (channel requires owned data)
                let _ =
                    sender.try_send((self.spectrum_in_buf.clone(), self.spectrum_out_buf.clone()));
            }
        }
    }
}
