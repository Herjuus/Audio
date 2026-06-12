/// Core DSP module.
///
/// This module is platform-independent and CPAL-free. It operates only on
/// buffers of f32 samples. All processors must be safe to call from a
/// real-time audio callback: no allocation, no blocking, no I/O.

use crate::state::Settings;

// ---------------------------------------------------------------------------
// Processor trait
// ---------------------------------------------------------------------------

/// Common lifecycle for all DSP processors.
pub trait Processor {
    /// Called before streaming begins. Processors should pre-compute
    /// coefficients and resize any preallocated buffers here.
    fn prepare(&mut self, sample_rate: f32, max_block_size: usize);

    /// Process a block of mono samples in-place.
    fn process(&mut self, buffer: &mut [f32]);

    /// Reset all internal state (history, envelopes, filters) to zero.
    fn reset(&mut self);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const MINUS_INF_DB: f32 = -120.0;

/// Convert a linear amplitude to dB. Returns `MINUS_INF_DB` for zero/negative input.
pub fn linear_to_db(linear: f32) -> f32 {
    if linear <= 0.0 {
        MINUS_INF_DB
    } else {
        20.0 * linear.log10()
    }
}

/// Convert dB to a linear amplitude multiplier.
pub fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Compute a 1-pole IIR smoothing coefficient.
///
/// The result is used as: `y = coeff * y_prev + (1 - coeff) * target`
///
/// After `time_ms` milliseconds the output reaches ~63% of the target.
fn time_to_coeff(time_ms: f32, sample_rate: f32) -> f32 {
    if time_ms <= 0.0 {
        return 0.0;
    }
    (-1.0 / (time_ms * 0.001 * sample_rate)).exp()
}

// ---------------------------------------------------------------------------
// SmoothedValue
// ---------------------------------------------------------------------------

/// 1-pole IIR parameter smoother.
///
/// Set a target with `set()`; call `next()` once per sample to get the
/// smoothed value. Call `snap()` to jump instantly (e.g. on init or reset).
pub struct SmoothedValue {
    current: f32,
    target: f32,
    coeff: f32,
}

impl SmoothedValue {
    pub fn new(value: f32) -> Self {
        Self { current: value, target: value, coeff: 0.0 }
    }

    pub fn prepare(&mut self, smooth_ms: f32, sample_rate: f32) {
        self.coeff = time_to_coeff(smooth_ms, sample_rate);
    }

    pub fn set(&mut self, value: f32) {
        self.target = value;
    }

    pub fn snap(&mut self, value: f32) {
        self.target = value;
        self.current = value;
    }

    pub fn next(&mut self) -> f32 {
        self.current = self.coeff * self.current + (1.0 - self.coeff) * self.target;
        self.current
    }
}

// ---------------------------------------------------------------------------
// Gain
// ---------------------------------------------------------------------------

/// Gain/trim stage with parameter smoothing.
///
/// When `set_db()` is called mid-stream the gain ramps to the new value
/// over `smooth_ms` milliseconds rather than jumping instantly, which
/// prevents audible clicks.
pub struct Gain {
    /// Target gain in linear scale (set via `set_db`).
    gain_linear: f32,
    /// Current smoothed gain — this is what actually multiplies samples.
    current_linear: f32,
    /// Time in ms for the gain to reach ~63% of a new target (1-pole IIR).
    pub smooth_ms: f32,
    smooth_coeff: f32,
}

impl Gain {
    pub fn new(db: f32) -> Self {
        let lin = db_to_linear(db);
        Self {
            gain_linear: lin,
            current_linear: lin,
            smooth_ms: 10.0,
            smooth_coeff: 0.0,
        }
    }

    pub fn set_db(&mut self, db: f32) {
        self.gain_linear = db_to_linear(db);
    }

    pub fn get_db(&self) -> f32 {
        linear_to_db(self.gain_linear)
    }
}

impl Processor for Gain {
    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.smooth_coeff = time_to_coeff(self.smooth_ms, sample_rate);
        // Snap to the current target so startup is silent rather than ramping
        // in from whatever the previous value was.
        self.current_linear = self.gain_linear;
    }

    fn process(&mut self, buffer: &mut [f32]) {
        for sample in buffer.iter_mut() {
            self.current_linear = self.smooth_coeff * self.current_linear
                + (1.0 - self.smooth_coeff) * self.gain_linear;
            *sample *= self.current_linear;
        }
    }

    fn reset(&mut self) {
        self.current_linear = self.gain_linear;
    }
}

// ---------------------------------------------------------------------------
// PeakMeter
// ---------------------------------------------------------------------------

/// Tracks the absolute peak level seen since the last call to `reset()`.
pub struct PeakMeter {
    peak: f32,
}

impl PeakMeter {
    pub fn new() -> Self {
        Self { peak: 0.0 }
    }

    pub fn peak_db(&self) -> f32 {
        linear_to_db(self.peak)
    }

    pub fn peak_linear(&self) -> f32 {
        self.peak
    }
}

impl Default for PeakMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl Processor for PeakMeter {
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}

    fn process(&mut self, buffer: &mut [f32]) {
        for &sample in buffer.iter() {
            let abs = sample.abs();
            if abs > self.peak {
                self.peak = abs;
            }
        }
    }

    fn reset(&mut self) {
        self.peak = 0.0;
    }
}

// ---------------------------------------------------------------------------
// HighPass
// ---------------------------------------------------------------------------

/// 2nd-order Butterworth high-pass filter implemented as a biquad.
///
/// Removes low-frequency rumble and handling noise below `cutoff_hz`.
/// Coefficients are taken from the Audio EQ Cookbook (RBJ).
/// Call `prepare()` before use to compute coefficients for the sample rate.
pub struct HighPass {
    pub cutoff_hz: f32,
    // Normalised biquad coefficients (divided by a0 in prepare)
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    // Filter state
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl HighPass {
    pub fn new(cutoff_hz: f32) -> Self {
        Self {
            cutoff_hz,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn compute_coeffs(&mut self, sample_rate: f32) {
        let w0 = 2.0 * std::f32::consts::PI * self.cutoff_hz / sample_rate;
        // Q = 1/sqrt(2) gives a Butterworth (maximally flat) response.
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let alpha = w0.sin() / (2.0 * q);

        let b0 = (1.0 + w0.cos()) / 2.0;
        let b1 = -(1.0 + w0.cos());
        let b2 = (1.0 + w0.cos()) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * w0.cos();
        let a2 = 1.0 - alpha;

        // Normalise by a0 so the difference equation needs no division per sample.
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }
}

impl Processor for HighPass {
    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.compute_coeffs(sample_rate);
    }

    fn process(&mut self, buffer: &mut [f32]) {
        for sample in buffer.iter_mut() {
            let x = *sample;
            let y = self.b0 * x
                + self.b1 * self.x1
                + self.b2 * self.x2
                - self.a1 * self.y1
                - self.a2 * self.y2;
            self.x2 = self.x1;
            self.x1 = x;
            self.y2 = self.y1;
            self.y1 = y;
            *sample = y;
        }
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Eq
// ---------------------------------------------------------------------------

/// The filter topology for one EQ band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BandKind {
    LowShelf,
    Peak,
    HighShelf,
}

/// One biquad EQ band.
///
/// Change the public fields then call `compute_coeffs(sample_rate)` to make
/// them take effect (or call `Eq::prepare` which does this for all bands).
#[derive(Debug, Clone)]
pub struct EqBand {
    pub kind: BandKind,
    /// Centre / corner frequency in Hz.
    pub freq_hz: f32,
    /// Gain in dB. Positive = boost, negative = cut.
    pub gain_db: f32,
    /// Q factor. Only used by `Peak` bands; shelves use a fixed Butterworth slope.
    pub q: f32,
    // RBJ biquad coefficients, normalised by a0
    b0: f32, b1: f32, b2: f32,
    a1: f32, a2: f32,
    // Delay-line state
    x1: f32, x2: f32,
    y1: f32, y2: f32,
}

impl EqBand {
    pub fn new(kind: BandKind, freq_hz: f32, gain_db: f32, q: f32) -> Self {
        Self {
            kind, freq_hz, gain_db, q,
            // Identity coefficients so process() before prepare() is silent-safe.
            b0: 1.0, b1: 0.0, b2: 0.0,
            a1: 0.0, a2: 0.0,
            x1: 0.0, x2: 0.0,
            y1: 0.0, y2: 0.0,
        }
    }

    /// Recompute RBJ biquad coefficients for `sample_rate`.
    /// Must be called after changing any public parameter.
    pub fn compute_coeffs(&mut self, sample_rate: f32) {
        let w0 = 2.0 * std::f32::consts::PI * self.freq_hz / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        // A = sqrt(10^(dB/20)) = 10^(dB/40). The /40 form is what the RBJ
        // Cookbook specifies — using /20 would apply twice the intended gain.
        let a = 10.0_f32.powf(self.gain_db / 40.0);

        let (b0, b1, b2, a0, a1, a2) = match self.kind {
            BandKind::LowShelf => {
                // Slope S = 1 (Butterworth) → alpha = sin(w0)/2 * sqrt(2)
                let alpha = sin_w0 / 2.0 * 2.0_f32.sqrt();
                let b0 =  a * ((a + 1.0) - (a - 1.0) * cos_w0 + 2.0 * a.sqrt() * alpha);
                let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
                let b2 =  a * ((a + 1.0) - (a - 1.0) * cos_w0 - 2.0 * a.sqrt() * alpha);
                let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + 2.0 * a.sqrt() * alpha;
                let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - 2.0 * a.sqrt() * alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BandKind::Peak => {
                let alpha = sin_w0 / (2.0 * self.q);
                let b0 = 1.0 + alpha * a;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0 - alpha * a;
                let a0 = 1.0 + alpha / a;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha / a;
                (b0, b1, b2, a0, a1, a2)
            }
            BandKind::HighShelf => {
                let alpha = sin_w0 / 2.0 * 2.0_f32.sqrt();
                let b0 =  a * ((a + 1.0) + (a - 1.0) * cos_w0 + 2.0 * a.sqrt() * alpha);
                let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
                let b2 =  a * ((a + 1.0) + (a - 1.0) * cos_w0 - 2.0 * a.sqrt() * alpha);
                let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + 2.0 * a.sqrt() * alpha;
                let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - 2.0 * a.sqrt() * alpha;
                (b0, b1, b2, a0, a1, a2)
            }
        };

        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    fn process_band(&mut self, buffer: &mut [f32]) {
        for sample in buffer.iter_mut() {
            let x = *sample;
            let y = self.b0 * x
                + self.b1 * self.x1
                + self.b2 * self.x2
                - self.a1 * self.y1
                - self.a2 * self.y2;
            self.x2 = self.x1; self.x1 = x;
            self.y2 = self.y1; self.y1 = y;
            *sample = y;
        }
    }

    fn reset_band(&mut self) {
        self.x1 = 0.0; self.x2 = 0.0;
        self.y1 = 0.0; self.y2 = 0.0;
    }
}

/// 4-band parametric EQ.
///
/// Fixed band layout:
///   `bands[0]` — Low shelf  (default 120 Hz)
///   `bands[1]` — Peak       (default 300 Hz,  Q 0.7)
///   `bands[2]` — Peak       (default 3000 Hz, Q 0.7)
///   `bands[3]` — High shelf (default 8000 Hz)
///
/// All bands default to 0 dB (transparent). Call `prepare()` before use.
pub struct Eq {
    pub bands: [EqBand; 4],
}

impl Eq {
    pub fn new() -> Self {
        Self {
            bands: [
                EqBand::new(BandKind::LowShelf,   120.0, 0.0, 0.707),
                EqBand::new(BandKind::Peak,        300.0, 0.0, 0.7),
                EqBand::new(BandKind::Peak,       3000.0, 0.0, 0.7),
                EqBand::new(BandKind::HighShelf,  8000.0, 0.0, 0.707),
            ],
        }
    }
}

impl Default for Eq {
    fn default() -> Self { Self::new() }
}

impl Processor for Eq {
    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        for band in self.bands.iter_mut() {
            band.compute_coeffs(sample_rate);
        }
    }

    fn process(&mut self, buffer: &mut [f32]) {
        for band in self.bands.iter_mut() {
            band.process_band(buffer);
        }
    }

    fn reset(&mut self) {
        for band in self.bands.iter_mut() {
            band.reset_band();
        }
    }
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/// Noise gate. Attenuates the signal when it falls below `threshold_db`.
///
/// Uses a two-stage design:
/// - A fast peak envelope follower measures the signal level.
/// - A separate gain smoother opens/closes the gate with configurable
///   attack, hold, and release times to avoid clicks and chattering.
pub struct Gate {
    pub threshold_db: f32,
    /// How fast the gate opens once the signal exceeds the threshold (ms).
    pub attack_ms: f32,
    /// How fast the gate closes after the hold time expires (ms).
    pub release_ms: f32,
    /// How long the gate stays open after the signal drops below threshold (ms).
    pub hold_ms: f32,

    // Computed in prepare()
    threshold_linear: f32,
    /// Fast envelope follower: tracks rising peaks quickly.
    env_attack_coeff: f32,
    /// Fast envelope follower: falls slightly slower than attack.
    env_release_coeff: f32,
    gain_attack_coeff: f32,
    gain_release_coeff: f32,
    hold_samples: usize,

    // State
    envelope: f32,
    gain: f32,
    hold_counter: usize,
}

impl Gate {
    pub fn new() -> Self {
        Self {
            threshold_db: -40.0,
            attack_ms: 5.0,
            release_ms: 150.0,
            hold_ms: 100.0,
            threshold_linear: 0.0,
            env_attack_coeff: 0.0,
            env_release_coeff: 0.0,
            gain_attack_coeff: 0.0,
            gain_release_coeff: 0.0,
            hold_samples: 0,
            envelope: 0.0,
            gain: 0.0,
            hold_counter: 0,
        }
    }
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Processor for Gate {
    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.threshold_linear = db_to_linear(self.threshold_db);
        // The envelope follower uses fixed fast times so it tracks peaks
        // tightly without adding its own smearing on top of the gate times.
        self.env_attack_coeff = time_to_coeff(0.5, sample_rate);
        self.env_release_coeff = time_to_coeff(20.0, sample_rate);
        self.gain_attack_coeff = time_to_coeff(self.attack_ms, sample_rate);
        self.gain_release_coeff = time_to_coeff(self.release_ms, sample_rate);
        self.hold_samples = (self.hold_ms * 0.001 * sample_rate) as usize;
    }

    fn process(&mut self, buffer: &mut [f32]) {
        for sample in buffer.iter_mut() {
            let abs = sample.abs();

            // Update peak envelope
            let env_coeff = if abs > self.envelope {
                self.env_attack_coeff
            } else {
                self.env_release_coeff
            };
            self.envelope = env_coeff * self.envelope + (1.0 - env_coeff) * abs;

            // Determine target gain
            let target = if self.envelope >= self.threshold_linear {
                self.hold_counter = self.hold_samples;
                1.0f32
            } else if self.hold_counter > 0 {
                self.hold_counter -= 1;
                1.0f32
            } else {
                0.0f32
            };

            // Smooth gain to avoid clicks
            let gain_coeff = if target > self.gain {
                self.gain_attack_coeff
            } else {
                self.gain_release_coeff
            };
            self.gain = gain_coeff * self.gain + (1.0 - gain_coeff) * target;

            *sample *= self.gain;
        }
    }

    fn reset(&mut self) {
        self.envelope = 0.0;
        self.gain = 0.0;
        self.hold_counter = 0;
    }
}

// ---------------------------------------------------------------------------
// Compressor
// ---------------------------------------------------------------------------

/// Feed-forward compressor with peak level detection.
///
/// Reduces gain when the input level exceeds `threshold_db`. The amount of
/// reduction is controlled by `ratio`: at 4:1, a signal 4 dB above the
/// threshold comes out only 1 dB above it.
pub struct Compressor {
    pub threshold_db: f32,
    /// Compression ratio (e.g. 4.0 for 4:1). Values below 1.0 expand.
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    /// Output makeup gain in dB, applied after compression.
    pub makeup_db: f32,

    // Computed in prepare()
    attack_coeff: f32,
    release_coeff: f32,
    makeup: SmoothedValue,

    // State
    envelope: f32,
}

impl Compressor {
    pub fn new() -> Self {
        Self {
            threshold_db: -18.0,
            ratio: 4.0,
            attack_ms: 10.0,
            release_ms: 150.0,
            makeup_db: 0.0,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            makeup: SmoothedValue::new(1.0),
            envelope: 0.0,
        }
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Processor for Compressor {
    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.attack_coeff = time_to_coeff(self.attack_ms, sample_rate);
        self.release_coeff = time_to_coeff(self.release_ms, sample_rate);
        self.makeup.prepare(10.0, sample_rate);
        self.makeup.snap(db_to_linear(self.makeup_db));
    }

    fn process(&mut self, buffer: &mut [f32]) {
        for sample in buffer.iter_mut() {
            let abs = sample.abs();

            let coeff = if abs > self.envelope { self.attack_coeff } else { self.release_coeff };
            self.envelope = coeff * self.envelope + (1.0 - coeff) * abs;

            let env_db = linear_to_db(self.envelope);
            let gain_reduction_db = if env_db > self.threshold_db {
                (env_db - self.threshold_db) * (1.0 - 1.0 / self.ratio)
            } else {
                0.0
            };

            *sample *= db_to_linear(-gain_reduction_db) * self.makeup.next();
        }
    }

    fn reset(&mut self) {
        self.envelope = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Limiter
// ---------------------------------------------------------------------------

/// Brick-wall peak limiter.
///
/// Prevents the output from exceeding `ceiling_db`. Uses instantaneous
/// attack (gain is reduced immediately when a peak is detected) and a
/// smoothed release so the gain recovers gradually rather than snapping back.
pub struct Limiter {
    pub ceiling_db: f32,
    pub release_ms: f32,

    // Computed in prepare()
    ceiling: SmoothedValue,
    release_coeff: f32,

    // State
    gain: f32,
}

impl Limiter {
    pub fn new() -> Self {
        Self {
            ceiling_db: -1.0,
            release_ms: 50.0,
            ceiling: SmoothedValue::new(db_to_linear(-1.0)),
            release_coeff: 0.0,
            gain: 1.0,
        }
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Processor for Limiter {
    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.ceiling.prepare(10.0, sample_rate);
        self.ceiling.snap(db_to_linear(self.ceiling_db));
        self.release_coeff = time_to_coeff(self.release_ms, sample_rate);
    }

    fn process(&mut self, buffer: &mut [f32]) {
        for sample in buffer.iter_mut() {
            let abs = sample.abs();
            let ceiling = self.ceiling.next();

            let target_gain = if abs > ceiling { ceiling / abs } else { 1.0 };

            if target_gain < self.gain {
                self.gain = target_gain;
            } else {
                self.gain = self.release_coeff * self.gain
                    + (1.0 - self.release_coeff) * target_gain;
            }

            *sample *= self.gain;
        }
    }

    fn reset(&mut self) {
        self.gain = 1.0;
    }
}

// ---------------------------------------------------------------------------
// VoiceChain
// ---------------------------------------------------------------------------

/// Complete voice processing chain.
///
/// Signal order:
///   gain → high_pass → eq → gate → compressor → limiter → peak_meter
pub struct VoiceChain {
    pub gain: Gain,
    pub high_pass: HighPass,
    pub eq: Eq,
    pub gate: Gate,
    pub compressor: Compressor,
    pub limiter: Limiter,
    pub peak_meter: PeakMeter,
    // Per-processor bypass flags. When true the processor is skipped entirely.
    pub gain_enabled: bool,
    pub high_pass_enabled: bool,
    pub eq_enabled: bool,
    pub gate_enabled: bool,
    pub compressor_enabled: bool,
    pub limiter_enabled: bool,
    sample_rate: f32,
    max_block_size: usize,
}

impl VoiceChain {
    pub fn new() -> Self {
        Self {
            gain: Gain::new(0.0),
            high_pass: HighPass::new(80.0),
            eq: Eq::new(),
            gate: Gate::new(),
            compressor: Compressor::new(),
            limiter: Limiter::new(),
            peak_meter: PeakMeter::new(),
            gain_enabled: true,
            high_pass_enabled: true,
            eq_enabled: true,
            gate_enabled: true,
            compressor_enabled: true,
            limiter_enabled: true,
            sample_rate: 48_000.0,
            max_block_size: 4096,
        }
    }

    pub fn prepare(&mut self, sample_rate: f32, max_block_size: usize) {
        self.sample_rate = sample_rate;
        self.max_block_size = max_block_size;
        self.gain.prepare(sample_rate, max_block_size);
        self.high_pass.prepare(sample_rate, max_block_size);
        self.eq.prepare(sample_rate, max_block_size);
        self.gate.prepare(sample_rate, max_block_size);
        self.compressor.prepare(sample_rate, max_block_size);
        self.limiter.prepare(sample_rate, max_block_size);
        self.peak_meter.prepare(sample_rate, max_block_size);
    }

    pub fn process(&mut self, buffer: &mut [f32]) {
        if self.gain_enabled       { self.gain.process(buffer); }
        if self.high_pass_enabled  { self.high_pass.process(buffer); }
        if self.eq_enabled         { self.eq.process(buffer); }
        if self.gate_enabled       { self.gate.process(buffer); }
        if self.compressor_enabled { self.compressor.process(buffer); }
        if self.limiter_enabled    { self.limiter.process(buffer); }
        self.peak_meter.process(buffer);
    }

    pub fn reset(&mut self) {
        self.gain.reset();
        self.high_pass.reset();
        self.eq.reset();
        self.gate.reset();
        self.compressor.reset();
        self.limiter.reset();
        self.peak_meter.reset();
    }

    /// Apply a loaded `Settings` to the chain. Safe to call while streaming:
    /// gain changes are smoothed, and all coefficient recomputation happens
    /// via `prepare()` which does not touch running state (envelopes, etc.).
    pub fn apply_settings(&mut self, s: &Settings) {
        self.gain_enabled       = s.gain_enabled;
        self.high_pass_enabled  = s.high_pass_enabled;
        self.gate_enabled       = s.gate_enabled;
        self.eq_enabled         = s.eq_enabled;
        self.compressor_enabled = s.compressor_enabled;
        self.limiter_enabled    = s.limiter_enabled;

        self.gain.set_db(s.gain_db);

        self.high_pass.cutoff_hz = s.high_pass_cutoff_hz;
        self.high_pass.prepare(self.sample_rate, self.max_block_size);

        self.gate.threshold_db = s.gate_threshold_db;
        self.gate.attack_ms = s.gate_attack_ms;
        self.gate.release_ms = s.gate_release_ms;
        self.gate.hold_ms = s.gate_hold_ms;
        self.gate.prepare(self.sample_rate, self.max_block_size);

        self.compressor.threshold_db = s.compressor_threshold_db;
        self.compressor.ratio = s.compressor_ratio;
        self.compressor.attack_ms = s.compressor_attack_ms;
        self.compressor.release_ms = s.compressor_release_ms;
        self.compressor.makeup_db = s.compressor_makeup_db;
        self.compressor.makeup.set(db_to_linear(s.compressor_makeup_db));
        self.compressor.prepare(self.sample_rate, self.max_block_size);

        self.limiter.ceiling_db = s.limiter_ceiling_db;
        self.limiter.release_ms = s.limiter_release_ms;
        self.limiter.ceiling.set(db_to_linear(s.limiter_ceiling_db));
        self.limiter.prepare(self.sample_rate, self.max_block_size);

        for (i, band) in self.eq.bands.iter_mut().enumerate() {
            band.freq_hz = s.eq_band_freq_hz[i];
            band.gain_db = s.eq_band_gain_db[i];
            band.q       = s.eq_band_q[i];
            band.compute_coeffs(self.sample_rate);
        }
    }
}

impl Default for VoiceChain {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f32 = 48_000.0;
    const BLOCK: usize = 512;

    fn sine_block(freq_hz: f32, amplitude: f32, sample_rate: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| {
                amplitude
                    * (2.0 * std::f32::consts::PI * freq_hz * i as f32 / sample_rate).sin()
            })
            .collect()
    }

    fn rms(buf: &[f32]) -> f32 {
        let sum_sq: f32 = buf.iter().map(|&s| s * s).sum();
        (sum_sq / buf.len() as f32).sqrt()
    }

    // --- db_to_linear / linear_to_db ---

    #[test]
    fn db_linear_roundtrip() {
        for db in [-60.0, -20.0, -6.0, 0.0, 6.0, 20.0] {
            let roundtrip = linear_to_db(db_to_linear(db));
            assert!(
                (roundtrip - db).abs() < 0.001,
                "roundtrip failed for {db} dB: got {roundtrip}"
            );
        }
    }

    #[test]
    fn zero_db_is_unity() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn minus_6db_is_roughly_half() {
        let half = db_to_linear(-6.0206);
        assert!((half - 0.5).abs() < 0.0001);
    }

    #[test]
    fn linear_to_db_zero_returns_minus_inf() {
        assert_eq!(linear_to_db(0.0), MINUS_INF_DB);
    }

    // --- Gain ---

    #[test]
    fn gain_zero_db_is_transparent() {
        let mut g = Gain::new(0.0);
        let mut buf = vec![0.5_f32; 8];
        g.process(&mut buf);
        for s in &buf {
            assert!((*s - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn gain_minus_6db_halves_amplitude() {
        let mut g = Gain::new(-6.0206);
        let mut buf = vec![1.0_f32; 8];
        g.process(&mut buf);
        for s in &buf {
            assert!((*s - 0.5).abs() < 0.0001, "expected ~0.5, got {s}");
        }
    }

    #[test]
    fn gain_silence_stays_silent() {
        let mut g = Gain::new(20.0);
        let mut buf = vec![0.0_f32; 64];
        g.process(&mut buf);
        for s in &buf {
            assert_eq!(*s, 0.0);
        }
    }

    #[test]
    fn gain_set_db_updates_level() {
        let mut g = Gain::new(0.0);
        g.set_db(-20.0);
        let mut buf = vec![1.0_f32; 1];
        g.process(&mut buf);
        let expected = db_to_linear(-20.0);
        assert!((buf[0] - expected).abs() < 1e-6);
    }

    // --- PeakMeter ---

    #[test]
    fn peak_meter_silence_reads_minus_inf() {
        let mut m = PeakMeter::new();
        let mut buf = vec![0.0_f32; BLOCK];
        m.process(&mut buf);
        assert_eq!(m.peak_db(), MINUS_INF_DB);
    }

    #[test]
    fn peak_meter_detects_full_scale() {
        let mut m = PeakMeter::new();
        let mut buf = vec![0.5_f32; BLOCK];
        buf[100] = 1.0;
        m.process(&mut buf);
        assert!((m.peak_linear() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn peak_meter_resets() {
        let mut m = PeakMeter::new();
        let mut buf = vec![1.0_f32; BLOCK];
        m.process(&mut buf);
        m.reset();
        assert_eq!(m.peak_linear(), 0.0);
    }

    #[test]
    fn peak_meter_tracks_negative_samples() {
        let mut m = PeakMeter::new();
        let mut buf = vec![-0.75_f32; BLOCK];
        m.process(&mut buf);
        assert!((m.peak_linear() - 0.75).abs() < 1e-6);
    }

    // --- HighPass ---

    #[test]
    fn high_pass_blocks_dc() {
        let mut hp = HighPass::new(80.0);
        hp.prepare(SAMPLE_RATE, BLOCK);
        // DC signal: constant value of 1.0
        let mut buf = vec![1.0f32; BLOCK * 4];
        hp.process(&mut buf);
        // After the filter settles the output should be near zero
        let tail = &buf[BLOCK * 3..];
        for &s in tail {
            assert!(s.abs() < 0.01, "DC not blocked: {s}");
        }
    }

    #[test]
    fn high_pass_passes_high_frequencies() {
        let mut hp = HighPass::new(80.0);
        hp.prepare(SAMPLE_RATE, BLOCK);
        // 1 kHz is well above the 80 Hz cutoff — should pass with ~0 dB loss
        let mut buf = sine_block(1000.0, 1.0, SAMPLE_RATE, BLOCK * 4);
        let rms_before = rms(&buf);
        hp.process(&mut buf);
        let rms_after = rms(&buf[BLOCK * 2..]);
        let loss_db = linear_to_db(rms_after / rms_before);
        assert!(loss_db > -1.0, "1 kHz attenuated by {loss_db:.1} dB — too much");
    }

    #[test]
    fn high_pass_attenuates_below_cutoff() {
        let mut hp = HighPass::new(80.0);
        hp.prepare(SAMPLE_RATE, BLOCK);
        // 20 Hz is well below the 80 Hz cutoff — should be significantly attenuated
        let mut buf = sine_block(20.0, 1.0, SAMPLE_RATE, BLOCK * 8);
        let rms_before = rms(&buf);
        hp.process(&mut buf);
        let rms_after = rms(&buf[BLOCK * 4..]);
        let loss_db = linear_to_db(rms_after / rms_before);
        assert!(loss_db < -10.0, "20 Hz only attenuated by {loss_db:.1} dB — not enough");
    }

    // --- Gate ---

    #[test]
    fn gate_passes_loud_signal() {
        let mut gate = Gate::new();
        gate.threshold_db = -40.0;
        gate.prepare(SAMPLE_RATE, BLOCK);
        // Signal at -6 dB: well above the -40 dB threshold
        let mut buf = sine_block(440.0, 0.5, SAMPLE_RATE, BLOCK * 10);
        gate.process(&mut buf);
        // Tail of the buffer should have significant energy
        let tail_rms = rms(&buf[BLOCK * 8..]);
        assert!(tail_rms > 0.1, "gate closed on loud signal: rms = {tail_rms}");
    }

    #[test]
    fn gate_attenuates_silence() {
        let mut gate = Gate::new();
        gate.threshold_db = -20.0;
        gate.hold_ms = 0.0;
        gate.release_ms = 1.0; // very fast release so we don't need a huge buffer
        gate.prepare(SAMPLE_RATE, BLOCK);
        // Silence: should fall below the -20 dB threshold
        let mut buf = vec![0.0f32; BLOCK * 20];
        gate.process(&mut buf);
        let tail_rms = rms(&buf[BLOCK * 15..]);
        assert!(tail_rms < 0.01, "gate did not close on silence: rms = {tail_rms}");
    }

    // --- Compressor ---

    #[test]
    fn compressor_reduces_loud_signal() {
        let mut comp = Compressor::new();
        comp.threshold_db = -20.0;
        comp.ratio = 4.0;
        comp.attack_ms = 1.0;
        comp.prepare(SAMPLE_RATE, BLOCK);
        // Signal at 0 dB: 20 dB above threshold — compressor should reduce it
        let input = sine_block(440.0, 1.0, SAMPLE_RATE, BLOCK * 10);
        let input_rms = rms(&input[BLOCK * 5..]);
        let mut buf = input;
        comp.process(&mut buf);
        let output_rms = rms(&buf[BLOCK * 5..]);
        assert!(
            output_rms < input_rms * 0.9,
            "compressor did not reduce level: in={input_rms:.3} out={output_rms:.3}"
        );
    }

    #[test]
    fn compressor_leaves_quiet_signal_unchanged() {
        let mut comp = Compressor::new();
        comp.threshold_db = -20.0;
        comp.ratio = 4.0;
        comp.prepare(SAMPLE_RATE, BLOCK);
        // Signal at -40 dB: well below threshold — should pass unchanged
        let amplitude = db_to_linear(-40.0);
        let input = sine_block(440.0, amplitude, SAMPLE_RATE, BLOCK);
        let mut buf = input.clone();
        comp.process(&mut buf);
        for (a, b) in input.iter().zip(buf.iter()) {
            assert!((a - b).abs() < 1e-4, "quiet signal modified: {a} -> {b}");
        }
    }

    // --- Limiter ---

    #[test]
    fn limiter_clips_at_ceiling() {
        let mut lim = Limiter::new();
        lim.ceiling_db = -1.0;
        lim.prepare(SAMPLE_RATE, BLOCK);
        let ceiling = db_to_linear(-1.0);
        // Over-threshold signal
        let mut buf = vec![2.0f32; BLOCK];
        lim.process(&mut buf);
        for &s in &buf {
            assert!(
                s <= ceiling + 1e-5,
                "limiter exceeded ceiling: {s} > {ceiling}"
            );
        }
    }

    #[test]
    fn limiter_passes_quiet_signal() {
        let mut lim = Limiter::new();
        lim.ceiling_db = -1.0;
        lim.prepare(SAMPLE_RATE, BLOCK);
        // Signal well below ceiling
        let amplitude = db_to_linear(-12.0);
        let input = sine_block(440.0, amplitude, SAMPLE_RATE, BLOCK);
        let mut buf = input.clone();
        lim.process(&mut buf);
        for (a, b) in input.iter().zip(buf.iter()) {
            assert!((a - b).abs() < 1e-4, "quiet signal modified by limiter");
        }
    }

    // --- Eq ---

    #[test]
    fn eq_all_bands_zero_db_is_transparent() {
        let mut eq = Eq::new(); // all bands default to 0 dB
        eq.prepare(SAMPLE_RATE, BLOCK);
        let input = sine_block(1000.0, 0.5, SAMPLE_RATE, BLOCK);
        let mut buf = input.clone();
        eq.process(&mut buf);
        for (a, b) in input.iter().zip(buf.iter()) {
            assert!((a - b).abs() < 1e-4, "0 dB EQ modified signal: {a} -> {b}");
        }
    }

    #[test]
    fn eq_peak_boost_raises_level_at_target_frequency() {
        let mut eq = Eq::new();
        eq.bands[2].freq_hz = 1000.0; // peak band at 1 kHz
        eq.bands[2].gain_db = 12.0;
        eq.bands[2].q       = 2.0;   // fairly narrow so other bands are unaffected
        eq.prepare(SAMPLE_RATE, BLOCK);
        // 1 kHz tone: should be boosted
        let input = sine_block(1000.0, 0.2, SAMPLE_RATE, BLOCK * 4);
        let rms_before = rms(&input[BLOCK..]);
        let mut buf = input;
        eq.process(&mut buf);
        let rms_after = rms(&buf[BLOCK..]);
        assert!(
            rms_after > rms_before * 1.5,
            "+12 dB boost at 1 kHz had no effect: before={rms_before:.4} after={rms_after:.4}"
        );
    }

    #[test]
    fn eq_peak_cut_reduces_level_at_target_frequency() {
        let mut eq = Eq::new();
        eq.bands[2].freq_hz = 1000.0;
        eq.bands[2].gain_db = -12.0;
        eq.bands[2].q       = 2.0;
        eq.prepare(SAMPLE_RATE, BLOCK);
        let input = sine_block(1000.0, 0.5, SAMPLE_RATE, BLOCK * 4);
        let rms_before = rms(&input[BLOCK..]);
        let mut buf = input;
        eq.process(&mut buf);
        let rms_after = rms(&buf[BLOCK..]);
        assert!(
            rms_after < rms_before * 0.6,
            "-12 dB cut at 1 kHz had no effect: before={rms_before:.4} after={rms_after:.4}"
        );
    }

    #[test]
    fn eq_low_shelf_boost_does_not_affect_high_frequency() {
        let mut eq = Eq::new();
        eq.bands[0].freq_hz = 120.0;
        eq.bands[0].gain_db = 12.0;
        eq.prepare(SAMPLE_RATE, BLOCK);
        // 8 kHz is far above the shelf — should be essentially unchanged
        let input = sine_block(8000.0, 0.5, SAMPLE_RATE, BLOCK * 4);
        let rms_before = rms(&input[BLOCK..]);
        let mut buf = input;
        eq.process(&mut buf);
        let rms_after = rms(&buf[BLOCK..]);
        let change_db = linear_to_db(rms_after / rms_before);
        assert!(
            change_db.abs() < 1.5,
            "low shelf leaked into 8 kHz: {change_db:.1} dB change"
        );
    }

    // --- VoiceChain ---

    #[test]
    fn voice_chain_applies_gain_and_meters() {
        let mut chain = VoiceChain::new();
        chain.gain.set_db(-6.0206);
        // Push compressor threshold above the signal so it does not compress.
        chain.compressor.threshold_db = 0.0;
        chain.prepare(SAMPLE_RATE, BLOCK);

        // Process enough samples for the gate to fully open.
        let mut buf = sine_block(440.0, 1.0, SAMPLE_RATE, BLOCK * 10);
        chain.process(&mut buf);

        // After the gate is open and no compression is applied, the peak
        // should be close to 0.5 (unity sine reduced by -6 dB).
        assert!(
            chain.peak_meter.peak_linear() > 0.45 && chain.peak_meter.peak_linear() < 0.55,
            "peak was {}",
            chain.peak_meter.peak_linear()
        );
    }

    #[test]
    fn voice_chain_limiter_prevents_clipping() {
        let mut chain = VoiceChain::new();
        chain.gain.set_db(20.0); // boost hard to force clipping
        chain.prepare(SAMPLE_RATE, BLOCK);
        let ceiling = db_to_linear(chain.limiter.ceiling_db);
        let mut buf = sine_block(440.0, 1.0, SAMPLE_RATE, BLOCK);
        chain.process(&mut buf);
        for &s in &buf {
            assert!(s.abs() <= ceiling + 1e-5, "chain exceeded limiter ceiling: {s}");
        }
    }
}
