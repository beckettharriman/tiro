//! Give the process a portal-visible app id by putting it in an
//! `app-<id>-<pid>.scope` systemd user scope.
//!
//! xdg-desktop-portal resolves a non-sandboxed caller's app id ITSELF — the
//! request carries no client-settable identity. Verified against the
//! installed portal (1.22.1, `src/xdp-app-info-host.c`): it asks systemd for
//! the caller's user unit (`sd_pid_get_user_unit`), requires an `app-…` name
//! in the systemd desktop-environment format, extracts the app id from the
//! unit name, and then requires `<app id>.desktop` to exist in the XDG
//! applications dirs (`g_desktop_app_info_new`). Any miss leaves the app id
//! empty, and `GlobalShortcuts.CreateSession` then refuses with NotAllowed
//! ("An app id is required") — so a terminal/script launch, which sits in
//! the terminal's scope, could never engage the portal hotkey path.
//!
//! Fix: before the portal worker first touches D-Bus, if the current unit
//! does not already resolve to an app id, migrate this process into a fresh
//! transient scope named `app-<identifier>-<pid>.scope` via systemd's
//! `StartTransientUnit` — the D-Bus equivalent of `systemd-run --user
//! --scope`, but moving the ALREADY-RUNNING process, so there is no re-exec
//! and the single-instance layer is untouched. Launches that are already
//! correctly scoped (menu/.desktop launches) are a no-op; any failure just
//! logs and leaves the X11-grab fallback in place. The id parsing below
//! mirrors the portal's own regexes so the no-op check agrees with what the
//! portal will conclude.

use std::path::PathBuf;

/// Ensure the GlobalShortcuts portal can identify this process. `app_id` is
/// the Tauri `identifier` from tauri.conf.json — the single app id, reused.
/// Every outcome is logged (release builds route stderr to tiro.log).
pub fn ensure_app_scope(app_id: &str) {
    if !crate::hotkeys_portal::wayland_session() {
        return; // the portal hotkey path is only used on Wayland sessions
    }
    if let Some(unit) = current_unit() {
        if let Some(id) = parse_app_id_from_unit(&unit) {
            // Launched as an app already (e.g. a .desktop launch).
            if desktop_file_installed(&id) {
                eprintln!(
                    "app scope: already in {unit} -> app id '{id}'; \
                     the GlobalShortcuts portal can identify us"
                );
                return;
            }
            if id == app_id {
                // Migrating would derive the same id; only the desktop
                // file is missing, and that we must not install at runtime.
                eprintln!(
                    "app scope: in {unit} -> app id '{id}', but no {id}.desktop \
                     is installed in the XDG applications dirs — the \
                     GlobalShortcuts portal will refuse and the X11-grab \
                     hotkeys stay; see BUILDING.md"
                );
                return;
            }
            // An app- unit under a foreign id the portal cannot verify:
            // fall through and claim our own.
        }
    }
    let pid = std::process::id();
    let scope = scope_name(app_id, pid);
    if let Err(e) = tauri::async_runtime::block_on(start_transient_scope(&scope, pid)) {
        eprintln!(
            "app scope: could not move into {scope}: {e}; the GlobalShortcuts \
             portal cannot identify us (the X11-grab hotkeys stay)"
        );
        return;
    }
    if !wait_for_migration(&scope) {
        eprintln!(
            "app scope: {scope} started but the cgroup move never showed up; \
             the GlobalShortcuts portal may still refuse"
        );
    } else if desktop_file_installed(app_id) {
        eprintln!(
            "app scope: moved into {scope} (app id '{app_id}') for the GlobalShortcuts portal"
        );
    } else {
        eprintln!(
            "app scope: moved into {scope}, but no {app_id}.desktop is \
             installed in the XDG applications dirs — the GlobalShortcuts \
             portal will refuse and the X11-grab hotkeys stay; install \
             src-tauri/linux/{app_id}.desktop to ~/.local/share/applications \
             (see BUILDING.md)"
        );
    }
}

/// `app-<id>-<pid>.scope` — the systemd desktop-environment spec's
/// `app[-<launcher>]-<ApplicationID>-<RANDOM>.scope` shape, with the pid as
/// the (unique-per-process) random part.
fn scope_name(app_id: &str, pid: u32) -> String {
    format!("app-{app_id}-{pid}.scope")
}

/// Move `pid` into a fresh transient scope on the systemd *user* manager
/// (session bus; no polkit involved — we own the manager). This is exactly
/// what `systemd-run --user --scope` does: StartTransientUnit with a PIDs
/// property naming the already-running process.
async fn start_transient_scope(scope: &str, pid: u32) -> Result<(), ashpd::zbus::Error> {
    use ashpd::zbus::zvariant::Value;
    let conn = ashpd::zbus::Connection::session().await?;
    let props: Vec<(&str, Value)> = vec![("PIDs", Value::from(vec![pid]))];
    let aux: Vec<(&str, Vec<(&str, Value)>)> = Vec::new();
    conn.call_method(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        "StartTransientUnit",
        &(scope, "fail", props, aux),
    )
    .await?;
    Ok(())
}

/// StartTransientUnit returns when the job is queued; the cgroup move lands
/// when the job runs (milliseconds later). Poll our own cgroup until it
/// shows the new scope. True = we are in it.
fn wait_for_migration(scope: &str) -> bool {
    for _ in 0..50 {
        if current_unit().as_deref() == Some(scope) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// Leaf unit of this process's cgroup (v2 unified hierarchy: the `0::` line
/// of /proc/self/cgroup). This is what `sd_pid_get_user_unit` reports for a
/// process sitting directly in its unit's cgroup — ours always does.
fn current_unit() -> Option<String> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?
        .trim();
    let leaf = path.rsplit('/').next()?.trim();
    if leaf.is_empty() {
        None
    } else {
        Some(leaf.to_string())
    }
}

/// The app id the portal would extract from a systemd unit name, or None.
/// Mirrors xdg-desktop-portal 1.22.1 `_xdp_app_info_host_parse_app_id_from_unit_name`:
///   regex1: `^app-(?:[[:alnum:]]+\-)?(.+?)(?:\-[[:alnum:]]*)(?:\.scope|\.slice)$`
///   regex2: `^app-(?:[[:alnum:]]+\-)?(.+?)(?:@[[:alnum:]]*|\-autostart)?\.service$`
/// followed by a relaxed C-unescape (`\x2d` etc).
pub(crate) fn parse_app_id_from_unit(unit: &str) -> Option<String> {
    let rest = unit.strip_prefix("app-")?;
    let scope_body = rest
        .strip_suffix(".scope")
        .or_else(|| rest.strip_suffix(".slice"));
    let id = match (scope_body, rest.strip_suffix(".service")) {
        (Some(body), _) => with_optional_launcher(body, lazy_id_scope)?,
        (None, Some(body)) => with_optional_launcher(body, lazy_id_service)?,
        (None, None) => return None,
    };
    let id = cunescape_relax(id);
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// The optional `(?:[[:alnum:]]+\-)?` launcher prefix, tried first like the
/// regex engine does. Backtracking inside the alnum run can never reach
/// another '-' (the run is maximal), so there is exactly one candidate.
fn with_optional_launcher<'a>(
    body: &'a str,
    id_match: impl Fn(&'a str) -> Option<&'a str>,
) -> Option<&'a str> {
    let run = body
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric())
        .count();
    if run > 0 && body.as_bytes().get(run) == Some(&b'-') {
        if let Some(id) = id_match(&body[run + 1..]) {
            return Some(id);
        }
    }
    id_match(body)
}

/// `(.+?)` then the MANDATORY `\-[[:alnum:]]*` random suffix (scope/slice
/// form): lazily, the id ends at the first '-' whose tail is all alnum.
fn lazy_id_scope(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    (1..b.len())
        .find(|&i| b[i] == b'-' && b[i + 1..].iter().all(u8::is_ascii_alphanumeric))
        .map(|i| &s[..i])
}

/// `(.+?)` then an OPTIONAL `@[[:alnum:]]*` or `-autostart` suffix (service
/// form): lazily, the id ends at the first position whose remainder is one
/// of those — or is empty (the whole body is the id).
fn lazy_id_service(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    (1..=b.len())
        .find(|&i| {
            let rest = &b[i..];
            rest.is_empty()
                || (rest[0] == b'@' && rest[1..].iter().all(u8::is_ascii_alphanumeric))
                || rest == b"-autostart"
        })
        .map(|i| &s[..i])
}

/// systemd unit names escape awkward bytes as `\xNN` (e.g. `\x2d` for '-').
/// Relaxed like the portal's UNESCAPE_RELAX: bad escapes pass through.
fn cunescape_relax(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1] == b'x' {
            if let (Some(hi), Some(lo)) = (hex(b[i + 2]), hex(b[i + 3])) {
                out.push(hi * 16 + lo);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Does `<id>.desktop` exist where the portal's `g_desktop_app_info_new`
/// will look? Advisory only (the check runs in the portal's process with its
/// own environment) — this powers log lines, never control flow the portal
/// disagrees with in the failing direction.
fn desktop_file_installed(id: &str) -> bool {
    let name = format!("{id}.desktop");
    xdg_data_dirs()
        .iter()
        .any(|dir| dir.join("applications").join(&name).is_file())
}

fn xdg_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(home) => dirs.push(PathBuf::from(home)),
        None => {
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(PathBuf::from(home).join(".local/share"));
            }
        }
    }
    match std::env::var("XDG_DATA_DIRS") {
        Ok(list) if !list.is_empty() => {
            dirs.extend(list.split(':').filter(|p| !p.is_empty()).map(PathBuf::from));
        }
        _ => {
            dirs.push(PathBuf::from("/usr/local/share"));
            dirs.push(PathBuf::from("/usr/share"));
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(unit: &str) -> Option<String> {
        parse_app_id_from_unit(unit)
    }

    #[test]
    fn our_own_scope_name_round_trips_through_the_portal_parse() {
        // The whole point: the scope we create must parse back to the
        // tauri.conf.json identifier.
        let scope = scope_name("dev.tiro.app", 41234);
        assert_eq!(scope, "app-dev.tiro.app-41234.scope");
        assert_eq!(parse(&scope).as_deref(), Some("dev.tiro.app"));
    }

    #[test]
    fn scope_and_slice_forms_parse() {
        assert_eq!(
            parse("app-org.kde.konsole-proof24208.scope").as_deref(),
            Some("org.kde.konsole")
        );
        assert_eq!(
            parse("app-org.kde.dolphin-12345.slice").as_deref(),
            Some("org.kde.dolphin")
        );
        // launcher prefix form: app-<launcher>-<id>-<random>.scope
        assert_eq!(
            parse("app-gnome-org.gnome.Baobab-1234.scope").as_deref(),
            Some("org.gnome.Baobab")
        );
    }

    #[test]
    fn scope_form_requires_the_random_suffix() {
        // The portal's regex1 makes `-<RANDOM>` mandatory for scopes, and
        // regex2 only covers .service — so a bare app-<id>.scope yields
        // nothing. (Why scope_name always appends the pid.)
        assert_eq!(parse("app-dev.tiro.app.scope"), None);
    }

    #[test]
    fn service_forms_parse() {
        // KDE's .desktop launches produce app-<id>@<random>.service.
        assert_eq!(
            parse("app-dev.tiro.app@abc123.service").as_deref(),
            Some("dev.tiro.app")
        );
        assert_eq!(
            parse("app-dev.tiro.app.service").as_deref(),
            Some("dev.tiro.app")
        );
        assert_eq!(
            parse("app-org.foo-autostart.service").as_deref(),
            Some("org.foo")
        );
        assert_eq!(
            parse("app-kde-org.kde.kalendarac@deadbeef.service").as_deref(),
            Some("org.kde.kalendarac")
        );
    }

    #[test]
    fn escaped_dashes_unescape_like_the_portal() {
        // The example from the portal source itself.
        assert_eq!(
            parse("app-gnome-org.gnome.Evolution\\x2dalarm\\x2dnotify-2437.scope").as_deref(),
            Some("org.gnome.Evolution-alarm-notify")
        );
    }

    #[test]
    fn non_app_units_yield_nothing() {
        assert_eq!(parse("tmux-spawn-c0a11fb2.scope"), None);
        assert_eq!(parse("app.slice"), None);
        assert_eq!(parse("session-2.scope"), None);
        assert_eq!(parse("app-.scope"), None);
        assert_eq!(parse("app-foo.weird"), None);
    }

    #[test]
    fn cunescape_relax_passes_bad_escapes_through() {
        assert_eq!(cunescape_relax("a\\x2db"), "a-b");
        assert_eq!(cunescape_relax("a\\xZZb"), "a\\xZZb");
        assert_eq!(cunescape_relax("trailing\\x2"), "trailing\\x2");
        assert_eq!(cunescape_relax("plain"), "plain");
    }
}
