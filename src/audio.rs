// Audio device and stream handling.
//
// Responsibilities:
// - list input/output devices
// - open microphone and output device
// - connect CPAL callbacks to the DSP chain
// - handle stream errors and device reconnection

use std::sync::{
    mpsc,
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, StreamConfig};
use ringbuf::{
    traits::{Consumer, Producer, Split},
    HeapRb,
};

use crate::dsp::{Processor, VoiceChain};
use crate::state::Settings;

// ---------------------------------------------------------------------------
// Device listing
// ---------------------------------------------------------------------------

/// Print all available input and output devices to stdout, along with their
/// supported sample rates and channel counts.
pub fn list_devices() {
    let host = cpal::default_host();

    let default_input_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();
    let default_output_name = host
        .default_output_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();

    println!("Input devices:");
    match host.input_devices() {
        Err(e) => println!("  error listing input devices: {e}"),
        Ok(devices) => {
            let mut found = false;
            for device in devices {
                found = true;
                let name = device.name().unwrap_or_else(|_| "<unknown>".into());
                let marker = if name == default_input_name { " (default)" } else { "" };
                println!("  {name}{marker}");
                print_supported_configs(&device, true);
            }
            if !found {
                println!("  none found");
            }
        }
    }

    println!();
    println!("Output devices:");
    match host.output_devices() {
        Err(e) => println!("  error listing output devices: {e}"),
        Ok(devices) => {
            let mut found = false;
            for device in devices {
                found = true;
                let name = device.name().unwrap_or_else(|_| "<unknown>".into());
                let marker = if name == default_output_name { " (default)" } else { "" };
                println!("  {name}{marker}");
                print_supported_configs(&device, false);
            }
            if !found {
                println!("  none found");
            }
        }
    }
}

fn print_supported_configs(device: &Device, is_input: bool) {
    if is_input {
        match device.supported_input_configs() {
            Err(e) => println!("    (could not query configs: {e})"),
            Ok(configs) => {
                for config in configs {
                    print_config_range(&config);
                }
            }
        }
    } else {
        match device.supported_output_configs() {
            Err(e) => println!("    (could not query configs: {e})"),
            Ok(configs) => {
                for config in configs {
                    print_config_range(&config);
                }
            }
        }
    }
}

fn print_config_range(config: &cpal::SupportedStreamConfigRange) {
    let channels = config.channels();
    let min_hz = config.min_sample_rate().0;
    let max_hz = config.max_sample_rate().0;
    let format = config.sample_format();

    let rate_str = if min_hz == max_hz {
        format!("{min_hz} Hz")
    } else {
        format!("{min_hz}–{max_hz} Hz")
    };

    let ch_str = match channels {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        n => format!("{n}ch"),
    };

    println!("    {rate_str}, {ch_str}, {format}");
}

// ---------------------------------------------------------------------------
// Device lookup
// ---------------------------------------------------------------------------

fn find_input_device(host: &cpal::Host, name: &str) -> Option<Device> {
    let needle = name.to_lowercase();
    host.input_devices().ok()?.find(|d| {
        d.name()
            .map(|n| n.to_lowercase().contains(&needle))
            .unwrap_or(false)
    })
}

fn find_output_device(host: &cpal::Host, name: &str) -> Option<Device> {
    let needle = name.to_lowercase();
    host.output_devices().ok()?.find(|d| {
        d.name()
            .map(|n| n.to_lowercase().contains(&needle))
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Config negotiation
// ---------------------------------------------------------------------------

/// Choose `StreamConfig`s for a matched input/output pair.
///
/// The output device's default sample rate is treated as authoritative.
/// The input device is queried for a supported config at that rate; if none
/// is found the input falls back to its own default (and the caller will see
/// a rate mismatch warning).
fn negotiate_configs(
    input_device: &Device,
    output_device: &Device,
) -> Result<(StreamConfig, StreamConfig), DeviceError> {
    let output_config: StreamConfig = output_device
        .default_output_config()
        .map_err(|e| DeviceError::StreamBuildFailed(format!("no output config: {e}")))?
        .into();

    let out_rate = output_config.sample_rate;

    // Try to find an input config range that supports the output sample rate.
    let input_config: StreamConfig = input_device
        .supported_input_configs()
        .ok()
        .and_then(|mut ranges| {
            ranges.find(|r| {
                r.min_sample_rate() <= out_rate && out_rate <= r.max_sample_rate()
            })
        })
        .map(|r| StreamConfig {
            channels: r.channels(),
            sample_rate: out_rate,
            buffer_size: cpal::BufferSize::Default,
        })
        .unwrap_or_else(|| {
            input_device
                .default_input_config()
                .map(|c| c.into())
                .unwrap_or_else(|_| StreamConfig {
                    channels: 1,
                    sample_rate: out_rate,
                    buffer_size: cpal::BufferSize::Default,
                })
        });

    Ok((input_config, output_config))
}

// ---------------------------------------------------------------------------
// Device error
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum DeviceError {
    NotFound(String),
    StreamBuildFailed(String),
}

// ---------------------------------------------------------------------------
// try_start
// ---------------------------------------------------------------------------

/// Attempt to open the requested devices, build streams, and return them
/// together with a channel that receives stream-error messages.
///
/// The streams are *not* played yet — the caller is responsible for calling
/// `.play()` on both and later dropping them when a reconnect is needed.
fn try_start(
    host: &cpal::Host,
    input_name: Option<&str>,
    output_name: Option<&str>,
    settings: &Settings,
) -> Result<(cpal::Stream, cpal::Stream, mpsc::Receiver<String>), DeviceError> {
    // --- Resolve devices -------------------------------------------------------

    let input_device = match input_name {
        Some(name) => find_input_device(host, name)
            .ok_or_else(|| DeviceError::NotFound(format!("no input device matching '{name}'")))?,
        None => host
            .default_input_device()
            .ok_or_else(|| DeviceError::NotFound("no default input device".into()))?,
    };

    let output_device = match output_name {
        Some(name) => find_output_device(host, name)
            .ok_or_else(|| DeviceError::NotFound(format!("no output device matching '{name}'")))?,
        None => host
            .default_output_device()
            .ok_or_else(|| DeviceError::NotFound("no default output device".into()))?,
    };

    let in_name = input_device.name().unwrap_or_else(|_| "<unknown>".into());
    let out_name = output_device.name().unwrap_or_else(|_| "<unknown>".into());

    // --- Pick configs ----------------------------------------------------------

    let (input_config, output_config) =
        negotiate_configs(&input_device, &output_device)?;

    let in_rate = input_config.sample_rate.0;
    let out_rate = output_config.sample_rate.0;
    let in_ch = input_config.channels as usize;
    let out_ch = output_config.channels as usize;

    println!("Input:  {in_name} — {in_rate} Hz, {in_ch}ch");
    println!("Output: {out_name} — {out_rate} Hz, {out_ch}ch");

    if in_rate != out_rate {
        println!(
            "Warning: sample rate mismatch ({in_rate} Hz in vs {out_rate} Hz out). \
             Resampling is not yet implemented — audio will be pitched wrong."
        );
    }

    // --- Ring buffer -----------------------------------------------------------

    let latency_frames = 4096usize;
    let (mut producer, mut consumer) = HeapRb::<f32>::new(latency_frames * 4).split();

    // Pre-fill with silence to avoid an underrun on the first output callback.
    for _ in 0..latency_frames {
        producer.try_push(0.0).ok();
    }

    // --- DSP chain (lives in the output callback) ------------------------------

    let mut mono_buf = vec![0.0f32; latency_frames * 2];
    let mut chain = VoiceChain::new();
    chain.prepare(out_rate as f32, latency_frames);
    chain.apply_settings(settings);

    // --- Error channel ---------------------------------------------------------
    //
    // Bounded to 1 so try_send() in callbacks never blocks. A second error
    // arriving while one is already queued is silently dropped — that's fine,
    // because any error triggers a full reconnect.

    let (error_tx, error_rx) = mpsc::sync_channel::<String>(1);
    let error_tx_out = error_tx.clone();

    // --- Build streams ---------------------------------------------------------

    let input_stream = input_device
        .build_input_stream(
            &input_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let frames = data.len() / in_ch.max(1);
                for frame in 0..frames {
                    // Take channel 0 only — on an aggregate device the mic is first;
                    // averaging all channels would mix in BlackHole's loopback.
                    producer.try_push(data[frame * in_ch]).ok();
                }
            },
            move |err| {
                error_tx.try_send(format!("input: {err}")).ok();
            },
            None,
        )
        .map_err(|e| DeviceError::StreamBuildFailed(format!("failed to build input stream: {e}")))?;

    let output_stream = output_device
        .build_output_stream(
            &output_config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / out_ch.max(1);
                let mono = &mut mono_buf[..frames];

                for s in mono.iter_mut() {
                    *s = consumer.try_pop().unwrap_or(0.0);
                }

                chain.process(mono);

                for (i, chunk) in data.chunks_mut(out_ch).enumerate() {
                    for sample in chunk.iter_mut() {
                        *sample = mono[i];
                    }
                }
            },
            move |err| {
                error_tx_out.try_send(format!("output: {err}")).ok();
            },
            None,
        )
        .map_err(|e| DeviceError::StreamBuildFailed(format!("failed to build output stream: {e}")))?;

    Ok((input_stream, output_stream, error_rx))
}

// ---------------------------------------------------------------------------
// Passthrough with reconnection
// ---------------------------------------------------------------------------

/// Open `input_name` and `output_name` (substring match), apply `settings`,
/// wire them through the DSP chain, and block until the process is killed.
///
/// If either stream reports an error the app waits two seconds and tries to
/// reopen the devices. This handles microphone disconnection, output device
/// changes, and transient driver failures without requiring a restart.
///
/// Pass `None` for device names to use the system defaults.
pub fn start_passthrough(input_name: Option<&str>, output_name: Option<&str>, settings: Settings) {
    let host = cpal::default_host();

    loop {
        match try_start(&host, input_name, output_name, &settings) {
            Ok((input_stream, output_stream, error_rx)) => {
                input_stream.play().expect("failed to start input stream");
                output_stream.play().expect("failed to start output stream");

                println!("Running — press Ctrl+C to stop.");

                // Block until a stream error arrives or the channel is closed.
                loop {
                    match error_rx.recv_timeout(Duration::from_secs(5)) {
                        // No error yet — keep waiting.
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        // An error was reported.
                        Ok(msg) => {
                            eprintln!("Stream error: {msg}");
                            break;
                        }
                        // Channel closed (both senders dropped) — shouldn't
                        // happen normally, but treat it as an error.
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            eprintln!("Stream channel closed unexpectedly.");
                            break;
                        }
                    }
                }

                // Drop the streams to free the devices before reconnecting.
                drop(input_stream);
                drop(output_stream);
                eprintln!("Attempting to reconnect in 2 seconds...");
            }
            Err(DeviceError::NotFound(msg)) => {
                eprintln!("Device not found: {msg}");
                eprintln!("Retrying in 2 seconds — plug in your device or check the name.");
            }
            Err(DeviceError::StreamBuildFailed(msg)) => {
                eprintln!("Failed to start streams: {msg}");
                eprintln!("Retrying in 2 seconds...");
            }
        }

        std::thread::sleep(Duration::from_secs(2));
    }
}

// ---------------------------------------------------------------------------
// GUI audio engine
// ---------------------------------------------------------------------------

/// Commands sent from the GUI thread to the audio engine thread.
pub enum AudioCmd {
    ChangeDevices {
        input_name: Option<String>,
        output_name: Option<String>,
        monitor_name: Option<String>,
        monitor_enabled: bool,
    },
    UpdateSettings(Box<Settings>),
    Stop,
}

/// Runs the audio engine on a background thread with lock-free communication.
///
/// Settings travel via an `mpsc::SyncSender` → per-session ring buffer (cap 1)
/// → output callback `try_pop()`. Peak level travels back via an `Arc<AtomicU32>`
/// storing the f32 bit-pattern of the linear amplitude.
pub struct AudioEngine {
    peak_meter: Arc<AtomicU32>,
    /// Toggled without reconnect — the output callback checks this each buffer.
    monitor_active: Arc<AtomicBool>,
    cmd_tx: mpsc::SyncSender<AudioCmd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioEngine {
    /// Spawn the audio engine thread and begin streaming immediately.
    pub fn new(
        input: Option<String>,
        output: Option<String>,
        settings: Settings,
        monitor: Option<String>,
        monitor_enabled: bool,
    ) -> Self {
        let peak_meter = Arc::new(AtomicU32::new(0u32));
        let monitor_active = Arc::new(AtomicBool::new(monitor_enabled));
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<AudioCmd>(4);

        let peak_meter_clone = Arc::clone(&peak_meter);
        let monitor_active_clone = Arc::clone(&monitor_active);
        let thread = std::thread::spawn(move || {
            audio_engine_thread(input, output, settings, cmd_rx, peak_meter_clone, monitor, monitor_enabled, monitor_active_clone);
        });

        Self {
            peak_meter,
            monitor_active,
            cmd_tx,
            thread: Some(thread),
        }
    }

    /// Send updated settings to the audio callback (non-blocking; drops if full).
    pub fn update_settings(&self, s: Settings) {
        self.cmd_tx
            .try_send(AudioCmd::UpdateSettings(Box::new(s)))
            .ok();
    }

    /// Request a device change. The engine will reconnect with the new devices.
    pub fn change_devices(
        &self,
        input: Option<String>,
        output: Option<String>,
        monitor: Option<String>,
        monitor_enabled: bool,
    ) {
        self.cmd_tx
            .try_send(AudioCmd::ChangeDevices {
                input_name: input,
                output_name: output,
                monitor_name: monitor,
                monitor_enabled,
            })
            .ok();
    }

    /// Toggle monitor on/off without reconnecting streams.
    pub fn set_monitor_active(&self, enabled: bool) {
        self.monitor_active.store(enabled, Ordering::Relaxed);
    }

    /// Read the latest peak level in dBFS from the audio callback (lock-free).
    pub fn peak_db(&self) -> f32 {
        let bits = self.peak_meter.load(Ordering::Relaxed);
        crate::dsp::linear_to_db(f32::from_bits(bits))
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.cmd_tx.try_send(AudioCmd::Stop).ok();
        if let Some(t) = self.thread.take() {
            t.join().ok();
        }
    }
}

// ---------------------------------------------------------------------------
// try_start_engine — like try_start but with GUI ring buffer + peak meter
// ---------------------------------------------------------------------------

fn try_start_engine(
    host: &cpal::Host,
    input_name: Option<&str>,
    output_name: Option<&str>,
    settings: &Settings,
    mut settings_consumer: ringbuf::HeapCons<Settings>,
    peak_meter: Arc<AtomicU32>,
    monitor_name: Option<&str>,
    monitor_enabled: bool,
    monitor_active: Arc<AtomicBool>,
) -> Result<(cpal::Stream, cpal::Stream, Option<cpal::Stream>, mpsc::Receiver<String>), DeviceError> {
    // --- Resolve devices -------------------------------------------------------

    let input_device = match input_name {
        Some(name) => find_input_device(host, name)
            .ok_or_else(|| DeviceError::NotFound(format!("no input device matching '{name}'")))?,
        None => host
            .default_input_device()
            .ok_or_else(|| DeviceError::NotFound("no default input device".into()))?,
    };

    let output_device = match output_name {
        Some(name) => find_output_device(host, name)
            .ok_or_else(|| DeviceError::NotFound(format!("no output device matching '{name}'")))?,
        None => host
            .default_output_device()
            .ok_or_else(|| DeviceError::NotFound("no default output device".into()))?,
    };

    let in_name = input_device.name().unwrap_or_else(|_| "<unknown>".into());
    let out_name = output_device.name().unwrap_or_else(|_| "<unknown>".into());

    // --- Pick configs ----------------------------------------------------------

    let (input_config, output_config) =
        negotiate_configs(&input_device, &output_device)?;

    let in_rate = input_config.sample_rate.0;
    let out_rate = output_config.sample_rate.0;
    let in_ch = input_config.channels as usize;
    let out_ch = output_config.channels as usize;

    eprintln!("Input:  {in_name} — {in_rate} Hz, {in_ch}ch");
    eprintln!("Output: {out_name} — {out_rate} Hz, {out_ch}ch");

    if in_rate != out_rate {
        eprintln!(
            "Warning: sample rate mismatch ({in_rate} Hz in vs {out_rate} Hz out). \
             Resampling is not yet implemented — audio may sound wrong."
        );
    }

    // --- Ring buffer -----------------------------------------------------------

    let latency_frames = 2048usize;
    let rb_capacity = latency_frames * 8;
    let (mut producer, mut consumer) = HeapRb::<f32>::new(rb_capacity).split();

    for _ in 0..rb_capacity / 2 {
        producer.try_push(0.0).ok();
    }

    // --- DSP chain (lives in the output callback) ------------------------------

    let mut mono_buf = vec![0.0f32; latency_frames * 2];
    let mut chain = crate::dsp::VoiceChain::new();
    chain.prepare(out_rate as f32, latency_frames);
    chain.apply_settings(settings);

    // --- Monitor ring buffer (fed by output callback, consumed by monitor stream)

    let (mut monitor_producer, mut monitor_consumer) =
        HeapRb::<f32>::new(latency_frames * 4).split();
    for _ in 0..latency_frames {
        monitor_producer.try_push(0.0).ok();
    }

    // --- Error channel ---------------------------------------------------------

    let (error_tx, error_rx) = mpsc::sync_channel::<String>(1);
    let error_tx_out = error_tx.clone();
    let error_tx_mon = error_tx.clone();

    // --- Build streams ---------------------------------------------------------

    let input_stream = input_device
        .build_input_stream(
            &input_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let frames = data.len() / in_ch.max(1);
                for frame in 0..frames {
                    // Take channel 0 only — on an aggregate device the mic is first;
                    // averaging all channels would mix in BlackHole's loopback.
                    producer.try_push(data[frame * in_ch]).ok();
                }
            },
            move |err| {
                error_tx.try_send(format!("input: {err}")).ok();
            },
            None,
        )
        .map_err(|e| DeviceError::StreamBuildFailed(format!("failed to build input stream: {e}")))?;

    let mut last_out = 0.0f32;
    let output_stream = output_device
        .build_output_stream(
            &output_config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // Poll for new settings (lock-free).
                if let Some(s) = settings_consumer.try_pop() {
                    chain.apply_settings(&s);
                }

                let frames = data.len() / out_ch.max(1);
                let mono = &mut mono_buf[..frames];

                for s in mono.iter_mut() {
                    match consumer.try_pop() {
                        Some(v) => { last_out = v; *s = v; }
                        // Gentle fade on underrun avoids hard silence that sounds robotic.
                        None => { last_out *= 0.9; *s = last_out; }
                    }
                }

                chain.process(mono);

                // Write peak level for GUI meter.
                let peak = chain.peak_meter.peak_linear();
                peak_meter.store(peak.to_bits(), Ordering::Relaxed);
                chain.peak_meter.reset();

                // Feed monitor ring buffer only when monitoring is active.
                if monitor_active.load(Ordering::Relaxed) {
                    for &s in mono.iter() {
                        monitor_producer.try_push(s).ok();
                    }
                }

                for (i, chunk) in data.chunks_mut(out_ch).enumerate() {
                    for sample in chunk.iter_mut() {
                        *sample = mono[i];
                    }
                }
            },
            move |err| {
                error_tx_out.try_send(format!("output: {err}")).ok();
            },
            None,
        )
        .map_err(|e| DeviceError::StreamBuildFailed(format!("failed to build output stream: {e}")))?;

    // --- Monitor stream (optional) ---------------------------------------------

    let monitor_stream = if monitor_enabled {
        let mon_dev = monitor_name.and_then(|n| find_output_device(host, n));
        match mon_dev {
            None => {
                if monitor_name.is_some() {
                    eprintln!("Warning: monitor device not found, monitoring disabled.");
                }
                None
            }
            Some(mon_device) => {
                let mon_name_str = mon_device.name().unwrap_or_else(|_| "<unknown>".into());
                let mon_default = mon_device
                    .default_output_config()
                    .map_err(|e| DeviceError::StreamBuildFailed(format!("monitor config: {e}")))?;
                let mon_ch = mon_default.channels() as usize;
                let mon_rate = mon_default.sample_rate().0;
                let mon_config: StreamConfig = mon_default.into();
                eprintln!("Monitor: {mon_name_str} — {mon_rate} Hz, {mon_ch}ch");
                // Ratio: how many output samples per input sample.
                // e.g. out=48000, mon=44100 → ratio ≈ 0.919 → consume ~0.919 src per dst frame.
                let resample_ratio = out_rate as f64 / mon_rate as f64;
                let mut resample_pos = 0.0f64;
                let mut last_sample = 0.0f32;
                match mon_device.build_output_stream(
                    &mon_config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        for chunk in data.chunks_mut(mon_ch) {
                            // Pop src samples until we've advanced past this dst frame.
                            resample_pos += resample_ratio;
                            while resample_pos >= 1.0 {
                                last_sample = monitor_consumer.try_pop().unwrap_or(0.0);
                                resample_pos -= 1.0;
                            }
                            for sample in chunk.iter_mut() {
                                *sample = last_sample;
                            }
                        }
                    },
                    move |err| {
                        error_tx_mon.try_send(format!("monitor: {err}")).ok();
                    },
                    None,
                ) {
                    Ok(stream) => Some(stream),
                    Err(e) => {
                        eprintln!("Warning: could not build monitor stream: {e}");
                        None
                    }
                }
            }
        }
    } else {
        None
    };

    Ok((input_stream, output_stream, monitor_stream, error_rx))
}

// ---------------------------------------------------------------------------
// audio_engine_thread
// ---------------------------------------------------------------------------

fn audio_engine_thread(
    initial_input: Option<String>,
    initial_output: Option<String>,
    initial_settings: Settings,
    cmd_rx: mpsc::Receiver<AudioCmd>,
    peak_meter: Arc<AtomicU32>,
    initial_monitor: Option<String>,
    initial_monitor_enabled: bool,
    monitor_active: Arc<AtomicBool>,
) {
    let host = cpal::default_host();
    let mut input_name = initial_input;
    let mut output_name = initial_output;
    let mut current_settings = initial_settings;
    let mut monitor_name = initial_monitor;
    let mut monitor_enabled = initial_monitor_enabled;

    'reconnect: loop {
        // Drain any pending commands before attempting to connect.
        loop {
            match cmd_rx.try_recv() {
                Ok(AudioCmd::Stop) => return,
                Ok(AudioCmd::ChangeDevices { input_name: i, output_name: o, monitor_name: m, monitor_enabled: me }) => {
                    input_name = i;
                    output_name = o;
                    monitor_name = m;
                    monitor_enabled = me;
                }
                Ok(AudioCmd::UpdateSettings(s)) => {
                    current_settings = *s;
                }
                Err(_) => break,
            }
        }

        // Per-session settings ring buffer (capacity 1, lock-free).
        let (mut settings_producer, settings_consumer) =
            HeapRb::<Settings>::new(1).split();

        match try_start_engine(
            &host,
            input_name.as_deref(),
            output_name.as_deref(),
            &current_settings,
            settings_consumer,
            Arc::clone(&peak_meter),
            monitor_name.as_deref(),
            monitor_enabled,
            Arc::clone(&monitor_active),
        ) {
            Ok((in_stream, out_stream, mon_stream, error_rx)) => {
                if let Err(e) = in_stream.play() {
                    eprintln!("Failed to start input stream: {e}");
                    std::thread::sleep(Duration::from_secs(2));
                    continue 'reconnect;
                }
                if let Err(e) = out_stream.play() {
                    eprintln!("Failed to start output stream: {e}");
                    std::thread::sleep(Duration::from_secs(2));
                    continue 'reconnect;
                }
                if let Some(ref ms) = mon_stream {
                    if let Err(e) = ms.play() {
                        eprintln!("Warning: failed to start monitor stream: {e}");
                    }
                }

                loop {
                    match error_rx.recv_timeout(Duration::from_millis(200)) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            loop {
                                match cmd_rx.try_recv() {
                                    Ok(AudioCmd::Stop) => return,
                                    Ok(AudioCmd::UpdateSettings(s)) => {
                                        current_settings = *s;
                                        settings_producer.try_push(current_settings.clone()).ok();
                                    }
                                    Ok(AudioCmd::ChangeDevices { input_name: i, output_name: o, monitor_name: m, monitor_enabled: me }) => {
                                        input_name = i;
                                        output_name = o;
                                        monitor_name = m;
                                        monitor_enabled = me;
                                        drop(in_stream);
                                        drop(out_stream);
                                        drop(mon_stream);
                                        eprintln!("Device change requested, reconnecting...");
                                        std::thread::sleep(Duration::from_millis(100));
                                        continue 'reconnect;
                                    }
                                    Err(_) => break,
                                }
                            }
                        }
                        Ok(msg) => {
                            eprintln!("Stream error: {msg}");
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            eprintln!("Stream channel closed unexpectedly.");
                            break;
                        }
                    }
                }

                drop(in_stream);
                drop(out_stream);
                drop(mon_stream);
                eprintln!("Attempting to reconnect in 2 seconds...");
            }
            Err(DeviceError::NotFound(msg)) => {
                eprintln!("Device not found: {msg}");
                eprintln!("Retrying in 2 seconds...");
            }
            Err(DeviceError::StreamBuildFailed(msg)) => {
                eprintln!("Failed to start streams: {msg}");
                eprintln!("Retrying in 2 seconds...");
            }
        }

        std::thread::sleep(Duration::from_secs(2));
    }
}

// ---------------------------------------------------------------------------
// Device name listing helpers for the GUI
// ---------------------------------------------------------------------------

/// Return the names of all available input devices.
pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|devs| {
            devs.filter_map(|d| d.name().ok()).collect()
        })
        .unwrap_or_default()
}

/// Return the names of all available output devices.
pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|devs| {
            devs.filter_map(|d| d.name().ok()).collect()
        })
        .unwrap_or_default()
}
