//! Give the process a portal-visible app id by putting it in an
//! `app-<id>-<pid>.scope` systemd user scope.
//!
//! xdg-desktop-portal resolves a non-sandboxed caller's app id ITSELF — the
//! request carries no client-settable identity. Verified against the
//! installed portal (1.22.1, `src/xdp-app-info-host.c`): it asks systemd for
//! the caller's user unit (`sd_pid_get_user_unit`), requires an `app-…` name
//! in the systemd desktop-environment format, extracts the app id from the
//! unit name, and then requires `<app id>.desktop` to LOAD under GLib
//! (`g_desktop_app_info_new`): present in the XDG applications dirs AND
//! carrying an Exec whose argv0 resolves to an executable in the portal
//! process's PATH. Any miss leaves the app id empty, and `GlobalShortcuts.CreateSession` then refuses with NotAllowed
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
//!
//! Future alternative (not implemented): xdg-desktop-portal >= 1.18 also
//! exposes `org.freedesktop.host.portal.Registry.Register(app_id)` (present
//! in the installed 1.22.1: data/org.freedesktop.host.portal.Registry.xml,
//! src/registry.c), which associates a D-Bus connection with an app id
//! directly — no systemd-unit derivation and no upfront desktop-file gate.
//! It must be that connection's FIRST portal contact ("Registered too
//! late"), and ashpd owns its own connection, so adopting it means hooking
//! ashpd's connection before the first GlobalShortcuts call.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Ensure the GlobalShortcuts portal can identify this process. `app_id` is
/// the Tauri `identifier` from tauri.conf.json — the single app id, reused.
/// Every outcome is logged (release builds route stderr to tiro.log).
pub fn ensure_app_scope(app_id: &str) {
    if !crate::hotkeys_portal::wayland_session() {
        return; // the portal hotkey path is only used on Wayland sessions
    }
    if let Some(unit) = current_unit() {
        if !should_migrate(Some(&unit), app_id) {
            // Launched AS our app (a .desktop launch): the unit already
            // derives our id — a migration would be a no-op. Only the
            // OUR-id case may skip: a foreign app id (a terminal like
            // Alacritty or xterm keeps children in its own app-… scope)
            // would file our shortcuts under THAT app in the compositor's
            // store, varying per launch and orphaning bindings.
            match desktop_file_status(app_id) {
                Ok(file) => eprintln!(
                    "app scope: already in {unit} -> app id '{app_id}' ({}); \
                     the GlobalShortcuts portal can identify us",
                    file.display()
                ),
                Err(why) => eprintln!(
                    "app scope: in {unit} -> app id '{app_id}', but {why} — \
                     the GlobalShortcuts portal will refuse and the X11-grab \
                     hotkeys stay; see BUILDING.md"
                ),
            }
            return;
        }
    }
    // The terminal's scope, no systemd unit at all, or a foreign app scope:
    // claim our own.
    let pid = std::process::id();
    let scope = scope_name(app_id, pid);
    if let Err(e) = start_transient_scope_bounded(scope.clone(), pid) {
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
        return;
    }
    match desktop_file_status(app_id) {
        Ok(file) => eprintln!(
            "app scope: moved into {scope} (app id '{app_id}', {}) for the \
             GlobalShortcuts portal",
            file.display()
        ),
        Err(why) => eprintln!(
            "app scope: moved into {scope}, but {why} — the GlobalShortcuts \
             portal will refuse and the X11-grab hotkeys stay; see BUILDING.md \
             (install src-tauri/linux/{app_id}.desktop and put a `tiro` \
             executable on PATH)"
        ),
    }
}

/// Migrate unless the inherited unit already derives OUR app id. Any other
/// outcome — no unit, an unparseable unit, or a unit that parses to a
/// foreign id — needs a scope of our own.
fn should_migrate(current_unit: Option<&str>, app_id: &str) -> bool {
    current_unit
        .and_then(parse_app_id_from_unit)
        .is_none_or(|id| id != app_id)
}

/// `app-<id>-<pid>.scope` — the systemd desktop-environment spec's
/// `app[-<launcher>]-<ApplicationID>-<RANDOM>.scope` shape, with the pid as
/// the (unique-per-process) random part.
fn scope_name(app_id: &str, pid: u32) -> String {
    format!("app-{app_id}-{pid}.scope")
}

/// zbus's connect handshake and method call carry no timeout of their own,
/// and this runs on the main thread during setup — a wedged session bus or
/// user manager must not hang startup invisibly forever. Run the whole
/// D-Bus phase on a detached thread and give up after a few seconds; a
/// timed-out thread is leaked (it holds nothing but a bus connection), and
/// a migration landing late is harmless.
const DBUS_TIMEOUT: Duration = Duration::from_secs(5);

fn start_transient_scope_bounded(scope: String, pid: u32) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = tauri::async_runtime::block_on(start_transient_scope(&scope, pid));
        let _ = tx.send(result.map_err(|e| e.to_string()));
    });
    match rx.recv_timeout(DBUS_TIMEOUT) {
        Ok(result) => result,
        Err(_) => Err(format!(
            "no reply from the session bus / systemd user manager within {}s",
            DBUS_TIMEOUT.as_secs()
        )),
    }
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

/// Would the portal's `g_desktop_app_info_new` accept `id`? GLib does more
/// than stat the file: it parses it and REJECTS an entry whose Exec argv0
/// does not resolve to an executable (absolute path, else $PATH search) —
/// a bare existence check would log success while the portal still refuses.
/// Ok carries the satisfying file; Err says precisely which half is broken.
/// Advisory only (the real check runs in the portal's process with its own
/// environment): powers log lines, never control flow.
fn desktop_file_status(id: &str) -> Result<PathBuf, String> {
    let name = format!("{id}.desktop");
    let Some(file) = xdg_data_dirs()
        .iter()
        .map(|dir| dir.join("applications").join(&name))
        .find(|p| p.is_file())
    else {
        return Err(format!("no {name} in the XDG applications dirs"));
    };
    let content = std::fs::read_to_string(&file)
        .map_err(|e| format!("{} is unreadable: {e}", file.display()))?;
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    match exec_resolves(&content, &path_dirs) {
        Ok(()) => Ok(file),
        Err(why) => Err(format!("{}: {why}", file.display())),
    }
}

/// The GLib-essential Exec validation, pure for testing: the
/// `[Desktop Entry]` group must carry an Exec whose argv0 resolves to an
/// executable — an argv0 with a path separator is checked directly, a bare
/// name is searched in `path_dirs`.
fn exec_resolves(content: &str, path_dirs: &[PathBuf]) -> Result<(), String> {
    let argv0 = exec_argv0(content)
        .ok_or("no Exec key in its [Desktop Entry] group (GLib rejects the file)")?;
    let found = if argv0.contains('/') {
        is_executable(Path::new(&argv0))
    } else {
        path_dirs.iter().any(|dir| is_executable(&dir.join(&argv0)))
    };
    if found {
        Ok(())
    } else {
        Err(format!(
            "its Exec argv0 '{argv0}' does not resolve to an executable, \
             so GLib rejects the file (and the portal the app id)"
        ))
    }
}

/// argv0 of the `[Desktop Entry]` group's Exec key: the first
/// double-quoted or whitespace-delimited token, mirroring how GLib's
/// `g_shell_parse_argv` would split it for the common cases.
fn exec_argv0(content: &str) -> Option<String> {
    let mut in_entry = false;
    for line in content.lines() {
        let line = line.trim();
        if let Some(group) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_entry = group == "Desktop Entry";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "Exec" {
            continue;
        }
        let value = value.trim();
        let argv0 = match value.strip_prefix('"') {
            Some(rest) => rest.split('"').next().unwrap_or_default(),
            None => value.split_whitespace().next().unwrap_or_default(),
        };
        return if argv0.is_empty() {
            None
        } else {
            Some(argv0.to_string())
        };
    }
    None
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
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

    const OUR_ID: &str = "dev.tiro.app";

    #[test]
    fn own_id_no_ops_foreign_id_migrates() {
        // Only a unit deriving OUR id may skip migration; inheriting a
        // terminal's app scope must not leave us identified as the terminal.
        assert!(!should_migrate(
            Some("app-dev.tiro.app-41234.scope"),
            OUR_ID
        ));
        assert!(!should_migrate(
            Some("app-dev.tiro.app@deadbeef.service"),
            OUR_ID
        ));
        assert!(should_migrate(
            Some("app-org.kde.konsole-1234.scope"),
            OUR_ID
        ));
        assert!(should_migrate(Some("app-Alacritty-77.scope"), OUR_ID));
        assert!(should_migrate(Some("tmux-spawn-c0a11fb2.scope"), OUR_ID));
        assert!(should_migrate(None, OUR_ID));
    }

    #[test]
    fn exec_argv0_extraction() {
        let entry = "[Desktop Entry]\nType=Application\nExec=/usr/bin/tiro --flag\n";
        assert_eq!(exec_argv0(entry).as_deref(), Some("/usr/bin/tiro"));
        let quoted = "[Desktop Entry]\nExec=\"/opt/spaced dir/tiro\" %U\n";
        assert_eq!(exec_argv0(quoted).as_deref(), Some("/opt/spaced dir/tiro"));
        let spaced_eq = "[Desktop Entry]\nExec = tiro\n";
        assert_eq!(exec_argv0(spaced_eq).as_deref(), Some("tiro"));
        assert_eq!(exec_argv0("[Desktop Entry]\nName=Tiro\n"), None);
        // Exec outside [Desktop Entry] (e.g. a Desktop Action) is GLib-invisible
        assert_eq!(
            exec_argv0("[Desktop Action new]\nExec=/bin/sh\n[Desktop Entry]\nName=T\n"),
            None
        );
    }

    #[test]
    fn exec_resolution_mirrors_glib() {
        // Absolute argv0: checked directly, PATH irrelevant.
        let abs = "[Desktop Entry]\nExec=/bin/sh -c true\n";
        assert!(exec_resolves(abs, &[]).is_ok());
        // Bare argv0: found via the given PATH dirs.
        let bare = "[Desktop Entry]\nExec=sh\n";
        assert!(exec_resolves(bare, &[PathBuf::from("/usr/bin")]).is_ok());
        // Missing binary: GLib would reject the file -> so do we.
        let missing = "[Desktop Entry]\nExec=definitely-not-a-real-binary-4213\n";
        let err = exec_resolves(missing, &[PathBuf::from("/usr/bin")]).expect_err("must fail");
        assert!(err.contains("definitely-not-a-real-binary-4213"), "{err}");
        assert!(err.contains("does not resolve"), "{err}");
        // No Exec at all: GLib rejects too.
        let none = "[Desktop Entry]\nName=Tiro\n";
        let err = exec_resolves(none, &[]).expect_err("must fail");
        assert!(err.contains("no Exec"), "{err}");
    }
}
