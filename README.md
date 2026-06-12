# micapp

Real-time microphone processing for Discord, OBS, and voice chat.

```
mic input → gain → high-pass → gate → EQ → compressor → limiter → output device
```

Designed for streamers and gamers who want clean voice audio without running a full DAW.

## Features

- Low-latency real-time processing
- Noise gate, compressor, limiter, EQ, high-pass filter
- Routes to any output device — use with BlackHole (macOS) or VB-Audio Virtual Cable (Windows)
- Monitor output — hear yourself through headphones while routing to a virtual cable
- Saves settings automatically
- System tray — runs in the background, starts hidden on login
- Cross-platform: macOS, Windows, Linux

## Quickstart

```bash
git clone https://github.com/yourname/micapp
cd micapp
cargo run --bin micapp-gui
```

Set **Input** to your microphone and **Output** to your virtual audio cable. In Discord/OBS, select the virtual cable as the microphone input.

## Virtual audio cable setup

### macOS — BlackHole

```bash
brew install blackhole-2ch
```

Reboot after installing. Set Output to `BlackHole 2ch`. In Discord: mic = `BlackHole 2ch`.

### Windows — VB-Audio Virtual Cable

Download and install [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (free).  
Set Output to `CABLE Input`. In Discord: mic = `CABLE Output`.

### Linux — PipeWire

```bash
pactl load-module module-null-sink sink_name=micapp
```

Set Output to `micapp`. In Discord: mic = `micapp.monitor`.

## Signal chain

| Stage | What it does |
|---|---|
| Gain | Input trim in dB |
| High-pass | Removes low rumble (default: 80 Hz) |
| Gate | Cuts background noise during silence |
| EQ | 4-band parametric equalizer |
| Compressor | Evens out volume differences |
| Limiter | Hard ceiling to prevent clipping |

Each stage can be bypassed individually.

## Building from source

Requires Rust 1.75+.

**macOS / Linux**
```bash
cargo build --release
```

**Linux** — also needs ALSA headers:
```bash
sudo apt install libasound2-dev pkg-config
cargo build --release
```

**Windows** — install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) then:
```bash
cargo build --release
```

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
