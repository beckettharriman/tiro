//! AC/battery detection, ported from the original's `on_ac_power`
//! (GetSystemPowerStatus). Windows keeps that exact call; Linux reads
//! /sys/class/power_supply. Unknown always means battery — the battery-safe
//! default (POWER-1): guessing "plugged" would let the GPU path pin the
//! discrete GPU on battery.

#[cfg(target_os = "linux")]
use std::path::Path;

/// Any Mains-type supply reporting online == 1 means AC power. No supplies
/// at all (VMs, some desktops) reads as battery — the safe default.
#[cfg(target_os = "linux")]
fn any_mains_online(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim() == "Mains"
            && std::fs::read_to_string(path.join("online"))
                .unwrap_or_default()
                .trim()
                == "1"
        {
            return true;
        }
    }
    false
}

/// True if plugged in. Unknown/battery -> false.
#[cfg(target_os = "linux")]
pub fn on_ac_power() -> bool {
    any_mains_online(Path::new("/sys/class/power_supply"))
}

/// True if plugged in. Unknown/battery -> false.
/// ACLineStatus: 0 = battery, 1 = AC, 255 = unknown.
#[cfg(target_os = "windows")]
pub fn on_ac_power() -> bool {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return false;
    }
    status.ACLineStatus == 1
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn supply(dir: &TempDir, name: &str, kind: &str, online: Option<&str>) {
        let p = dir.path().join(name);
        fs::create_dir(&p).unwrap();
        fs::write(p.join("type"), format!("{kind}\n")).unwrap();
        if let Some(v) = online {
            fs::write(p.join("online"), format!("{v}\n")).unwrap();
        }
    }

    #[test]
    fn mains_online_is_ac() {
        let dir = TempDir::new().unwrap();
        supply(&dir, "AC1", "Mains", Some("1"));
        supply(&dir, "BAT1", "Battery", None);
        assert!(any_mains_online(dir.path()));
    }

    #[test]
    fn mains_offline_is_battery() {
        let dir = TempDir::new().unwrap();
        supply(&dir, "AC1", "Mains", Some("0"));
        supply(&dir, "BAT1", "Battery", None);
        assert!(!any_mains_online(dir.path()));
    }

    #[test]
    fn no_supplies_defaults_to_battery() {
        let dir = TempDir::new().unwrap();
        assert!(!any_mains_online(dir.path()));
        assert!(
            !any_mains_online(&dir.path().join("missing")),
            "unreadable dir is battery too"
        );
    }

    #[test]
    fn battery_only_machine_reads_battery() {
        let dir = TempDir::new().unwrap();
        supply(&dir, "BAT0", "Battery", None);
        assert!(!any_mains_online(dir.path()));
    }
}
