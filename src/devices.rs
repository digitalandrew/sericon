use crate::config::Adapters;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub port: String,
    pub stable_path: Option<String>,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub serial_number: Option<String>,
    pub product: Option<String>,
    pub interface: Option<u8>,
    pub driver: Option<String>,
}
impl Device {
    pub fn explicit(port: String) -> Self {
        Self {
            port,
            stable_path: None,
            vid: None,
            pid: None,
            serial_number: None,
            product: None,
            interface: None,
            driver: None,
        }
    }
    pub fn matches(&self, profile: &str) -> bool {
        let tigard = self
            .product
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains("tigard");
        // A multifunction adapter's second interface is not a fallback UART.
        if tigard && self.interface != Some(0) {
            return false;
        }
        match profile {
            "cp2102" | "cp210x" => {
                self.vid == Some(0x10c4)
                    && (self.pid == Some(0xea60) || self.driver.as_deref() == Some("cp210x"))
            }
            "tigard" => tigard && self.vid == Some(0x0403) && self.interface == Some(0),
            "ftdi" => {
                !tigard
                    && self.vid == Some(0x0403)
                    && [Some(0x6001), Some(0x6015)].contains(&self.pid)
            }
            "ch340" | "ch34x" => {
                self.vid == Some(0x1a86)
                    && [Some(0x7523), Some(0x5523), Some(0x55d4)].contains(&self.pid)
            }
            "pl2303" => self.vid == Some(0x067b) && self.pid == Some(0x2303),
            "cdc-acm" => self.driver.as_deref() == Some("cdc_acm"),
            "usb" => self.vid.is_some(),
            _ => false,
        }
    }
}
fn read(path: &Path, field: &str) -> Option<String> {
    fs::read_to_string(path.join(field))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
pub fn discover() -> Result<Vec<Device>> {
    discover_at(Path::new("/sys/class/tty"), Path::new("/dev"))
}
fn discover_at(sys: &Path, dev: &Path) -> Result<Vec<Device>> {
    let mut devices = Vec::new();
    for entry in fs::read_dir(sys)? {
        let entry = entry?;
        let name = entry.file_name();
        let path = dev.join(&name);
        if !path.exists() {
            continue;
        }
        let Ok(device_path) = fs::canonicalize(entry.path().join("device")) else {
            continue;
        };
        let mut d = Device::explicit(path.to_string_lossy().into_owned());
        for ancestor in device_path.ancestors() {
            if d.interface.is_none() {
                d.interface = read(ancestor, "bInterfaceNumber")
                    .and_then(|v| u8::from_str_radix(&v, 16).ok());
            }
            if d.driver.is_none() {
                d.driver = fs::read_link(ancestor.join("driver"))
                    .ok()
                    .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));
            }
            if let Some(vid) = read(ancestor, "idVendor") {
                d.vid = u16::from_str_radix(&vid, 16).ok();
                d.pid = read(ancestor, "idProduct").and_then(|s| u16::from_str_radix(&s, 16).ok());
                d.product = read(ancestor, "product");
                d.serial_number = read(ancestor, "serial");
                break;
            }
        }
        if d.vid.is_none() {
            continue;
        }
        if let Ok(links) = fs::read_dir(dev.join("serial/by-id")) {
            let mut matches: Vec<PathBuf> = links
                .flatten()
                .map(|e| e.path())
                .filter(|p| fs::canonicalize(p).ok().as_ref() == Some(&path))
                .collect();
            matches.sort();
            d.stable_path = matches.first().map(|p| p.to_string_lossy().into_owned());
        }
        devices.push(d);
    }
    devices.sort_by_key(|d| {
        (
            d.serial_number.clone(),
            d.stable_path.clone(),
            d.port.clone(),
        )
    });
    Ok(devices)
}
pub fn select(devices: &[Device], prefs: &Adapters) -> Result<Device> {
    for profile in &prefs.prefer {
        if let Some(d) = devices.iter().find(|d| {
            d.matches(profile)
                && prefs
                    .serial_number
                    .as_ref()
                    .is_none_or(|s| d.serial_number.as_ref() == Some(s))
        }) {
            return Ok(d.clone());
        }
    }
    bail!(
        "no matching UART adapter found; run 'sericon devices', connect a preferred adapter, or use --port /dev/ttyUSB0"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_and_tigard_interface() {
        let mut cp = Device::explicit("/dev/ttyUSB9".into());
        cp.vid = Some(0x10c4);
        cp.pid = Some(0xea60);
        let mut t = Device::explicit("/dev/ttyUSB0".into());
        t.vid = Some(0x0403);
        t.product = Some("Tigard V1.1".into());
        t.interface = Some(0);
        let mut j = t.clone();
        j.interface = Some(1);
        let devs = vec![j.clone(), t.clone(), cp.clone()];
        assert_eq!(select(&devs, &Adapters::default()).unwrap().port, cp.port);
        let p = Adapters {
            prefer: vec!["tigard".into(), "cp2102".into()],
            serial_number: None,
        };
        assert_eq!(select(&devs, &p).unwrap().port, t.port);
        assert!(!j.matches("usb"));
    }

    #[test]
    fn discovers_usb_identity_from_sysfs_and_filters_serial() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let usb = root.join("usb/1-2");
        let interface = usb.join("1-2:1.0");
        let tty = interface.join("ttyUSB4");
        let sys = root.join("sys/class/tty");
        let dev = root.join("dev");
        fs::create_dir_all(&tty).unwrap();
        fs::create_dir_all(sys.join("ttyUSB4")).unwrap();
        fs::create_dir_all(dev.join("serial/by-id")).unwrap();
        for (name, value) in [
            ("idVendor", "10c4"),
            ("idProduct", "ea60"),
            ("serial", "fixture-42"),
            ("product", "CP2102"),
        ] {
            fs::write(usb.join(name), value).unwrap();
        }
        fs::write(interface.join("bInterfaceNumber"), "00").unwrap();
        symlink("/sys/bus/usb-serial/drivers/cp210x", tty.join("driver")).unwrap();
        symlink(&tty, sys.join("ttyUSB4/device")).unwrap();
        fs::write(dev.join("ttyUSB4"), "").unwrap();
        symlink(dev.join("ttyUSB4"), dev.join("serial/by-id/fixture")).unwrap();
        let found = discover_at(&sys, &dev).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].interface, Some(0));
        assert_eq!(found[0].driver.as_deref(), Some("cp210x"));
        assert!(
            found[0]
                .stable_path
                .as_ref()
                .unwrap()
                .ends_with("by-id/fixture")
        );
        let mut prefs = Adapters {
            serial_number: Some("wrong".into()),
            ..Adapters::default()
        };
        assert!(select(&found, &prefs).is_err());
        prefs.serial_number = Some("fixture-42".into());
        assert!(select(&found, &prefs).is_ok());
    }
}
