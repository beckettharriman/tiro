//! Battery presence (hardware class, never charge state). Windows asks
//! GetSystemPowerStatus; Linux reads /sys/class/power_supply. The engine
//! no longer switches on the power source; this only feeds the hardware
//! line in the log and the hardware snapshot the panel receives.

#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "macos")]
pub use crate::macos::battery_present;

/// Any Battery-type supply present under the sysfs root — PRESENCE, not
/// charge state. Device batteries (a bluetooth mouse's `type = Battery`
/// with `scope = Device`) must not make a desktop read as a laptop, so a
/// supply with a non-System scope is ignored; a missing `scope` file means
/// a system battery (older kernels).
#[cfg(target_os = "linux")]
fn any_system_battery(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        let scope = std::fs::read_to_string(path.join("scope")).unwrap_or_default();
        let scope = scope.trim();
        if scope.is_empty() || scope == "System" {
            return true;
        }
    }
    false
}

/// Whether the machine HAS a battery (hardware class, not charge state).
/// No battery -> the engine policy treats the machine as a desktop.
#[cfg(target_os = "linux")]
pub fn battery_present() -> bool {
    any_system_battery(Path::new("/sys/class/power_supply"))
}

/// Whether the machine HAS a battery (hardware class, not charge state).
/// BatteryFlag bit 128 = "no system battery"; 255 = unknown -> read as no
/// battery so a desktop with a confused BIOS doesn't grow battery UI.
#[cfg(target_os = "windows")]
pub fn battery_present() -> bool {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return false;
    }
    status.BatteryFlag != 255 && status.BatteryFlag & 128 == 0
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
    fn battery_presence_is_detected() {
        let dir = TempDir::new().unwrap();
        supply(&dir, "AC1", "Mains", Some("1"));
        supply(&dir, "BAT0", "Battery", None);
        assert!(any_system_battery(dir.path()));
    }

    #[test]
    fn no_battery_reads_as_desktop_hardware() {
        let dir = TempDir::new().unwrap();
        supply(&dir, "AC1", "Mains", Some("1"));
        assert!(!any_system_battery(dir.path()));
        assert!(!any_system_battery(&dir.path().join("missing")));
    }

    #[test]
    fn device_scoped_batteries_do_not_count() {
        // a bluetooth mouse battery must not turn a desktop into a laptop
        let dir = TempDir::new().unwrap();
        supply(&dir, "hid-aa.bb-battery", "Battery", None);
        fs::write(
            dir.path().join("hid-aa.bb-battery").join("scope"),
            "Device\n",
        )
        .unwrap();
        assert!(!any_system_battery(dir.path()));
        // but an explicit System scope counts
        supply(&dir, "BAT0", "Battery", None);
        fs::write(dir.path().join("BAT0").join("scope"), "System\n").unwrap();
        assert!(any_system_battery(dir.path()));
    }
}
