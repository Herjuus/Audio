#![allow(clippy::empty_line_after_doc_comments)]
/// Offline DSP test — Milestones 2 & 5
///
/// Generates test signals, runs them through individual processors and the
/// full VoiceChain, writes results to test_output/ as 32-bit float mono WAV
/// files, and prints peak readings.
///
/// Usage:
///   cargo run --example offline_test
///
/// Open test_output/ in Audacity, Reaper, or any audio editor.

use hound::{SampleFormat, WavSpec, WavWriter};
use micapp::dsp::{
    db_to_linear, linear_to_db, Compressor, Gate, HighPass, Limiter, Processor, VoiceChain,
};

const SAMPLE_RATE: u32 = 48_000;

// ---------------------------------------------------------------------------
// WAV helpers
// ---------------------------------------------------------------------------

fn wav_spec() -> WavSpec {
    WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    }
}

fn write_wav(path: &str, samples: &[f32]) {
    let mut writer = WavWriter::create(path, wav_spec())
        .unwrap_or_else(|e| panic!("failed to create {path}: {e}"));
    for &s in samples {
        writer
            .write_sample(s)
            .unwrap_or_else(|e| panic!("write error for {path}: {e}"));
    }
    writer
        .finalize()
        .unwrap_or_else(|e| panic!("finalize error for {path}: {e}"));
}

fn report(label: &str, buf: &[f32]) {
    let peak = buf.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    println!("  {label:<50}  peak: {:>+7.1} dBFS", linear_to_db(peak));
}

// ---------------------------------------------------------------------------
// Signal generators
// ---------------------------------------------------------------------------

fn gen_silence(secs: f32) -> Vec<f32> {
    vec![0.0; samples(secs)]
}

fn gen_sine(freq_hz: f32, amplitude: f32, secs: f32) -> Vec<f32> {
    let n = samples(secs);
    (0..n)
        .map(|i| {
            amplitude
                * (2.0 * std::f32::consts::PI * freq_hz * i as f32 / SAMPLE_RATE as f32).sin()
        })
        .collect()
}

/// Single sample at 1.0, then silence.
fn gen_impulse(secs: f32) -> Vec<f32> {
    let mut buf = vec![0.0f32; samples(secs)];
    buf[0] = 1.0;
    buf
}

/// Short burst of full-scale samples followed by a quiet tail.
fn gen_transient(secs: f32) -> Vec<f32> {
    let n = samples(secs);
    (0..n)
        .map(|i| if i < 2_000 { 1.0 } else { 0.05 })
        .collect()
}

/// Rough voice simulation: quiet background → speech-level tone → quiet again.
fn gen_voice_sim(secs: f32) -> Vec<f32> {
    let n = samples(secs);
    (0..n)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            // Amplitude envelope
            let env: f32 = if t < 0.3 {
                0.02 // quiet background
            } else if t < 0.5 {
                0.02 + 0.78 * ((t - 0.3) / 0.2) // ramp up
            } else if t < secs - 0.3 {
                0.8 // sustained speech level
            } else if t < secs - 0.1 {
                0.8 * (1.0 - (t - (secs - 0.3)) / 0.2) // ramp down
            } else {
                0.02 // quiet again
            };
            // Simple harmonic content (rough voice-like timbre)
            env * (0.6 * (2.0 * std::f32::consts::PI * 200.0 * t).sin()
                + 0.3 * (2.0 * std::f32::consts::PI * 400.0 * t).sin()
                + 0.1 * (2.0 * std::f32::consts::PI * 800.0 * t).sin())
        })
        .collect()
}

fn samples(secs: f32) -> usize {
    (SAMPLE_RATE as f32 * secs) as usize
}

// ---------------------------------------------------------------------------
// Per-processor runs
// ---------------------------------------------------------------------------

fn run_hpf(label: &str, mut signal: Vec<f32>, cutoff_hz: f32) {
    let mut hp = HighPass::new(cutoff_hz);
    hp.prepare(SAMPLE_RATE as f32, signal.len());
    hp.process(&mut signal);
    let path = format!("test_output/{label}.wav");
    report(&path, &signal);
    write_wav(&path, &signal);
}

fn run_gate(label: &str, mut signal: Vec<f32>, threshold_db: f32) {
    let mut gate = Gate::new();
    gate.threshold_db = threshold_db;
    gate.prepare(SAMPLE_RATE as f32, signal.len());
    gate.process(&mut signal);
    let path = format!("test_output/{label}.wav");
    report(&path, &signal);
    write_wav(&path, &signal);
}

fn run_compressor(label: &str, mut signal: Vec<f32>, threshold_db: f32, ratio: f32) {
    let mut comp = Compressor::new();
    comp.threshold_db = threshold_db;
    comp.ratio = ratio;
    comp.prepare(SAMPLE_RATE as f32, signal.len());
    comp.process(&mut signal);
    let path = format!("test_output/{label}.wav");
    report(&path, &signal);
    write_wav(&path, &signal);
}

fn run_limiter(label: &str, mut signal: Vec<f32>, ceiling_db: f32) {
    let mut lim = Limiter::new();
    lim.ceiling_db = ceiling_db;
    lim.prepare(SAMPLE_RATE as f32, signal.len());
    lim.process(&mut signal);
    let path = format!("test_output/{label}.wav");
    report(&path, &signal);
    write_wav(&path, &signal);
}

fn run_chain(label: &str, mut signal: Vec<f32>, gain_db: f32) {
    let mut chain = VoiceChain::new();
    chain.gain.set_db(gain_db);
    chain.prepare(SAMPLE_RATE as f32, signal.len());
    chain.process(&mut signal);
    let path = format!("test_output/{label}.wav");
    report(&path, &signal);
    write_wav(&path, &signal);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    std::fs::create_dir_all("test_output").expect("could not create test_output/");
    println!("Writing WAV files to test_output/\n");

    // -----------------------------------------------------------------------
    // High-pass filter
    // -----------------------------------------------------------------------
    println!("High-pass filter (cutoff 80 Hz)");
    // Impulse response: feed a single sample and observe the filter's decay.
    // In a DAW you can zoom in to see the characteristic 2nd-order HPF ring.
    run_hpf("hpf_impulse",          gen_impulse(0.1),               80.0);
    // 20 Hz is far below cutoff — should be heavily attenuated.
    run_hpf("hpf_sine_20hz",        gen_sine(20.0,   0.9, 1.0),     80.0);
    // 80 Hz is at the cutoff — should be ~3 dB down.
    run_hpf("hpf_sine_80hz",        gen_sine(80.0,   0.9, 1.0),     80.0);
    // 1 kHz is well above cutoff — should pass with no meaningful loss.
    run_hpf("hpf_sine_1khz",        gen_sine(1000.0, 0.9, 1.0),     80.0);

    // -----------------------------------------------------------------------
    // Gate
    // -----------------------------------------------------------------------
    println!("\nGate (threshold -30 dB)");
    // Voice sim: quiet intro → loud section → quiet outro.
    // The gate should open when speech starts and close after it ends.
    run_gate("gate_voice_sim",      gen_voice_sim(2.0),              -30.0);
    // Silence should stay silent — gate stays closed the whole time.
    run_gate("gate_silence",        gen_silence(1.0),                -30.0);
    // Loud sustained sine — gate should open and stay open.
    run_gate("gate_loud_sine",      gen_sine(440.0, 0.8, 1.0),       -30.0);

    // -----------------------------------------------------------------------
    // Compressor
    // -----------------------------------------------------------------------
    println!("\nCompressor (threshold -18 dB, 4:1)");
    // Loud sine at 0 dB — well above threshold, should be noticeably reduced.
    run_compressor("comp_loud_sine",    gen_sine(440.0, 0.9, 1.0),  -18.0, 4.0);
    // Quiet sine at -30 dB — below threshold, should pass unchanged.
    run_compressor("comp_quiet_sine",
        gen_sine(440.0, db_to_linear(-30.0), 1.0), -18.0, 4.0);
    // Transient — shows the compressor's attack kicking in on the burst.
    run_compressor("comp_transient",    gen_transient(1.0),          -18.0, 4.0);

    // -----------------------------------------------------------------------
    // Limiter
    // -----------------------------------------------------------------------
    println!("\nLimiter (ceiling -1 dBFS)");
    // Over-threshold: 2× full scale — should be clamped to the ceiling.
    run_limiter("lim_overdrive",        vec![2.0f32; samples(0.5)],  -1.0);
    // Transient: the burst hits hard, the quiet tail should recover cleanly.
    run_limiter("lim_transient",        gen_transient(1.0),          -1.0);
    // Quiet sine — should pass through completely unchanged.
    run_limiter("lim_quiet_sine",
        gen_sine(440.0, db_to_linear(-12.0), 1.0), -1.0);

    // -----------------------------------------------------------------------
    // Full VoiceChain
    // -----------------------------------------------------------------------
    println!("\nFull VoiceChain (gain 0 dB, all defaults)");
    run_chain("chain_sine_440",         gen_sine(440.0, 0.9, 1.0),   0.0);
    run_chain("chain_transient",        gen_transient(1.0),           0.0);
    run_chain("chain_voice_sim",        gen_voice_sim(2.0),           0.0);
    // Crank the gain to +20 dB — the limiter should hold everything below -1 dBFS.
    run_chain("chain_overdrive_p20db",  gen_sine(440.0, 0.9, 1.0),  20.0);

    // -----------------------------------------------------------------------
    // Latency check
    // -----------------------------------------------------------------------
    // None of our processors buffer samples — they all work sample-by-sample.
    // This means they add zero samples of algorithmic latency: an impulse fed
    // at sample 0 produces output at sample 0.
    //
    // Note: the *gate* starts with gain=0 and ramps open over its attack time,
    // so its output at sample 0 will be near-zero — that is intentional gain
    // behaviour, not sample delay. The gate_impulse WAV shows this ramp.
    println!("\nLatency check — impulse through each processor");

    let mut imp = gen_impulse(0.05);
    let mut hp = HighPass::new(80.0);
    hp.prepare(SAMPLE_RATE as f32, imp.len());
    hp.process(&mut imp);
    println!(
        "  HighPass: sample[0] = {:.6}  (non-zero = no sample delay)",
        imp[0]
    );
    write_wav("test_output/latency_hpf_impulse.wav", &imp);

    let mut imp = gen_impulse(0.05);
    let mut lim = Limiter::new();
    lim.prepare(SAMPLE_RATE as f32, imp.len());
    lim.process(&mut imp);
    println!(
        "  Limiter:  sample[0] = {:.6}  (non-zero = no sample delay)",
        imp[0]
    );
    write_wav("test_output/latency_lim_impulse.wav", &imp);

    let mut imp = gen_impulse(0.05);
    let mut gate = Gate::new();
    gate.prepare(SAMPLE_RATE as f32, imp.len());
    gate.process(&mut imp);
    println!(
        "  Gate:     sample[0] = {:.6}  (near-zero is expected — gate starts closed)",
        imp[0]
    );
    write_wav("test_output/latency_gate_impulse.wav", &imp);

    println!();
    println!("Done. Open test_output/ in Audacity or a DAW.");
    println!("Things to look for:");
    println!("  hpf_impulse       — the HPF's decaying ring after the impulse");
    println!("  gate_voice_sim    — gate opening and closing around the speech section");
    println!("  comp_loud_sine    — waveform squashed by the compressor");
    println!("  chain_overdrive   — limiter holding the peak at exactly -1 dBFS");
    println!("  latency_*         — all non-gate processors start output at sample 0");
}
