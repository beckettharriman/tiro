//! The Vocabulary editor's storage: hot words live in `vocab.txt` (the same
//! file the transcription bias prompt reads — one word/phrase per line,
//! `#` comments preserved), corrections in `corrections.txt` (one
//! `heard => written` per line). Both sit next to config.ini in the app
//! dir. Corrections are applied as post-replacements to the CLIPBOARD text
//! only — the verbatim transcript is always logged unchanged.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

const CORRECTIONS_FILE: &str = "corrections.txt";
const VOCAB_FILE: &str = "vocab.txt";

/// Hot words from `vocab.txt`: non-blank, non-comment lines, in file order.
pub fn read_hotwords(app_dir: &Path) -> Vec<String> {
    let raw = fs::read_to_string(app_dir.join(VOCAB_FILE)).unwrap_or_default();
    raw.lines()
        .map(str::trim)
        .filter(|ln| !ln.is_empty() && !ln.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Corrections from `corrections.txt`: `heard => written` pairs, in file
/// order. Lines without the arrow (or with an empty side) are skipped.
pub fn read_corrections(app_dir: &Path) -> Vec<(String, String)> {
    let raw = fs::read_to_string(app_dir.join(CORRECTIONS_FILE)).unwrap_or_default();
    raw.lines()
        .filter_map(|ln| {
            let ln = ln.trim();
            if ln.is_empty() || ln.starts_with('#') {
                return None;
            }
            let (heard, written) = ln.split_once("=>")?;
            let (heard, written) = (heard.trim(), written.trim());
            if heard.is_empty() || written.is_empty() {
                return None;
            }
            Some((heard.to_string(), written.to_string()))
        })
        .collect()
}

/// Rewrite `vocab.txt` with the given hot words, keeping any leading `#`
/// comment block (the file doubles as user-editable documentation).
fn write_hotwords(app_dir: &Path, words: &[String]) -> std::io::Result<()> {
    let path = app_dir.join(VOCAB_FILE);
    let old = fs::read_to_string(&path).unwrap_or_default();
    let mut out = String::new();
    for ln in old.lines() {
        let t = ln.trim();
        if t.starts_with('#') || t.is_empty() {
            out.push_str(ln);
            out.push('\n');
        } else {
            break; // first word line — the rest is regenerated below
        }
    }
    for w in words {
        out.push_str(w);
        out.push('\n');
    }
    fs::write(path, out)
}

fn write_corrections(app_dir: &Path, pairs: &[(String, String)]) -> std::io::Result<()> {
    let mut out = String::new();
    for (heard, written) in pairs {
        out.push_str(&format!("{heard} => {written}\n"));
    }
    fs::write(app_dir.join(CORRECTIONS_FILE), out)
}

/// `list_vocab`: the editor's snapshot.
pub fn list(app_dir: &Path) -> Value {
    let corrections: Vec<Value> = read_corrections(app_dir)
        .into_iter()
        .map(|(h, w)| json!([h, w]))
        .collect();
    json!({ "hotwords": read_hotwords(app_dir), "corrections": corrections })
}

/// A bridge string with the newline/arrow characters that would corrupt the
/// line-based files stripped out.
fn clean_term(v: &Value) -> Option<String> {
    let s = v.as_str()?.replace("=>", " ");
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `set_vocab`: persist both lists (the editor sends full snapshots — add
/// and remove are the same write).
pub fn set(app_dir: &Path, hotwords: &Value, corrections: &Value) -> Value {
    let words: Vec<String> = hotwords
        .as_array()
        .map(|a| a.iter().filter_map(clean_term).collect())
        .unwrap_or_default();
    let pairs: Vec<(String, String)> = corrections
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let arr = p.as_array()?;
                    Some((clean_term(arr.first()?)?, clean_term(arr.get(1)?)?))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut ok = true;
    if let Err(e) = write_hotwords(app_dir, &words) {
        eprintln!("vocab.txt write failed: {e}");
        ok = false;
    }
    if let Err(e) = write_corrections(app_dir, &pairs) {
        eprintln!("corrections.txt write failed: {e}");
        ok = false;
    }
    json!({ "ok": ok })
}

/// Apply the `heard => written` corrections to `text` (the clipboard-bound
/// string): case-insensitive whole-phrase matches, replaced in file order.
/// Never returns an empty string for non-empty input — a pathological rule
/// set falls back to the uncorrected text (never lose a take).
pub fn apply_corrections(text: &str, pairs: &[(String, String)]) -> String {
    if pairs.is_empty() || text.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for (heard, written) in pairs {
        let pattern = format!(r"(?i)\b{}\b", regex::escape(heard));
        if let Ok(re) = regex::Regex::new(&pattern) {
            out = re.replace_all(&out, written.as_str()).into_owned();
        }
    }
    if out.trim().is_empty() {
        text.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn hotwords_round_trip_preserving_comments() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(VOCAB_FILE),
            "# my words\n\nTiro\nPipeWire\n",
        )
        .unwrap();
        assert_eq!(read_hotwords(dir.path()), ["Tiro", "PipeWire"]);
        set(
            dir.path(),
            &json!(["Tiro", "Beckett"]),
            &json!([["tyro", "Tiro"]]),
        );
        let raw = fs::read_to_string(dir.path().join(VOCAB_FILE)).unwrap();
        assert_eq!(raw, "# my words\n\nTiro\nBeckett\n", "comment block kept");
        assert_eq!(
            read_corrections(dir.path()),
            [("tyro".into(), "Tiro".into())]
        );
    }

    #[test]
    fn missing_files_read_as_empty() {
        let dir = TempDir::new().unwrap();
        assert!(read_hotwords(dir.path()).is_empty());
        assert!(read_corrections(dir.path()).is_empty());
        let v = list(dir.path());
        assert_eq!(v["hotwords"], json!([]));
        assert_eq!(v["corrections"], json!([]));
    }

    #[test]
    fn corrections_skip_malformed_lines() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(CORRECTIONS_FILE),
            "# fixes\ntyro => Tiro\nno arrow line\n => empty heard\njoy con => Joy-Con\n",
        )
        .unwrap();
        assert_eq!(
            read_corrections(dir.path()),
            [
                ("tyro".into(), "Tiro".into()),
                ("joy con".into(), "Joy-Con".into())
            ]
        );
    }

    #[test]
    fn apply_is_case_insensitive_whole_phrase() {
        let pairs = vec![
            ("tyro".to_string(), "Tiro".to_string()),
            ("med star".to_string(), "MedStar".to_string()),
        ];
        assert_eq!(
            apply_corrections("Tyro and med star, but tyrode stays.", &pairs),
            "Tiro and MedStar, but tyrode stays."
        );
        assert_eq!(apply_corrections("", &pairs), "");
        assert_eq!(apply_corrections("nothing to do", &[]), "nothing to do");
    }

    #[test]
    fn apply_never_empties_a_take() {
        let pairs = vec![("um".to_string(), " ".to_string())];
        assert_eq!(apply_corrections("um", &pairs), "um", "fallback kept");
    }

    #[test]
    fn set_scrubs_arrows_and_newlines() {
        let dir = TempDir::new().unwrap();
        set(
            dir.path(),
            &json!(["a => b\nc"]),
            &json!([["he\nard", "writ => ten"]]),
        );
        assert_eq!(read_hotwords(dir.path()), ["a b c"]);
        assert_eq!(
            read_corrections(dir.path()),
            [("he ard".into(), "writ ten".into())]
        );
    }
}
