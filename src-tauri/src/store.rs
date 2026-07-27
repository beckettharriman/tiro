//! Transcript store, ported from the original's vault logging: one JSONL and
//! one Markdown file per day in `vault_dir`, falling back to the app-local
//! `fallback_dir` when the vault is unwritable. File names, JSON field
//! order, and the Markdown layout match the original byte for byte so
//! existing vaults carry over seamlessly.

use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Local, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::ConfigStore;

/// One JSONL record — field order matters: it mirrors the original's dict
/// order so new lines are indistinguishable from old ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rec {
    pub ts: String,
    pub text: String,
    pub clean: String,
    pub mic: String,
    pub model: String,
    pub device: String,
    pub secs: f64,
}

/// `log_dir`: probe the vault with a real write, fall back to
/// `fallback_dir` otherwise. Returns (dir, is_vault).
pub fn log_dir(cfg: &ConfigStore) -> Result<(PathBuf, bool), String> {
    let vault = PathBuf::from(cfg.get("vault_dir"));
    let probe = || -> std::io::Result<()> {
        fs::create_dir_all(&vault)?;
        let test = vault.join(".write_test");
        fs::write(&test, "")?;
        fs::remove_file(&test)?;
        Ok(())
    };
    if probe().is_ok() {
        return Ok((vault, true));
    }
    let fallback = PathBuf::from(cfg.get("fallback_dir"));
    fs::create_dir_all(&fallback).map_err(|e| e.to_string())?;
    Ok((fallback, false))
}

fn markdown_header(day: &str) -> String {
    format!(
        "# Tiro — {day}\n\nVerbatim dictation capture (raw layer; the refine-transcript \
         pipeline cleans this later). One line per utterance.\n\n"
    )
}

fn append(path: &PathBuf, content: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new().append(true).create(true).open(path)?;
    f.write_all(content.as_bytes())
}

/// `write_log`: append the take to today's JSONL and Markdown files. File
/// write failures are logged and swallowed (the clipboard already has the
/// text); the record is returned either way for the panel push.
pub fn write_log(
    cfg: &ConfigStore,
    verbatim: &str,
    clean: &str,
    mic: &str,
    model: &str,
    device: &str,
    secs: f64,
) -> Result<(bool, Rec), String> {
    write_log_at(cfg, verbatim, clean, mic, model, device, secs, Local::now())
}

#[allow(clippy::too_many_arguments)]
fn write_log_at(
    cfg: &ConfigStore,
    verbatim: &str,
    clean: &str,
    mic: &str,
    model: &str,
    device: &str,
    secs: f64,
    now: DateTime<Local>,
) -> Result<(bool, Rec), String> {
    let (base, is_vault) = log_dir(cfg)?;
    let day = now.format("%Y-%m-%d").to_string();
    let rec = Rec {
        ts: now.format("%Y-%m-%dT%H:%M:%S").to_string(),
        text: verbatim.to_string(),
        clean: clean.to_string(),
        mic: mic.to_string(),
        model: model.to_string(),
        device: device.to_string(),
        secs: (secs * 100.0).round() / 100.0,
    };
    let result = (|| -> std::io::Result<()> {
        let mut line = serde_json::to_string(&rec).map_err(std::io::Error::other)?;
        line.push('\n');
        append(&base.join(format!("{day}.jsonl")), &line)?;
        let md_path = base.join(format!("{day}.md"));
        let fresh = !md_path.exists();
        let mut md = String::new();
        if fresh {
            md.push_str(&markdown_header(&day));
        }
        md.push_str(&format!("- **{}** — {verbatim}\n", now.format("%H:%M")));
        append(&md_path, &md)
    })();
    if let Err(err) = result {
        eprintln!("log write failed: {err}");
    }
    Ok((is_vault, rec))
}

/// `read_today_entries`: today's records, newest first, capped at `limit`
/// (the panel shows up to 200). Unparseable lines are skipped.
pub fn read_today_entries(cfg: &ConfigStore, limit: usize) -> Vec<Rec> {
    let Ok((base, _)) = log_dir(cfg) else {
        return Vec::new();
    };
    let path = base.join(format!("{}.jsonl", Local::now().format("%Y-%m-%d")));
    let Ok(raw) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out: Vec<Rec> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if out.len() > limit {
        out.drain(..out.len() - limit);
    }
    out.reverse();
    out
}

/// An ISO `YYYY-MM-DDTHH:MM:SS` timestamp -> 12-hour clock like `3:42 PM`.
fn fmt_clock(ts: &str) -> String {
    let dt = NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S")
        .unwrap_or_else(|_| Local::now().naive_local());
    let h = match dt.hour() % 12 {
        0 => 12,
        h => h,
    };
    let half = if dt.hour() < 12 { "AM" } else { "PM" };
    format!("{}:{:02} {}", h, dt.minute(), half)
}

/// Seconds -> `M:SS` (e.g. 8 -> `0:08`). Ties round to even like Python.
fn fmt_dur(secs: f64) -> String {
    let s = secs.round_ties_even().max(0.0) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `entry_from_rec`: the JS-bridge entry shape for one record.
pub fn entry_from_rec(rec: &Rec, cfg: &ConfigStore) -> Value {
    let shown = if rec.clean.is_empty() {
        &rec.text
    } else {
        &rec.clean
    };
    // The original used Python's per-process hash() % 100000 here; any
    // deterministic small hash serves the same purpose (a stable DOM id).
    let mut hasher = std::hash::DefaultHasher::new();
    shown.hash(&mut hasher);
    let id_hash = hasher.finish() % 100_000;
    let device = if matches!(rec.device.to_lowercase().as_str(), "cuda" | "gpu") {
        "GPU"
    } else {
        "CPU"
    };
    let model = if rec.model.is_empty() {
        cfg.get("model")
    } else {
        rec.model.clone()
    };
    let mic = if rec.mic.is_empty() {
        cfg.get("mic_name")
    } else {
        rec.mic.clone()
    };
    json!({
        "id": format!("{}|{}", rec.ts, id_hash),
        "clock": fmt_clock(&rec.ts),
        "dur": fmt_dur(rec.secs),
        "text": shown,
        "device": device,
        "model": model,
        "mic": mic,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn cfg_in(dir: &TempDir) -> ConfigStore {
        let mut cfg = ConfigStore::load(dir.path().join("config.ini"), dir.path());
        cfg.set("vault_dir", dir.path().join("vault").to_str().unwrap());
        cfg.set("fallback_dir", dir.path().join("logs").to_str().unwrap());
        cfg
    }

    fn at(h: u32, m: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 7, 27, h, m, s).unwrap()
    }

    #[test]
    fn writes_jsonl_line_in_original_format() {
        let dir = TempDir::new().unwrap();
        let cfg = cfg_in(&dir);
        let (is_vault, rec) = write_log_at(
            &cfg,
            "hello there",
            "Hello there.",
            "Mic A",
            "base.en",
            "cpu",
            8.234,
            at(15, 42, 7),
        )
        .unwrap();
        assert!(is_vault);
        assert_eq!(rec.secs, 8.23);
        let raw = fs::read_to_string(dir.path().join("vault").join("2026-07-27.jsonl")).unwrap();
        assert_eq!(
            raw,
            "{\"ts\":\"2026-07-27T15:42:07\",\"text\":\"hello there\",\"clean\":\"Hello there.\",\
             \"mic\":\"Mic A\",\"model\":\"base.en\",\"device\":\"cpu\",\"secs\":8.23}\n"
        );
    }

    #[test]
    fn markdown_gets_header_once_then_appends() {
        let dir = TempDir::new().unwrap();
        let cfg = cfg_in(&dir);
        write_log_at(
            &cfg,
            "first take",
            "First take.",
            "",
            "base.en",
            "cpu",
            2.0,
            at(9, 5, 0),
        )
        .unwrap();
        write_log_at(
            &cfg,
            "second take",
            "Second take.",
            "",
            "base.en",
            "cpu",
            3.0,
            at(9, 6, 0),
        )
        .unwrap();
        let md = fs::read_to_string(dir.path().join("vault").join("2026-07-27.md")).unwrap();
        assert_eq!(
            md,
            "# Tiro — 2026-07-27\n\nVerbatim dictation capture (raw layer; the \
             refine-transcript pipeline cleans this later). One line per utterance.\n\n\
             - **09:05** — first take\n- **09:06** — second take\n"
        );
    }

    #[test]
    fn today_entries_are_newest_first_and_capped() {
        let dir = TempDir::new().unwrap();
        let cfg = cfg_in(&dir);
        for i in 0..205 {
            write_log_at(
                &cfg,
                &format!("take {i}"),
                "",
                "",
                "m",
                "cpu",
                1.0,
                at(10, 0, 0),
            )
            .unwrap();
        }
        let entries = read_today_entries(&cfg, 200);
        assert_eq!(entries.len(), 200);
        assert_eq!(entries[0].text, "take 204", "newest first");
        assert_eq!(entries[199].text, "take 5", "oldest 5 dropped");
    }

    #[test]
    fn corrupt_jsonl_lines_are_skipped() {
        let dir = TempDir::new().unwrap();
        let cfg = cfg_in(&dir);
        write_log_at(&cfg, "good", "", "", "m", "cpu", 1.0, at(10, 0, 0)).unwrap();
        let path = dir.path().join("vault").join("2026-07-27.jsonl");
        let mut raw = fs::read_to_string(&path).unwrap();
        raw.push_str("not json at all\n");
        fs::write(&path, raw).unwrap();
        write_log_at(&cfg, "after", "", "", "m", "cpu", 1.0, at(10, 1, 0)).unwrap();
        let entries = read_today_entries(&cfg, 200);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "after");
    }

    #[cfg(unix)]
    #[test]
    fn unwritable_vault_falls_back() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let cfg = cfg_in(&dir);
        let vault = dir.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        fs::set_permissions(&vault, fs::Permissions::from_mode(0o555)).unwrap();
        let (is_vault, _) =
            write_log_at(&cfg, "fallback take", "", "", "m", "cpu", 1.0, at(11, 0, 0)).unwrap();
        assert!(!is_vault, "vault unwritable -> fallback");
        assert!(dir.path().join("logs").join("2026-07-27.jsonl").exists());
        fs::set_permissions(&vault, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn entry_shape_matches_bridge_contract() {
        let dir = TempDir::new().unwrap();
        let cfg = cfg_in(&dir);
        let rec = Rec {
            ts: "2026-07-27T15:42:07".into(),
            text: "hello there".into(),
            clean: "Hello there.".into(),
            mic: "Mic A".into(),
            model: "small.en".into(),
            device: "cuda".into(),
            secs: 8.0,
        };
        let e = entry_from_rec(&rec, &cfg);
        assert!(e["id"]
            .as_str()
            .unwrap()
            .starts_with("2026-07-27T15:42:07|"));
        assert_eq!(e["clock"], "3:42 PM");
        assert_eq!(e["dur"], "0:08");
        assert_eq!(e["text"], "Hello there.", "clean preferred over text");
        assert_eq!(e["device"], "GPU", "legacy cuda maps to GPU");
        assert_eq!(e["model"], "small.en");
        assert_eq!(e["mic"], "Mic A");
    }

    #[test]
    fn entry_falls_back_to_text_and_config() {
        let dir = TempDir::new().unwrap();
        let cfg = cfg_in(&dir);
        let rec = Rec {
            ts: "2026-07-27T00:05:00".into(),
            text: "raw words".into(),
            clean: String::new(),
            mic: String::new(),
            model: String::new(),
            device: "gpu".into(),
            secs: 130.0,
        };
        let e = entry_from_rec(&rec, &cfg);
        assert_eq!(e["text"], "raw words");
        assert_eq!(e["clock"], "12:05 AM", "midnight hour renders as 12");
        assert_eq!(e["dur"], "2:10");
        assert_eq!(e["device"], "GPU");
        assert_eq!(e["model"], "base.en", "falls back to config model");
    }

    #[test]
    fn clock_covers_noon_and_pm() {
        assert_eq!(fmt_clock("2026-07-27T12:00:00"), "12:00 PM");
        assert_eq!(fmt_clock("2026-07-27T23:59:00"), "11:59 PM");
        assert_eq!(fmt_clock("2026-07-27T01:09:00"), "1:09 AM");
    }

    #[test]
    fn dur_rounds_ties_to_even_like_python() {
        assert_eq!(fmt_dur(8.5), "0:08");
        assert_eq!(fmt_dur(9.5), "0:10");
        assert_eq!(fmt_dur(-3.0), "0:00");
        assert_eq!(fmt_dur(59.6), "1:00");
    }
}
