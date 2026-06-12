// Application state and settings.

use serde::{Deserialize, Serialize};
use std::path::Path;

fn default_true() -> bool { true }

/// All user-configurable parameters for the voice chain.
///
/// The defaults match the processor defaults in dsp.rs. Saving the default
/// settings to disk produces a ready-to-edit starting point.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Last-used input device name. `None` means use the system default.
    pub input_device: Option<String>,
    /// Last-used output device name. `None` means use the system default.
    pub output_device: Option<String>,
    /// Optional monitor device (e.g. headphones) to hear processed audio live.
    pub monitor_device: Option<String>,
    pub monitor_enabled: bool,

    pub gain_db: f32,
    #[serde(default = "default_true")]
    pub gain_enabled: bool,

    pub high_pass_cutoff_hz: f32,
    #[serde(default = "default_true")]
    pub high_pass_enabled: bool,

    pub gate_threshold_db: f32,
    pub gate_attack_ms: f32,
    pub gate_release_ms: f32,
    pub gate_hold_ms: f32,
    #[serde(default = "default_true")]
    pub gate_enabled: bool,

    pub compressor_threshold_db: f32,
    pub compressor_ratio: f32,
    pub compressor_attack_ms: f32,
    pub compressor_release_ms: f32,
    pub compressor_makeup_db: f32,
    #[serde(default = "default_true")]
    pub compressor_enabled: bool,

    pub limiter_ceiling_db: f32,
    pub limiter_release_ms: f32,
    #[serde(default = "default_true")]
    pub limiter_enabled: bool,

    /// Centre/corner frequencies for each EQ band (Hz).
    /// Band order: low shelf, low-mid peak, high-mid peak, high shelf.
    pub eq_band_freq_hz: [f32; 4],
    /// Gain for each EQ band (dB). 0.0 = transparent.
    pub eq_band_gain_db: [f32; 4],
    /// Q factor for each EQ band. Only used by peak bands.
    pub eq_band_q: [f32; 4],
    #[serde(default = "default_true")]
    pub eq_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            monitor_device: None,
            monitor_enabled: false,

            gain_db: 0.0,
            gain_enabled: true,

            high_pass_cutoff_hz: 80.0,
            high_pass_enabled: true,

            gate_threshold_db: -40.0,
            gate_attack_ms: 5.0,
            gate_release_ms: 150.0,
            gate_hold_ms: 100.0,
            gate_enabled: true,

            compressor_threshold_db: -18.0,
            compressor_ratio: 3.0,
            compressor_attack_ms: 20.0,
            compressor_release_ms: 200.0,
            compressor_makeup_db: 0.0,
            compressor_enabled: true,

            limiter_ceiling_db: -1.0,
            limiter_release_ms: 50.0,
            limiter_enabled: true,

            eq_band_freq_hz: [120.0, 300.0, 3000.0, 8000.0],
            eq_band_gain_db: [0.0; 4],
            eq_band_q:       [0.707, 0.7, 0.7, 0.707],
            eq_enabled: true,
        }
    }
}

/// Return the platform-appropriate path for the persistent config file.
///
/// Windows : `%APPDATA%\micapp\config.toml`
/// macOS   : `~/Library/Application Support/micapp/config.toml`
/// Linux   : `$XDG_CONFIG_HOME/micapp/config.toml` or `~/.config/micapp/config.toml`
pub fn config_path() -> std::path::PathBuf {
    // Windows: use APPDATA — always set by the OS, even at login-startup.
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let mut p = std::path::PathBuf::from(appdata);
        p.push("micapp\\config.toml");
        return p;
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push("Library/Application Support/micapp/config.toml");
        return p;
    }

    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let mut p = std::path::PathBuf::from(xdg);
        p.push("micapp/config.toml");
        return p;
    }

    if let Some(home) = std::env::var_os("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push(".config/micapp/config.toml");
        return p;
    }

    std::path::PathBuf::from("config.toml")
}

impl Settings {
    /// Load settings from a TOML file. Missing keys fall back to defaults.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        toml::from_str(&text)
            .map_err(|e| format!("could not parse {}: {e}", path.display()))
    }

    /// Write settings to a TOML file, creating it if it does not exist.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| format!("could not serialise settings: {e}"))?;
        std::fs::write(path, text)
            .map_err(|e| format!("could not write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_roundtrip() {
        let original = Settings::default();
        let text = toml::to_string_pretty(&original).unwrap();
        let parsed: Settings = toml::from_str(&text).unwrap();
        assert!((original.gain_db - parsed.gain_db).abs() < 1e-6);
        assert!((original.compressor_ratio - parsed.compressor_ratio).abs() < 1e-6);
        assert!((original.limiter_ceiling_db - parsed.limiter_ceiling_db).abs() < 1e-6);
    }

    #[test]
    fn partial_toml_uses_defaults_for_missing_keys() {
        // Only override one field; everything else should fall back to Default.
        let toml = "gain_db = 6.0\n";
        let settings: Settings = toml::from_str(toml).unwrap();
        assert!((settings.gain_db - 6.0).abs() < 1e-6);
        assert!((settings.high_pass_cutoff_hz - 80.0).abs() < 1e-6);
        assert!((settings.compressor_ratio - 3.0).abs() < 1e-6);
    }
}
