use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Baud {
    Rate(u32),
    Auto(String),
}
impl Default for Baud {
    fn default() -> Self {
        Self::Auto("auto".into())
    }
}
impl Baud {
    pub fn parse(value: &str) -> Result<Self> {
        if value == "auto" {
            Ok(Self::default())
        } else {
            let rate: u32 = value
                .parse()
                .context("baud must be 'auto' or a positive integer")?;
            validate_rate(rate)?;
            Ok(Self::Rate(rate))
        }
    }
    pub fn fixed(&self) -> Option<u32> {
        if let Self::Rate(n) = self {
            Some(*n)
        } else {
            None
        }
    }
}
pub fn validate_rate(n: u32) -> Result<()> {
    if !(50..=4_000_000).contains(&n) {
        bail!("baud must be between 50 and 4000000");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Adapters {
    pub prefer: Vec<String>,
    pub serial_number: Option<String>,
}
impl Default for Adapters {
    fn default() -> Self {
        Self {
            prefer: ["cp210x", "tigard", "ftdi", "ch34x", "pl2303", "cdc-acm"]
                .map(String::from)
                .to_vec(),
            serial_number: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Serial {
    pub port: Option<String>,
    pub baud: Baud,
    pub baud_rates: Vec<u32>,
    pub sample_ms: u64,
    pub min_bytes: usize,
    pub data_bits: u8,
    pub parity: String,
    pub stop_bits: u8,
    pub flow_control: String,
    pub dtr: bool,
    pub rts: bool,
}
impl Default for Serial {
    fn default() -> Self {
        Self {
            port: None,
            baud: Baud::default(),
            baud_rates: vec![115200, 57600, 38400, 19200, 9600, 230400, 460800, 921600],
            sample_ms: 700,
            min_bytes: 32,
            data_bits: 8,
            parity: "none".into(),
            stop_bits: 1,
            flow_control: "none".into(),
            dtr: false,
            rts: false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
    pub enabled: bool,
    pub directory: PathBuf,
}
impl Default for Logging {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: PathBuf::from("."),
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub adapters: Adapters,
    pub serial: Serial,
    pub logging: Logging,
    pub formulas: Vec<crate::formula::Definition>,
    pub terminal: Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Terminal {
    pub scrollback_lines: usize,
    pub mouse: bool,
}
impl Default for Terminal {
    fn default() -> Self {
        Self {
            scrollback_lines: 10_000,
            mouse: true,
        }
    }
}
impl Terminal {
    pub fn validate(&self) -> Result<()> {
        if self.scrollback_lines > 100_000 {
            bail!("terminal.scrollback_lines must be 0..100000");
        }
        Ok(())
    }
}

pub fn default_path() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".config"))
        .join("sericon/config.toml")
}
impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let p = path.map(PathBuf::from).unwrap_or_else(default_path);
        let mut config: Self = match fs::read_to_string(&p) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("invalid configuration: {}", p.display()))?,
            Err(e) if path.is_none() && e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                return Err(e).with_context(|| format!("read configuration: {}", p.display()));
            }
        };
        let parent = std::path::absolute(&p)?.parent().unwrap().to_path_buf();
        for formula in &mut config.formulas {
            if let Some(script) = &mut formula.script
                && script.is_relative()
            {
                *script = parent.join(&*script);
            }
        }
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        self.terminal.validate()?;
        match &self.serial.baud {
            Baud::Rate(n) => validate_rate(*n)?,
            Baud::Auto(s) if s == "auto" => (),
            _ => bail!("serial.baud must be an integer or 'auto'"),
        }
        if self.serial.baud_rates.is_empty() || self.serial.baud_rates.len() > 64 {
            bail!("baud_rates must contain 1 to 64 candidates");
        }
        for n in &self.serial.baud_rates {
            validate_rate(*n)?;
        }
        if !(100..=10_000).contains(&self.serial.sample_ms) {
            bail!("sample_ms must be 100..10000");
        }
        if !(16..=4096).contains(&self.serial.min_bytes) {
            bail!("min_bytes must be 16..4096");
        }
        if !(5..=8).contains(&self.serial.data_bits) || ![1, 2].contains(&self.serial.stop_bits) {
            bail!("invalid data_bits or stop_bits");
        }
        if !["none", "odd", "even"].contains(&self.serial.parity.as_str()) {
            bail!("parity must be none, odd, or even");
        }
        if !["none", "hardware", "software"].contains(&self.serial.flow_control.as_str()) {
            bail!("flow_control must be none, hardware, or software");
        }
        for p in &self.adapters.prefer {
            if ![
                "cp2102", "cp210x", "tigard", "ftdi", "ch34x", "ch340", "pl2303", "cdc-acm", "usb",
            ]
            .contains(&p.as_str())
            {
                bail!("unknown adapter profile '{p}'");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_config_keeps_defaults() {
        let c: Config =
            toml::from_str("[logging]\nenabled = false\n[adapters]\nprefer = ['tigard', 'cp2102']")
                .unwrap();
        c.validate().unwrap();
        assert!(!c.logging.enabled);
        assert_eq!(c.serial.baud_rates[0], 115200);
    }
    #[test]
    fn reject_bad_settings() {
        assert!(Baud::parse("0").is_err());
        assert!(Baud::parse("banana").is_err());
        let mut c = Config::default();
        c.serial.baud_rates.clear();
        assert!(c.validate().is_err());
        assert!(toml::from_str::<Config>("[logging]\nenabeld = false").is_err());
    }
}
