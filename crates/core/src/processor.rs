use crate::constants::{FRAME_SIZE, SAMPLE_RATE};
use crate::echo_cancel::EchoCanceller;
use anyhow::{anyhow, Result};
use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type};
use nnnoiseless::DenoiseState;
use spectrum_analyzer::{samples_fft_to_spectrum, FrequencyLimit};
use spectrum_analyzer::scaling::divide_by_N_sqrt;
use spectrum_analyzer::windows::hann_window;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use webrtc_vad::{Vad, VadMode};
use crossbeam_channel::Sender;

// Gate timing constants (all in milliseconds)
const ATTACK_MS: u32 = 5;
const RELEASE_MS: u32 = 200;
const FADE_MS: u32 = 10;

/// Tracks minimum RMS over a sliding window to estimate noise floor.
pub struct NoiseFloorTracker {
    window: VecDeque<f32>,
    window_size: usize,
    current_floor: f32,
}

impl NoiseFloorTracker {
    pub fn new(window_seconds: f32) -> Self {
        let window_size = (window_seconds * 100.0) as usize;
        Self {
            window: VecDeque::with_capacity(window_size),
            window_size,
            current_floor: 0.01,
        }
    }

    pub fn update(&mut self, rms: f32) {
        self.window.push_back(rms);
        if self.window.len() > self.window_size {
            self.window.pop_front();
        }
        if self.window.len() >= 10 {
            let mut sorted: Vec<f32> = self.window.iter().cloned().collect();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let idx = sorted.len() / 10;
            self.current_floor = sorted[idx];
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
        let low_coeffs = Coefficients::<f32>::from_params(
            Type::LowShelf(low_gain_db),
            fs,
            200.0.hz(),
            0.707,
        ).map_err(|e| anyhow!("Failed to create low shelf filter: {:?}", e))?;

        // Peaking: 1000 Hz
        let mid_coeffs = Coefficients::<f32>::from_params(
            Type::PeakingEQ(mid_gain_db),
            fs,
            1000.0.hz(),
            1.0,
        ).map_err(|e| anyhow!("Failed to create peaking filter: {:?}", e))?;

        // High Shelf: 4000 Hz
        let high_coeffs = Coefficients::<f32>::from_params(
            Type::HighShelf(high_gain_db),
            fs,
            4000.0.hz(),
            0.707,
        ).map_err(|e| anyhow!("Failed to create high shelf filter: {:?}", e))?;

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
        
        let low_coeffs = Coefficients::<f32>::from_params(
            Type::LowShelf(low_gain_db),
            fs,
            200.0.hz(),
            0.707
        ).map_err(|e| anyhow!("Failed to update low shelf: {:?}", e))?;
        self.low_shelf.update_coefficients(low_coeffs);

        let mid_coeffs = Coefficients::<f32>::from_params(
            Type::PeakingEQ(mid_gain_db),
            fs,
            1000.0.hz(),
            1.0
        ).map_err(|e| anyhow!("Failed to update peaking: {:?}", e))?;
        self.peaking.update_coefficients(mid_coeffs);

        let high_coeffs = Coefficients::<f32>::from_params(
            Type::HighShelf(high_gain_db),
            fs,
            4000.0.hz(),
            0.707
        ).map_err(|e| anyhow!("Failed to update high shelf: {:?}", e))?;
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

    pub fn process_frame(&mut self, frame: &mut [f32]) {
        let sum: f32 = frame.iter().map(|x| x * x).sum();
        let rms = (sum / frame.len() as f32).sqrt();

        if rms > 0.0001 {
            let error = self.target_level / rms;
            let target_gain = if error < 1.0 {
                error
            } else {
                error.min(3.0)
            };

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

        for sample in frame.iter_mut() {
            let val = *sample * self.current_gain;
            *sample = val.clamp(-0.99, 0.99);
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
    denoise: Box<DenoiseState<'static>>,
    echo_canceller: Option<EchoCanceller>,
    eq: Option<ThreeBandEq>,
    agc_limiter: LookaheadLimiter,
    noise_floor_tracker: NoiseFloorTracker,
    vad: Vad,
    
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
}

// Safety: VoidProcessor owns all its raw pointers (Vad, EchoCanceller) and is moved to a single thread.
// It is not shared between threads (except for the Atomics, which are thread-safe).
unsafe impl Send for VoidProcessor {}

impl VoidProcessor {
    pub fn new(
        vad_sensitivity: i32,
        eq_params: (f32, f32, f32),
        agc_target_level: f32,
        echo_cancel_enabled: bool,
    ) -> Self {
        let mut vad = Vad::new_with_rate_and_mode(
            webrtc_vad::SampleRate::Rate48kHz,
            match vad_sensitivity {
                0 => VadMode::Quality,
                1 => VadMode::LowBitrate,
                2 => VadMode::Aggressive,
                _ => VadMode::VeryAggressive,
            }
        );

        Self {
            denoise: DenoiseState::new(),
            echo_canceller: if echo_cancel_enabled { Some(EchoCanceller::new()) } else { None },
            eq: ThreeBandEq::new(eq_params.0, eq_params.1, eq_params.2).ok(),
            agc_limiter: LookaheadLimiter::new(agc_target_level),
            noise_floor_tracker: NoiseFloorTracker::new(3.0),
            vad,
            
            gate_open: false,
            samples_since_close: 0,
            samples_since_open: 0,
            fade_position: 0,
            bypass_state: BypassState::Active,
            crossfade_pos: 0,
            calibration_samples: Vec::new(),

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
            self.vad = Vad::new_with_rate_and_mode(
                webrtc_vad::SampleRate::Rate48kHz,
                vad_mode
            );
        }

        if self.eq.is_some() {
             let new_low = f32::from_bits(self.eq_low_gain.load(Ordering::Relaxed));
             let new_mid = f32::from_bits(self.eq_mid_gain.load(Ordering::Relaxed));
             let new_high = f32::from_bits(self.eq_high_gain.load(Ordering::Relaxed));
             
             if (new_low - self.current_eq_low).abs() > 0.01 || 
                (new_mid - self.current_eq_mid).abs() > 0.01 || 
                (new_high - self.current_eq_high).abs() > 0.01 {
                    self.current_eq_low = new_low;
                    self.current_eq_mid = new_mid;
                    self.current_eq_high = new_high;
                    if let Some(eq_instance) = &mut self.eq {
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
            },
            BypassState::Bypassed if !bypass_requested => {
                self.bypass_state = BypassState::FadingIn;
                self.crossfade_pos = 0;
            },
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
        input_frame: &[f32], 
        output_frame: &mut [f32], 
        ref_frame: Option<&[f32]>,
        suppression_strength: f32,
        gate_threshold: f32,
        dynamic_threshold_enabled: bool,
    ) {
        let mut temp_input = [0.0f32; FRAME_SIZE];
        temp_input.copy_from_slice(input_frame);

        // Handle Bypass Logic High Level
        let crossfade_len = 480; // 10ms

        match self.bypass_state {
            BypassState::Bypassed => {
                 output_frame.copy_from_slice(input_frame);
            },
            _ => {
                // Run detailed processing
                
                // 1. Echo Cancellation
                if let (Some(aec), Some(reference)) = (&mut self.echo_canceller, ref_frame) {
                    if reference.len() == FRAME_SIZE {
                         let processed = aec.process_frame(&temp_input, reference);
                         temp_input.copy_from_slice(&processed);
                    }
                }

                // 2. Denoise (RNNoise)
                self.denoise.process_frame(output_frame, &temp_input);

                // 1b. Blend raw and denoised based on suppression strength
                for i in 0..FRAME_SIZE {
                    output_frame[i] = temp_input[i] * (1.0 - suppression_strength)
                        + output_frame[i] * suppression_strength;
                }

                // 2. Smart Gate Logic
                let sum: f32 = output_frame.iter().map(|x| x * x).sum();
                let rms = (sum / FRAME_SIZE as f32).sqrt();
                self.volume_level.store(rms.to_bits(), Ordering::Relaxed);

                // Calibration mode
                if self.calibration_mode.load(Ordering::Relaxed) {
                    self.calibration_samples.push(rms);
                    let calibration_duration_samples = SAMPLE_RATE * 3;
                    if self.calibration_samples.len() >= (calibration_duration_samples / FRAME_SIZE as u32) as usize {
                        let max_rms = self.calibration_samples.iter().cloned().fold(0.0f32, f32::max);
                        let suggested = (max_rms * 1.2).max(0.005);
                        self.calibration_result.store(suggested.to_bits(), Ordering::Relaxed);
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
                    vad_buffer[i] = (temp_input[i] * 32767.0).clamp(-32768.0, 32767.0) as i16;
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

                // 3. Apply Gate
                if !self.gate_open {
                    for sample in output_frame.iter_mut() {
                        if self.fade_position < fade_samples {
                            let fade_gain = 1.0 - (self.fade_position as f32 / fade_samples as f32);
                            *sample *= fade_gain;
                            self.fade_position += 1;
                        } else {
                            *sample = 0.0;
                        }
                    }
                } else {
                    self.fade_position = 0;
                }

                // 4. Equalizer
                if let Some(eq) = &mut self.eq {
                    for sample in output_frame.iter_mut() {
                        *sample = eq.process(*sample);
                    }
                }
                
                // 5. AGC
                if self.agc_enabled.load(Ordering::Relaxed) {
                    self.agc_limiter.process_frame(output_frame);
                }
            }
        }
        
        // Apply Crossfade transitions
        match self.bypass_state {
            BypassState::FadingOut => {
                for i in 0..FRAME_SIZE {
                    let t = self.crossfade_pos as f32 / crossfade_len as f32;
                    let gain_wet = (t * std::f32::consts::PI / 2.0).cos();
                    let gain_dry = (t * std::f32::consts::PI / 2.0).sin();
                    output_frame[i] = output_frame[i] * gain_wet + input_frame[i] * gain_dry;
                    
                    if self.crossfade_pos < crossfade_len { self.crossfade_pos += 1; }
                }
                if self.crossfade_pos >= crossfade_len { self.bypass_state = BypassState::Bypassed; }
            },
             BypassState::FadingIn => {
                for i in 0..FRAME_SIZE {
                    let t = self.crossfade_pos as f32 / crossfade_len as f32;
                    let gain_dry = (t * std::f32::consts::PI / 2.0).cos();
                    let gain_wet = (t * std::f32::consts::PI / 2.0).sin();
                    output_frame[i] = output_frame[i] * gain_wet + input_frame[i] * gain_dry;
                    
                    if self.crossfade_pos < crossfade_len { self.crossfade_pos += 1; }
                }
                if self.crossfade_pos >= crossfade_len { self.bypass_state = BypassState::Active; }
            },
            _ => {}
        }

        // Spectrum Analysis
        if let Some(sender) = &self.spectrum_sender {
            let window_in = hann_window(input_frame);
            let input_spectrum = samples_fft_to_spectrum(
                &window_in,
                SAMPLE_RATE,
                FrequencyLimit::Range(20.0, 20_000.0),
                Some(&divide_by_N_sqrt),
            ).ok();

            let window_out = hann_window(output_frame);
            let output_spectrum = samples_fft_to_spectrum(
                &window_out,
                SAMPLE_RATE,
                FrequencyLimit::Range(20.0, 20_000.0),
                Some(&divide_by_N_sqrt),
            ).ok();

            if let (Some(in_spec), Some(out_spec)) = (input_spectrum, output_spectrum) {
                 let in_mags: Vec<f32> = in_spec.data().iter().map(|(_, val)| val.val()).collect();
                 let out_mags: Vec<f32> = out_spec.data().iter().map(|(_, val)| val.val()).collect();
                 let _ = sender.try_send((in_mags, out_mags));
            }
        }
    }
}
