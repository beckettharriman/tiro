//! `config.ini` handling, ported from the original's `load_config` /
//! `save_config` / `gget` / `gbool` / `gset`.
//!
//! Behavioral contract (PORTING_NOTES §2):
//! - every key lives in `[general]`; keys and defaults are identical to the
//!   original so an existing `config.ini` carries over unchanged
//! - a corrupt/unreadable file never crashes startup: it is quarantined as
//!   `config.ini.bad-<stamp>` and rebuilt from defaults
//! - missing keys are backfilled from defaults; the file is only rewritten
//!   when something actually changed (no churn in a read-only dir)
//! - saves are atomic: sibling `.tmp` + fsync, single `.bak` of the prior
//!   file, then rename over; an unwritable dir degrades to in-memory config
//!
//! Escape/quote handling is disabled in both directions: Python's
//! `configparser` reads and writes values literally (think Windows paths like
//! `C:\Users\...`), so escaping here would corrupt a carried-over file.

use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use ini::{EscapePolicy, Ini, LineSeparator, ParseOption, WriteOption};

pub const SECTION: &str = "general";

const PARSE_OPTION: ParseOption = ParseOption {
    enabled_escape: false,
    enabled_quote: false,
    enabled_indented_mutiline_value: false,
    enabled_preserve_key_leading_whitespace: false,
};

fn write_option() -> WriteOption {
    WriteOption {
        escape_policy: EscapePolicy::Nothing,
        line_separator: LineSeparator::SystemDefault,
        kv_separator: " = ",
    }
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The original's `DEFAULTS["general"]`, in the same order.
pub fn defaults(app_dir: &Path) -> Vec<(&'static str, String)> {
    let vault = home_dir().join("Documents").join("Tiro");
    let fallback = app_dir.join("logs");
    vec![
        ("dictation_hotkey", "ctrl+alt+space".into()),
        ("paste_hotkey", "ctrl+alt+v".into()),
        ("panel_hotkey", "ctrl+alt+c".into()),
        ("model", "base.en".into()),
        ("model_battery", "base.en".into()),
        ("model_ac", "small.en".into()),
        ("device", "auto".into()),
        ("compute_type", "int8".into()),
        // Ignore battery state — a battery machine the user runs docked
        // can opt into desktop policy (single model, no AC/battery split).
        ("treat_as_desktop", "false".into()),
        // Multi-GPU machines: the chosen Vulkan device, stored as index +
        // name so a hardware change invalidates sanely (a stale name falls
        // back to the default device). Empty = auto (prefer discrete,
        // then largest VRAM).
        ("gpu_device_index", String::new()),
        ("gpu_device_name", String::new()),
        ("cancel_hotkey", "ctrl+alt+x".into()),
        ("mic_name", String::new()),
        ("beeps", "true".into()),
        ("sound_volume", "1.0".into()),
        ("pill", "true".into()),
        // Pill dock edge ("top" | "bottom") and its distance in px from that
        // work-area edge. bottom/110 is the pre-setting fixed spot, so an
        // untouched config places the pill exactly where it always was.
        ("pill_position", "bottom".into()),
        ("pill_padding", "110".into()),
        ("clipboard_cleanup", "light".into()),
        ("use_vocab_bias", "true".into()),
        ("theme", "system".into()),
        ("panel_transparency", "45".into()),
        ("vault_dir", vault.to_string_lossy().into_owned()),
        ("fallback_dir", fallback.to_string_lossy().into_owned()),
        ("save_transcripts", "true".into()),
        ("auto_restart", "true".into()),
        // RemoteDesktop-portal restore token for paste injection on Wayland
        // (set after the user approves the one-time permission dialog).
        ("portal_restore_token", String::new()),
        // Last on-screen panel position as "x,y" (physical px), so the panel
        // reopens where the user left it across restarts. Empty = never
        // placed -> centered on first show.
        ("panel_pos", String::new()),
    ]
}

/// Clone = a point-in-time snapshot: readers that need config values while
/// doing filesystem work clone under the cfg lock, drop the lock, then read
/// from the snapshot — a stale mount probed by store::log_dir must never
/// wedge every other cfg-lock user (see the engine LOCK LAW). Snapshots are
/// for reading; `set` on a clone would persist a fork of the config.
#[derive(Clone)]
pub struct ConfigStore {
    path: PathBuf,
    defaults: Vec<(&'static str, String)>,
    ini: Ini,
}

impl ConfigStore {
    /// Load `config.ini` from `path`, mirroring the original `load_config`:
    /// start from defaults, overlay the file if readable (quarantining a
    /// corrupt one), backfill missing keys, and persist only when changed.
    pub fn load(path: PathBuf, app_dir: &Path) -> Self {
        let defaults = defaults(app_dir);
        let mut changed = false;
        let ini = if path.exists() {
            match Ini::load_from_file_opt(&path, PARSE_OPTION) {
                Ok(mut ini) => {
                    // ONE-TIME migration to the split panel/paste scheme: a
                    // file from before `paste_hotkey` existed with the panel
                    // still on its old default (ctrl+alt+v) moves the panel to
                    // ctrl+alt+c so paste can take V. A deliberately rebound
                    // panel is left alone, and once `paste_hotkey` is present
                    // (backfilled below) this can never fire again.
                    if ini.get_from(Some(SECTION), "paste_hotkey").is_none()
                        && ini.get_from(Some(SECTION), "panel_hotkey") == Some("ctrl+alt+v")
                    {
                        ini.set_to(
                            Some(SECTION),
                            "panel_hotkey".to_string(),
                            "ctrl+alt+c".to_string(),
                        );
                        changed = true;
                    }
                    for (key, value) in &defaults {
                        if ini.get_from(Some(SECTION), key).is_none() {
                            ini.set_to(Some(SECTION), (*key).to_string(), value.clone());
                            changed = true;
                        }
                    }
                    ini
                }
                Err(err) => {
                    // Quarantine the unreadable file and rebuild from defaults
                    // so the app ALWAYS starts.
                    eprintln!("config unreadable, resetting: {err}");
                    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
                    let bad = path.with_extension(format!("ini.bad-{stamp}"));
                    if let Err(err2) = fs::rename(&path, &bad) {
                        eprintln!("could not quarantine bad config: {err2}");
                    }
                    changed = true;
                    Self::ini_from(&defaults)
                }
            }
        } else {
            changed = true; // no file yet — write the defaults out once
            Self::ini_from(&defaults)
        };
        let store = Self {
            path,
            defaults,
            ini,
        };
        if changed {
            store.save();
        }
        store
    }

    fn ini_from(defaults: &[(&'static str, String)]) -> Ini {
        let mut ini = Ini::new();
        for (key, value) in defaults {
            ini.set_to(Some(SECTION), (*key).to_string(), value.clone());
        }
        ini
    }

    /// `gget`: the stored value, else this key's default, else "".
    pub fn get(&self, key: &str) -> String {
        if let Some(value) = self.ini.get_from(Some(SECTION), key) {
            return value.to_string();
        }
        self.defaults
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    /// `gbool`: configparser's boolean forms; anything else is false.
    pub fn get_bool(&self, key: &str) -> bool {
        matches!(
            self.get(key).to_lowercase().as_str(),
            "1" | "yes" | "true" | "on"
        )
    }

    /// `gset`: update one key and persist immediately.
    pub fn set(&mut self, key: &str, value: &str) {
        self.ini
            .set_to(Some(SECTION), key.to_string(), value.to_string());
        self.save();
    }

    /// Atomic persist, mirroring the original `save_config`: sibling temp
    /// file + fsync, single `.bak` of the prior file, then rename over. An
    /// unwritable location degrades to in-memory config instead of failing.
    pub fn save(&self) {
        let tmp = self.path.with_extension("ini.tmp");
        let result = (|| -> std::io::Result<()> {
            let mut file = File::create(&tmp)?;
            let mut buf = Vec::new();
            self.ini
                .write_to_opt(&mut buf, write_option())
                .map_err(std::io::Error::other)?;
            file.write_all(&buf)?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            if self.path.exists() {
                // best-effort single backup of the prior good file
                let _ = fs::rename(&self.path, self.path.with_extension("ini.bak"));
            }
            fs::rename(&tmp, &self.path)?;
            Ok(())
        })();
        if let Err(err) = result {
            eprintln!("config save failed (running with in-memory settings): {err}");
            let _ = fs::remove_file(&tmp);
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn load_in(dir: &TempDir) -> ConfigStore {
        ConfigStore::load(dir.path().join("config.ini"), dir.path())
    }

    #[test]
    fn first_run_writes_defaults() {
        let dir = TempDir::new().unwrap();
        let cfg = load_in(&dir);
        assert!(cfg.path().exists(), "config.ini should be created");
        assert_eq!(cfg.get("dictation_hotkey"), "ctrl+alt+space");
        assert_eq!(cfg.get("paste_hotkey"), "ctrl+alt+v");
        assert_eq!(cfg.get("panel_hotkey"), "ctrl+alt+c");
        assert_eq!(cfg.get("cancel_hotkey"), "ctrl+alt+x");
        assert_eq!(cfg.get("portal_restore_token"), "");
        assert_eq!(cfg.get("panel_pos"), "", "no remembered position yet");
        assert_eq!(cfg.get("device"), "auto");
        assert_eq!(cfg.get("compute_type"), "int8");
        assert_eq!(cfg.get("model_battery"), "base.en");
        assert_eq!(cfg.get("model_ac"), "small.en");
        assert_eq!(cfg.get("model"), "base.en");
        assert_eq!(cfg.get("mic_name"), "");
        assert_eq!(cfg.get("sound_volume"), "1.0");
        assert_eq!(cfg.get("clipboard_cleanup"), "light");
        assert_eq!(cfg.get("theme"), "system");
        assert_eq!(cfg.get("panel_transparency"), "45");
        assert_eq!(cfg.get("pill_position"), "bottom");
        assert_eq!(cfg.get("pill_padding"), "110");
        assert_eq!(
            cfg.get("fallback_dir"),
            dir.path().join("logs").to_string_lossy()
        );
        assert!(cfg
            .get("vault_dir")
            .ends_with(&format!("Documents{}Tiro", std::path::MAIN_SEPARATOR)));
        for key in [
            "beeps",
            "pill",
            "use_vocab_bias",
            "save_transcripts",
            "auto_restart",
        ] {
            assert!(cfg.get_bool(key), "{key} should default to true");
        }
        let raw = fs::read_to_string(cfg.path()).unwrap();
        assert!(
            raw.contains("save_transcripts = true"),
            "fresh config.ini carries save_transcripts: {raw}"
        );
    }

    #[test]
    fn set_round_trips_through_reload() {
        let dir = TempDir::new().unwrap();
        let mut cfg = load_in(&dir);
        cfg.set("theme", "dark");
        cfg.set("mic_name", "USB Microphone");
        cfg.set("save_transcripts", "false");
        drop(cfg);
        let cfg = load_in(&dir);
        assert_eq!(cfg.get("theme"), "dark");
        assert_eq!(cfg.get("mic_name"), "USB Microphone");
        assert!(!cfg.get_bool("save_transcripts"));
        assert_eq!(cfg.get("model_battery"), "base.en", "other keys untouched");
    }

    #[test]
    fn windows_paths_round_trip_unescaped() {
        let dir = TempDir::new().unwrap();
        let mut cfg = load_in(&dir);
        cfg.set("vault_dir", r"C:\Users\becke\Documents\Tiro");
        drop(cfg);
        let cfg = load_in(&dir);
        assert_eq!(cfg.get("vault_dir"), r"C:\Users\becke\Documents\Tiro");
        let raw = fs::read_to_string(cfg.path()).unwrap();
        assert!(
            raw.contains(r"vault_dir = C:\Users\becke\Documents\Tiro"),
            "value must be written literally with configparser-style spacing: {raw}"
        );
    }

    #[test]
    fn partial_file_is_backfilled_preserving_existing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.ini");
        fs::write(&path, "[general]\ntheme = light\n").unwrap();
        let cfg = ConfigStore::load(path, dir.path());
        assert_eq!(cfg.get("theme"), "light", "existing value preserved");
        assert_eq!(cfg.get("model_ac"), "small.en", "missing key backfilled");
        assert_eq!(cfg.get("pill_position"), "bottom", "pill keys backfilled");
        assert_eq!(cfg.get("pill_padding"), "110", "pill keys backfilled");
        let raw = fs::read_to_string(cfg.path()).unwrap();
        assert!(raw.contains("dictation_hotkey"), "backfill was persisted");
    }

    #[test]
    fn unchanged_file_is_not_rewritten() {
        let dir = TempDir::new().unwrap();
        let cfg = load_in(&dir);
        let before = fs::read_to_string(cfg.path()).unwrap();
        let mtime = fs::metadata(cfg.path()).unwrap().modified().unwrap();
        drop(cfg);
        let cfg = load_in(&dir);
        assert_eq!(fs::read_to_string(cfg.path()).unwrap(), before);
        assert_eq!(
            fs::metadata(cfg.path()).unwrap().modified().unwrap(),
            mtime,
            "second load must not churn the file"
        );
    }

    #[test]
    fn corrupt_file_is_quarantined_and_reset() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.ini");
        fs::write(&path, "[unclosed\n%$ not ini at all").unwrap();
        let cfg = ConfigStore::load(path, dir.path());
        assert_eq!(cfg.get("device"), "auto", "reset to defaults");
        let quarantined = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".bad-"));
        assert!(quarantined, "bad file should be kept as .bad-<stamp>");
    }

    #[test]
    fn save_keeps_single_backup_of_prior_file() {
        let dir = TempDir::new().unwrap();
        let mut cfg = load_in(&dir);
        let before = fs::read_to_string(cfg.path()).unwrap();
        cfg.set("theme", "dark");
        let bak = dir.path().join("config.ini.bak");
        assert!(bak.exists(), "prior file kept as .bak");
        assert_eq!(fs::read_to_string(&bak).unwrap(), before);
        assert!(
            !dir.path().join("config.ini.tmp").exists(),
            "tmp cleaned up"
        );
    }

    #[test]
    fn migration_moves_old_default_panel_to_c() {
        // A pre-paste_hotkey file with the panel on its old default gets the
        // new scheme: panel -> ctrl+alt+c, paste backfilled to ctrl+alt+v.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.ini");
        fs::write(&path, "[general]\npanel_hotkey = ctrl+alt+v\n").unwrap();
        let cfg = ConfigStore::load(path, dir.path());
        assert_eq!(cfg.get("panel_hotkey"), "ctrl+alt+c");
        assert_eq!(cfg.get("paste_hotkey"), "ctrl+alt+v");
        let raw = fs::read_to_string(cfg.path()).unwrap();
        assert!(
            raw.contains("panel_hotkey = ctrl+alt+c"),
            "persisted: {raw}"
        );
        assert!(
            raw.contains("paste_hotkey = ctrl+alt+v"),
            "persisted: {raw}"
        );
    }

    #[test]
    fn migration_leaves_custom_panel_binding_alone() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.ini");
        fs::write(&path, "[general]\npanel_hotkey = ctrl+shift+p\n").unwrap();
        let cfg = ConfigStore::load(path, dir.path());
        assert_eq!(cfg.get("panel_hotkey"), "ctrl+shift+p", "custom kept");
        assert_eq!(cfg.get("paste_hotkey"), "ctrl+alt+v", "paste backfilled");
    }

    #[test]
    fn migration_fires_exactly_once() {
        // After the first migration the file has paste_hotkey, so a user who
        // then rebinds paste elsewhere and panel BACK to ctrl+alt+v keeps it.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.ini");
        fs::write(&path, "[general]\npanel_hotkey = ctrl+alt+v\n").unwrap();
        let mut cfg = ConfigStore::load(path.clone(), dir.path());
        assert_eq!(cfg.get("panel_hotkey"), "ctrl+alt+c");
        cfg.set("paste_hotkey", "ctrl+alt+b");
        cfg.set("panel_hotkey", "ctrl+alt+v");
        drop(cfg);
        let cfg = ConfigStore::load(path, dir.path());
        assert_eq!(cfg.get("panel_hotkey"), "ctrl+alt+v", "stable on reload");
        assert_eq!(cfg.get("paste_hotkey"), "ctrl+alt+b");
    }

    #[test]
    fn migrated_file_reloads_stably() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.ini");
        fs::write(&path, "[general]\npanel_hotkey = ctrl+alt+v\n").unwrap();
        drop(ConfigStore::load(path.clone(), dir.path()));
        let before = fs::read_to_string(&path).unwrap();
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        let cfg = ConfigStore::load(path.clone(), dir.path());
        assert_eq!(cfg.get("panel_hotkey"), "ctrl+alt+c");
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            mtime,
            "second load after migration must not rewrite the file"
        );
    }

    #[test]
    fn fresh_file_and_backfill_use_new_defaults() {
        // Backfill of a partial file (no panel_hotkey at all) is NOT the
        // migration path: both keys land on the new defaults.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.ini");
        fs::write(&path, "[general]\ntheme = light\n").unwrap();
        let cfg = ConfigStore::load(path, dir.path());
        assert_eq!(cfg.get("panel_hotkey"), "ctrl+alt+c");
        assert_eq!(cfg.get("paste_hotkey"), "ctrl+alt+v");
    }

    #[test]
    fn panel_pos_round_trips_and_backfills() {
        let dir = TempDir::new().unwrap();
        let mut cfg = load_in(&dir);
        cfg.set("panel_pos", "640,-128");
        drop(cfg);
        let cfg = load_in(&dir);
        assert_eq!(cfg.get("panel_pos"), "640,-128");
        let raw = fs::read_to_string(cfg.path()).unwrap();
        assert!(raw.contains("panel_pos = 640,-128"), "persisted: {raw}");
        // a pre-panel_pos file gets the key backfilled empty
        let path = dir.path().join("old.ini");
        fs::write(&path, "[general]\ntheme = light\n").unwrap();
        let cfg = ConfigStore::load(path, dir.path());
        assert_eq!(cfg.get("panel_pos"), "", "backfilled to unset");
    }

    #[test]
    fn boolean_forms_match_configparser() {
        let dir = TempDir::new().unwrap();
        let mut cfg = load_in(&dir);
        for (raw, expect) in [
            ("1", true),
            ("yes", true),
            ("True", true),
            ("ON", true),
            ("0", false),
            ("no", false),
            ("False", false),
            ("off", false),
            ("garbage", false),
        ] {
            cfg.set("beeps", raw);
            assert_eq!(cfg.get_bool("beeps"), expect, "beeps = {raw}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unwritable_dir_degrades_to_in_memory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let mut cfg = load_in(&dir);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        cfg.set("theme", "dark");
        assert_eq!(cfg.get("theme"), "dark", "in-memory value survives");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        drop(cfg);
        let cfg = load_in(&dir);
        assert_eq!(cfg.get("theme"), "system", "failed save left file as-is");
    }
}
