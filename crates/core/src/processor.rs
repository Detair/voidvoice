use crate::constants::{FRAME_SIZE, SAMPLE_RATE};
use crate::echo_cancel::EchoCanceller;
use anyhow::{anyhow, Result};
use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type};
use crossbeam_channel::Sender;
use nnnoiseless::DenoiseState;
use spectrum_analyzer::scaling::divide_by_N_sqrt;
use spectrum_analyzer::{samples_fft_to_spectrum, FrequencyLimit};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use webrtc_vad::{Vad, VadMode};

// Gate timing constants (all in milliseconds)
const ATTACK_MS: u32 = 5;
const RELEASE_MS: u32 = 200;
const FADE_MS: u32 = 10;

/// Tracks minimum RMS over a sliding window to estimate noise floor.
/// Uses a fixed-size ring buffer (3s at 100 frames/sec) to avoid allocations.
pub struct NoiseFloorTracker {
    window: [f32; 300],
    write_idx: usize,
    count: usize,
    current_floor: f32,
}

impl Default for NoiseFloorTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl NoiseFloorTracker {
    pub fn new() -> Self {
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
                (self.write_idx + 300 - 30) % 300
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
                self.current_floor = self.current_floor.mul_add(0.95, min_val * 0.05);
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
        } else if self.current_gain > 1.0 {
            self.current_gain -= 0.001;
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
    vad_instances: [Vad; 4], // Pre-created for all VadMode variants to avoid RT allocation
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
    current_eq_enabled: bool,
    current_agc_enabled: bool,
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
    pub eq_enabled: Arc<AtomicBool>,
    pub agc_enabled: Arc<AtomicBool>,
    pub agc_target: Arc<AtomicU32>,
    pub bypass_enabled: Arc<AtomicBool>,
    pub jitter_ewma_us: Arc<AtomicU32>,
    pub gate_threshold: Arc<AtomicU32>,
    pub suppression_strength: Arc<AtomicU32>,
    pub dynamic_threshold_enabled: Arc<AtomicBool>,
    pub spectrum_sender: Option<Sender<(Vec<f32>, Vec<f32>)>>,

    // Pre-allocated spectrum buffers (avoid allocations in audio thread)
    spectrum_in_buf: Vec<f32>,
    spectrum_out_buf: Vec<f32>,
    spectrum_frame_counter: u32,
    hann_coefficients: [f32; FRAME_SIZE],
    windowed_in: [f32; FRAME_SIZE],
    windowed_out: [f32; FRAME_SIZE],
}

// SAFETY: VoidProcessor owns all its mutable state (Vad, EchoCanceller, DenoiseState)
// and is moved to a single audio processing thread. These types use raw pointers internally
// (preventing auto-Send), but are safe to move across threads since they are exclusively
// owned and never aliased. The only cross-thread access is through Arc<Atomic*> fields,
// which are inherently thread-safe. VoidProcessor must NOT be shared via &reference
// across threads (it does not implement Sync), only moved (Send).
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for VoidProcessor {}

impl VoidProcessor {
    pub fn new(
        channels: usize,
        vad_sensitivity: i32,
        eq_params: (f32, f32, f32),
        agc_target_level: f32,
        echo_cancel_enabled: bool,
    ) -> Self {
        let vad_instances = [
            Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate48kHz, VadMode::Quality),
            Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate48kHz, VadMode::LowBitrate),
            Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate48kHz, VadMode::Aggressive),
            Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate48kHz, VadMode::VeryAggressive),
        ];

        let mut denoise = Vec::with_capacity(channels);
        let mut echo_canceller = Vec::with_capacity(channels);
        let mut eq = Vec::with_capacity(channels);

        // Pre-compute Hann window coefficients (periodic form matching spectrum-analyzer crate)
        let mut hann_coefficients = [0.0f32; FRAME_SIZE];
        for (i, coeff) in hann_coefficients.iter_mut().enumerate() {
            *coeff = 0.5
                * (1.0
                    - (2.0 * std::f32::consts::PI * i as f32 / FRAME_SIZE as f32).cos());
        }

        for _ in 0..channels {
            denoise.push(DenoiseState::new());
            if echo_cancel_enabled {
                if let Some(aec) = EchoCanceller::new() {
                    echo_canceller.push(aec);
                }
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
            noise_floor_tracker: NoiseFloorTracker::new(),
            vad_instances,
            channels,

            gate_open: false,
            samples_since_close: 0,
            samples_since_open: 0,
            fade_position: 0,
            bypass_state: BypassState::Active,
            crossfade_pos: 0,
            calibration_samples: Vec::with_capacity(300), // Pre-alloc for ~3s calibration

            current_vad_mode: vad_sensitivity,
            current_eq_enabled: true,
            current_agc_enabled: false,
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
            eq_enabled: Arc::new(AtomicBool::new(true)),
            agc_enabled: Arc::new(AtomicBool::new(false)),
            agc_target: Arc::new(AtomicU32::new(agc_target_level.to_bits())),
            bypass_enabled: Arc::new(AtomicBool::new(false)),
            jitter_ewma_us: Arc::new(AtomicU32::new(0)),
            gate_threshold: Arc::new(AtomicU32::new(0.015f32.to_bits())),
            suppression_strength: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            dynamic_threshold_enabled: Arc::new(AtomicBool::new(false)),
            spectrum_sender: None,
            // Pre-allocate spectrum buffers (FRAME_SIZE/2 bins typical for FFT)
            spectrum_in_buf: Vec::with_capacity(FRAME_SIZE / 2),
            spectrum_out_buf: Vec::with_capacity(FRAME_SIZE / 2),
            spectrum_frame_counter: 0,
            hann_coefficients,
            windowed_in: [0.0; FRAME_SIZE],
            windowed_out: [0.0; FRAME_SIZE],
        }
    }

    pub fn process_updates(&mut self) {
        // Check for settings updates
        let new_vad = self.vad_sensitivity.load(Ordering::Relaxed) as i32;
        if new_vad != self.current_vad_mode {
            self.current_vad_mode = new_vad.clamp(0, 3);
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

        // Cache EQ and AGC enabled state
        self.current_eq_enabled = self.eq_enabled.load(Ordering::Relaxed);
        self.current_agc_enabled = self.agc_enabled.load(Ordering::Relaxed);

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
        if input_frames.len() != channels || output_frames.len() != channels {
            // Mismatch: output silence rather than crashing the host
            log::error!(
                "Channel count mismatch: expected {}, got input={} output={}",
                channels,
                input_frames.len(),
                output_frames.len()
            );
            for out_ch in output_frames.iter_mut() {
                out_ch.fill(0.0);
            }
            return;
        }

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
                    if let Some(ref_ch) = refs.get(i).or_else(|| refs.first()) {
                        let mut aec_output = [0.0f32; FRAME_SIZE];
                        aec_instance.process_frame(&temp_input, ref_ch, &mut aec_output);
                        temp_input.copy_from_slice(&aec_output);
                    }
                }
            }

            // B. Denoise (RNNoise)
            if let Some(denoise_instance) = self.denoise.get_mut(i) {
                denoise_instance.process_frame(output_ch, &temp_input);
            }

            // C. Blend (Suppression Strength)
            for j in 0..FRAME_SIZE {
                output_ch[j] = temp_input[j].mul_add(1.0 - suppression_strength, output_ch[j] * suppression_strength);

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
                    let dynamic = self.noise_floor_tracker.floor().mul_add(1.5, 0.003);
                    dynamic.clamp(0.005, 0.08)
                } else {
                    gate_threshold
                };

                let mut vad_buffer = [0i16; FRAME_SIZE];
                for i in 0..FRAME_SIZE {
                    vad_buffer[i] = (mono_mix[i] * 32767.0).clamp(-32768.0, 32767.0) as i16;
                }
                let vad_idx = self.current_vad_mode.clamp(0, 3) as usize;
                let is_speech = self.vad_instances[vad_idx].is_voice_segment(&vad_buffer).unwrap_or(false);

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
                let mut final_fade = self.fade_position;
                for (i, output_ch) in output_frames.iter_mut().enumerate().take(channels) {

                    // Gate (each channel uses same fade envelope)
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
                        final_fade = local_fade;
                    }

                    // Equalizer
                    if self.current_eq_enabled {
                        if let Some(eq) = self.eq.get_mut(i) {
                            for sample in output_ch.iter_mut() {
                                *sample = eq.process(*sample);
                            }
                        }
                    }
                }

                // Update global fade position from per-sample tracking
                if !self.gate_open {
                    self.fade_position = final_fade;
                } else {
                    self.fade_position = 0;
                }

                // AGC (Linked)
                if self.current_agc_enabled {
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
                            output_frames[i][j].mul_add(gain_wet, input_frames[i][j] * gain_dry);
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
                            output_frames[i][j].mul_add(gain_wet, input_frames[i][j] * gain_dry);
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

        // Spectrum Analysis (On Mono Mix) - throttled to every 4th frame (~25fps)
        self.spectrum_frame_counter += 1;
        if self.spectrum_frame_counter >= 4 {
            self.spectrum_frame_counter = 0;
        }
        if self.spectrum_frame_counter == 0 {
        if let Some(sender) = &self.spectrum_sender {
            // Need Input Mono Mix too
            let mut input_mono = [0.0f32; FRAME_SIZE];
            for j in 0..FRAME_SIZE {
                for input_ch in input_frames.iter().take(channels) {
                    input_mono[j] += input_ch[j];
                }
                input_mono[j] *= norm_factor;
            }

            // Apply Hann window using pre-computed coefficients (avoids Vec allocation)
            for j in 0..FRAME_SIZE {
                self.windowed_in[j] = input_mono[j] * self.hann_coefficients[j];
                self.windowed_out[j] = mono_mix[j] * self.hann_coefficients[j];
            }

            let input_spectrum = samples_fft_to_spectrum(
                &self.windowed_in,
                SAMPLE_RATE,
                FrequencyLimit::Range(20.0, 20_000.0),
                Some(&divide_by_N_sqrt),
            )
            .ok();

            let output_spectrum = samples_fft_to_spectrum(
                &self.windowed_out,
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

                // Only clone when channel has room to avoid wasted Vec allocations
                if !sender.is_full() {
                    if let Err(crossbeam_channel::TrySendError::Disconnected(_)) =
                        sender.try_send((
                            self.spectrum_in_buf.clone(),
                            self.spectrum_out_buf.clone(),
                        ))
                    {
                        log::warn!("Spectrum receiver disconnected, disabling sender");
                        self.spectrum_sender = None;
                    }
                }
            }
        }
        } // spectrum throttle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── NoiseFloorTracker ─────────────────────────────────────────

    #[test]
    fn test_initial_floor() {
        let tracker = NoiseFloorTracker::new();
        assert!((tracker.floor() - 0.01).abs() < 0.001);
    }

    #[test]
    fn test_floor_converges_to_minimum() {
        let mut tracker = NoiseFloorTracker::new();
        // Feed a constant RMS for many frames
        for _ in 0..500 {
            tracker.update(0.05);
        }
        // Floor should converge toward 0.05
        assert!(
            tracker.floor() > 0.02,
            "Floor should converge upward: got {}",
            tracker.floor()
        );
    }

    #[test]
    fn test_floor_ignores_near_zero() {
        let mut tracker = NoiseFloorTracker::new();
        // Pre-fill with a known value
        for _ in 0..100 {
            tracker.update(0.03);
        }
        let floor_before = tracker.floor();
        // Feed near-zero values (below 0.0001 threshold)
        for _ in 0..50 {
            tracker.update(0.00001);
        }
        // Floor should not have dropped significantly from the near-zero values
        assert!(
            tracker.floor() > floor_before * 0.5,
            "Floor should not be dragged down by near-zero: got {}",
            tracker.floor()
        );
    }

    #[test]
    fn test_floor_updates_with_new_minimum() {
        let mut tracker = NoiseFloorTracker::new();
        for _ in 0..100 {
            tracker.update(0.1);
        }
        let high_floor = tracker.floor();
        // Now feed lower values
        for _ in 0..200 {
            tracker.update(0.005);
        }
        assert!(
            tracker.floor() < high_floor,
            "Floor should track downward: {} should be < {}",
            tracker.floor(),
            high_floor
        );
    }

    #[test]
    fn test_ring_buffer_wraps() {
        let mut tracker = NoiseFloorTracker::new();
        // Feed more than 300 samples (the ring buffer size)
        for i in 0..600 {
            tracker.update(0.01 + (i as f32) * 0.0001);
        }
        // Should not panic, and floor should be reasonable
        assert!(tracker.floor() > 0.0);
    }

    // ── ThreeBandEq ──────────────────────────────────────────────

    #[test]
    fn test_flat_eq_is_near_identity() {
        let mut eq = ThreeBandEq::new(0.0, 0.0, 0.0).unwrap();
        // Process a DC-ish pulse and verify output is close to input
        // (filters have transient response, so check after warmup)
        for _ in 0..100 {
            eq.process(0.5);
        }
        let out = eq.process(0.5);
        assert!(
            (out - 0.5).abs() < 0.05,
            "Flat EQ should be near-identity after warmup: got {}",
            out
        );
    }

    #[test]
    fn test_eq_construction_with_valid_gains() {
        assert!(ThreeBandEq::new(-6.0, 3.0, 6.0).is_ok());
        assert!(ThreeBandEq::new(0.0, 0.0, 0.0).is_ok());
        assert!(ThreeBandEq::new(10.0, -10.0, 10.0).is_ok());
    }

    #[test]
    fn test_eq_update_gains() {
        let mut eq = ThreeBandEq::new(0.0, 0.0, 0.0).unwrap();
        assert!(eq.update_gains(3.0, -3.0, 6.0).is_ok());
        assert!(eq.update_gains(-10.0, 0.0, 10.0).is_ok());
    }

    // ── LookaheadLimiter ─────────────────────────────────────────

    #[test]
    fn test_quiet_signal_gains_up() {
        let mut limiter = LookaheadLimiter::new(0.7);
        let mut data = vec![0.1f32; FRAME_SIZE];
        let mut frames: Vec<&mut [f32]> = vec![data.as_mut_slice()];
        // Process several frames to let gain ramp up
        for _ in 0..50 {
            limiter.process_frame(&mut frames);
        }
        // After many frames, samples should be boosted above 0.1
        assert!(
            frames[0][0].abs() > 0.1,
            "Quiet signal should be boosted: got {}",
            frames[0][0]
        );
    }

    #[test]
    fn test_loud_signal_gains_down() {
        let mut limiter = LookaheadLimiter::new(0.3);
        let mut data = vec![0.9f32; FRAME_SIZE];
        let mut frames: Vec<&mut [f32]> = vec![data.as_mut_slice()];
        for _ in 0..50 {
            limiter.process_frame(&mut frames);
        }
        // After many frames, samples should be reduced below 0.9
        assert!(
            frames[0][0].abs() < 0.9,
            "Loud signal should be attenuated: got {}",
            frames[0][0]
        );
    }

    #[test]
    fn test_output_never_clips() {
        let mut limiter = LookaheadLimiter::new(0.7);
        let mut data = vec![0.98f32; FRAME_SIZE];
        let mut frames: Vec<&mut [f32]> = vec![data.as_mut_slice()];
        for _ in 0..100 {
            limiter.process_frame(&mut frames);
        }
        for sample in frames[0].iter() {
            assert!(
                sample.abs() <= 0.99,
                "Output must not exceed ±0.99: got {}",
                sample
            );
        }
    }

    #[test]
    fn test_empty_frames_no_panic() {
        let mut limiter = LookaheadLimiter::new(0.7);
        let mut frames: Vec<&mut [f32]> = vec![];
        limiter.process_frame(&mut frames); // Should not panic
    }

    // ── VoidProcessor ────────────────────────────────────────────

    #[test]
    fn test_processor_creation() {
        let _p1 = VoidProcessor::new(1, 2, (0.0, 0.0, 0.0), 0.7, false);
        let _p2 = VoidProcessor::new(2, 0, (-3.0, 0.0, 3.0), 0.5, false);
    }

    #[test]
    fn test_silence_produces_silence() {
        let mut processor = VoidProcessor::new(1, 2, (0.0, 0.0, 0.0), 0.7, false);
        let input = [0.0f32; FRAME_SIZE];
        let mut output = [0.0f32; FRAME_SIZE];

        // Process enough frames for the gate to fully close
        for _ in 0..100 {
            processor.process_frame(
                &[&input],
                &mut [&mut output],
                None,
                1.0,
                0.015,
                false,
            );
        }

        // After many silent frames, output should be all zeros
        let max = output.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        assert!(max < 0.001, "Silent input should produce silent output: max={}", max);
    }

    #[test]
    fn test_bypass_passes_through() {
        let mut processor = VoidProcessor::new(1, 2, (0.0, 0.0, 0.0), 0.7, false);
        processor.bypass_enabled.store(true, Ordering::Relaxed);
        processor.process_updates();

        // Generate a non-zero signal
        let mut input = [0.0f32; FRAME_SIZE];
        for (i, s) in input.iter_mut().enumerate() {
            *s = (i as f32 / FRAME_SIZE as f32) * 0.5;
        }
        let mut output = [0.0f32; FRAME_SIZE];

        // Process enough frames for bypass crossfade to complete
        for _ in 0..20 {
            processor.process_frame(
                &[&input],
                &mut [&mut output],
                None,
                1.0,
                0.015,
                false,
            );
        }

        // After crossfade settles, output should match input
        for i in 0..FRAME_SIZE {
            assert!(
                (output[i] - input[i]).abs() < 0.01,
                "Bypass should pass through: sample {} expected {} got {}",
                i,
                input[i],
                output[i]
            );
        }
    }

    #[test]
    fn test_gate_closes_on_silence() {
        let mut processor = VoidProcessor::new(1, 2, (0.0, 0.0, 0.0), 0.7, false);

        // First, feed loud audio to open the gate
        let loud = [0.3f32; FRAME_SIZE];
        let mut output = [0.0f32; FRAME_SIZE];
        for _ in 0..10 {
            processor.process_frame(
                &[&loud],
                &mut [&mut output],
                None,
                1.0,
                0.015,
                false,
            );
        }

        // Now feed silence - gate should close after release period
        let silence = [0.0f32; FRAME_SIZE];
        for _ in 0..200 {
            processor.process_frame(
                &[&silence],
                &mut [&mut output],
                None,
                1.0,
                0.015,
                false,
            );
        }

        let max = output.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        assert!(max < 0.001, "Gate should close after silence: max={}", max);
    }

    #[test]
    fn test_channel_mismatch_does_not_panic() {
        let mut processor = VoidProcessor::new(2, 2, (0.0, 0.0, 0.0), 0.7, false);
        let input = [0.5f32; FRAME_SIZE];
        let mut output = [0.5f32; FRAME_SIZE];

        // Pass 1 channel to a 2-channel processor — should not panic
        processor.process_frame(
            &[&input],        // 1 channel, expected 2
            &mut [&mut output],
            None,
            1.0,
            0.015,
            false,
        );

        // Output should be zeroed (silence fallback)
        assert_eq!(output[0], 0.0, "Mismatch should produce silence");
    }

    #[test]
    fn test_process_updates_does_not_panic() {
        let mut processor = VoidProcessor::new(1, 2, (0.0, 0.0, 0.0), 0.7, false);
        // Call process_updates multiple times with no changes — should be safe
        for _ in 0..10 {
            processor.process_updates();
        }
    }
}
