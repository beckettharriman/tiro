//! Clipboard copy + the clipboard-only text cleanup, ported from the
//! original's `cleanup_text` and pyperclip usage. Cleanup only ever affects
//! the copied text — the verbatim transcript is always logged unchanged.

use std::sync::{Mutex, OnceLock};

use regex::Regex;

/// `cleanup_text`: collapse whitespace, then for any mode except `none`
/// apply the light rules (strip space before punctuation, standalone `i` ->
/// `I` including i'm/i'll/i've, capitalize the first letter, append `.` when
/// no terminal punctuation); `fillers` first strips um/uh/erm/hmm.
pub fn cleanup_text(text: &str, mode: &str) -> String {
    static FILLER: OnceLock<Regex> = OnceLock::new();
    static SPACE_PUNCT: OnceLock<Regex> = OnceLock::new();
    static STANDALONE_I: OnceLock<Regex> = OnceLock::new();

    let collapse = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut t = collapse(text);
    if t.is_empty() || mode == "none" {
        return t;
    }
    if mode == "fillers" {
        let filler = FILLER
            .get_or_init(|| Regex::new(r"(?i)\b(um+|uh+|erm+|hmm+)\b,?\s*").expect("filler regex"));
        t = collapse(&filler.replace_all(&t, ""));
    }
    let space_punct =
        SPACE_PUNCT.get_or_init(|| Regex::new(r"\s+([,.!?;:])").expect("punct regex"));
    t = space_punct.replace_all(&t, "$1").into_owned();
    let standalone_i = STANDALONE_I.get_or_init(|| Regex::new(r"\bi\b").expect("i regex"));
    t = standalone_i.replace_all(&t, "I").into_owned();
    let mut chars = t.chars();
    if let Some(first) = chars.next() {
        t = first.to_uppercase().collect::<String>() + chars.as_str();
        if !matches!(t.chars().last(), Some('.' | '!' | '?')) {
            t.push('.');
        }
    }
    t
}

/// What actually goes to the clipboard: the cleaned text, falling back to
/// the verbatim take when cleanup yields an empty string (STATE-3 — an
/// all-fillers take must never be lost or wipe the clipboard).
pub fn clipboard_text(verbatim: &str, mode: &str) -> String {
    let clean = cleanup_text(verbatim, mode);
    if clean.is_empty() {
        verbatim.to_string()
    } else {
        clean
    }
}

/// Copy `text` to the system clipboard. The Clipboard handle is kept alive
/// for the process lifetime — on X11 the selection would otherwise vanish
/// as soon as the handle drops. A failed set tears the handle down so the
/// next attempt starts fresh.
pub fn copy(text: &str) -> Result<(), String> {
    static CLIP: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    let holder = CLIP.get_or_init(|| Mutex::new(None));
    let mut guard = holder.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
    }
    if let Some(clipboard) = guard.as_mut() {
        if let Err(err) = clipboard.set_text(text.to_string()) {
            *guard = None;
            return Err(err.to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_and_trims_whitespace() {
        assert_eq!(cleanup_text("  hello   world  ", "light"), "Hello world.");
        assert_eq!(
            cleanup_text("hello\tthere\nfriend", "light"),
            "Hello there friend."
        );
    }

    #[test]
    fn none_mode_returns_collapsed_verbatim() {
        assert_eq!(cleanup_text("  hello   world  ", "none"), "hello world");
        assert_eq!(cleanup_text("no period added", "none"), "no period added");
    }

    #[test]
    fn capitalizes_first_letter_unless_already_upper() {
        assert_eq!(cleanup_text("hello", "light"), "Hello.");
        assert_eq!(cleanup_text("Hello", "light"), "Hello.");
        assert_eq!(cleanup_text("123 go", "light"), "123 go.");
    }

    #[test]
    fn appends_period_only_without_terminal_punctuation() {
        assert_eq!(cleanup_text("done", "light"), "Done.");
        assert_eq!(cleanup_text("done.", "light"), "Done.");
        assert_eq!(cleanup_text("really?", "light"), "Really?");
        assert_eq!(cleanup_text("wow!", "light"), "Wow!");
        assert_eq!(cleanup_text("wait;", "light"), "Wait;.");
    }

    #[test]
    fn standalone_i_is_capitalized() {
        assert_eq!(
            cleanup_text("i think i'm sure i'll go and i've been", "light"),
            "I think I'm sure I'll go and I've been."
        );
        assert_eq!(
            cleanup_text("the wifi is fine", "light"),
            "The wifi is fine."
        );
    }

    #[test]
    fn fixes_space_before_punctuation() {
        assert_eq!(cleanup_text("hello , world .", "light"), "Hello, world.");
        assert_eq!(cleanup_text("wait ; no : yes !", "light"), "Wait; no: yes!");
    }

    #[test]
    fn fillers_are_stripped_in_fillers_mode_only() {
        assert_eq!(
            cleanup_text("um, so uh this is hmm good", "fillers"),
            "So this is good."
        );
        assert_eq!(
            cleanup_text("ummm well uhhh fine erm ok", "fillers"),
            "Well fine ok."
        );
        assert_eq!(
            cleanup_text("um, so this stays", "light"),
            "Um, so this stays."
        );
        assert_eq!(
            cleanup_text("the column is umbrella-shaped", "fillers"),
            "The column is umbrella-shaped."
        );
    }

    #[test]
    fn unknown_mode_gets_light_rules() {
        assert_eq!(cleanup_text("hello there", "banana"), "Hello there.");
    }

    #[test]
    fn all_fillers_cleanup_is_empty_and_falls_back_to_verbatim() {
        assert_eq!(cleanup_text("um uh hmm", "fillers"), "");
        assert_eq!(clipboard_text("um uh hmm", "fillers"), "um uh hmm");
        assert_eq!(clipboard_text("real words", "fillers"), "Real words.");
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(cleanup_text("", "light"), "");
        assert_eq!(cleanup_text("   ", "fillers"), "");
    }
}
