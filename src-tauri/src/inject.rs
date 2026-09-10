//! Synthetic paste injection: press Ctrl+V at the focused window. The
//! clipboard already holds the text — this module only delivers the chord.
//!
//! Backends:
//! - Windows: `SendInput` scan of Ctrl+V.
//! - Linux, pure X11 session (`WAYLAND_DISPLAY` unset): XTEST fake key events
//!   via x11rb, keycodes resolved from the live keymap.
//! - Linux, Wayland session (`WAYLAND_DISPLAY` set): the XDG RemoteDesktop
//!   portal. Tiro's own windows are XWayland clients (GDK_BACKEND=x11), but
//!   XTEST from an XWayland client never reaches native Wayland apps, so the
//!   portal's virtual keyboard is the only injection path that works
//!   everywhere. The portal shows a one-time permission dialog; the restore
//!   token it hands back is persisted (config `portal_restore_token`) so the
//!   dialog never repeats. ONE portal session is created lazily on the first
//!   paste and reused for every later one — the desktop announces each
//!   session start (KDE pops a "remote control session started"
//!   notification), so per-paste sessions would mean per-paste
//!   notifications. The session is closed exactly once, on app exit
//!   (`close_portal_session`); a session the desktop tore down mid-run is
//!   detected on use and transparently recreated for that paste.
//!
//! THE classic bug of paste-key tools: the user's physical Ctrl+Alt+V is
//! still held when the hotkey fires, so a naive synthetic V lands as
//! Ctrl+Alt+V in the target app. Every backend below deals with it
//! concretely — Windows and X11 poll the real key state and wait for the
//! chord to be physically released (with explicit modifier key-ups as a
//! timeout backstop); the portal backend cannot see the physical keyboard,
//! so it waits a beat and sends explicit release events for every modifier
//! that is not part of the chord before pressing Ctrl+V.

/// How long to wait for the user to physically release the hotkey chord
/// before injecting anyway (with explicit key-ups as a backstop).
#[cfg(any(windows, target_os = "linux"))]
const RELEASE_WAIT_MS: u64 = 2000;

/// Poll interval while waiting for the physical release.
#[cfg(any(windows, target_os = "linux"))]
const RELEASE_POLL_MS: u64 = 15;

/// Settle delay between the observed release and the injected chord, so the
/// target app has processed its own key-up events (focus is not moving —
/// Tiro never takes it — but modifier state must have caught up).
#[cfg(any(windows, target_os = "linux"))]
const SETTLE_MS: u64 = 120;

/// Inject a Ctrl+V paste chord at the current cursor/focus position.
///
/// `load_token`/`save_token` read and persist the RemoteDesktop-portal
/// restore token ("" = none). Only the Wayland portal backend uses them, and
/// it calls both under its own paste lock — restore tokens are single-use,
/// so concurrent pastes must never interleave a read with a persist. The
/// other backends ignore them.
#[cfg(windows)]
pub fn paste_at_cursor(
    _load_token: impl FnOnce() -> String,
    _save_token: impl FnOnce(&str),
) -> Result<(), String> {
    windows_impl::paste()
}

/// See the Windows variant for the contract.
#[cfg(target_os = "linux")]
pub fn paste_at_cursor(
    load_token: impl FnOnce() -> String,
    save_token: impl FnOnce(&str),
) -> Result<(), String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        // A Wayland session, even though Tiro itself runs as an XWayland
        // client: only the portal reaches native Wayland apps.
        portal_impl::paste(load_token, save_token)
    } else {
        x11_impl::paste()
    }
}

/// Close the long-lived RemoteDesktop portal session, if one was ever
/// created. Called from the `RunEvent::Exit` hook, which the graceful exits
/// raise (tray Quit via `app.exit`, tray Restart via `request_restart`).
/// Killed processes (SIGTERM/SIGKILL) never reach it — for those the
/// backstop is process death itself: the D-Bus disconnect ends the session
/// on the portal side. An explicit Close is still worth it here because
/// ashpd sessions have no Drop hook, and on restart the old process's D-Bus
/// teardown may lag the new instance.
#[cfg(target_os = "linux")]
pub fn close_portal_session() {
    portal_impl::close_session();
}

/// Windows and macOS have no portal session; nothing to close.
#[cfg(any(windows, target_os = "macos"))]
pub fn close_portal_session() {}

#[cfg(target_os = "macos")]
pub fn paste_at_cursor(
    _load_token: impl FnOnce() -> String,
    _save_token: impl FnOnce(&str),
) -> Result<(), String> {
    crate::macos::paste()
}

#[cfg(windows)]
mod windows_impl {
    use std::mem::size_of;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    };

    const VK_SHIFT: u16 = 0x10;
    const VK_CONTROL: u16 = 0x11;
    const VK_MENU: u16 = 0x12; // Alt
    const VK_LWIN: u16 = 0x5B;
    const VK_RWIN: u16 = 0x5C;
    const VK_V: u16 = 0x56;

    fn key_down(vk: u16) -> bool {
        // High bit set = key is currently down.
        (unsafe { GetAsyncKeyState(i32::from(vk)) } as u16) & 0x8000 != 0
    }

    fn key_event(vk: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(events: &[INPUT]) -> Result<(), String> {
        let sent = unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        };
        if sent as usize == events.len() {
            Ok(())
        } else {
            Err(format!("SendInput injected {sent}/{} events", events.len()))
        }
    }

    pub fn paste() -> Result<(), String> {
        // Wait for the user's physical hotkey chord (Ctrl+Alt+V and any
        // other modifiers) to be released, otherwise the target receives
        // Ctrl+Alt+V again.
        let chord = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN, VK_V];
        let deadline = Instant::now() + Duration::from_millis(super::RELEASE_WAIT_MS);
        while chord.iter().any(|&vk| key_down(vk)) {
            if Instant::now() >= deadline {
                // Backstop: force-release the modifiers that are NOT part of
                // the injected chord so the paste still lands clean.
                eprintln!("inject: chord still held after wait; forcing modifier key-ups");
                let _ = send(&[
                    key_event(VK_MENU, true),
                    key_event(VK_SHIFT, true),
                    key_event(VK_LWIN, true),
                    key_event(VK_RWIN, true),
                ]);
                break;
            }
            std::thread::sleep(Duration::from_millis(super::RELEASE_POLL_MS));
        }
        std::thread::sleep(Duration::from_millis(super::SETTLE_MS));
        send(&[
            key_event(VK_CONTROL, false),
            key_event(VK_V, false),
            key_event(VK_V, true),
            key_event(VK_CONTROL, true),
        ])
    }
}

#[cfg(target_os = "linux")]
mod x11_impl {
    use std::time::{Duration, Instant};

    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::rust_connection::RustConnection;

    // X keysyms (X11/keysymdef.h).
    const XK_V_LOWER: u32 = 0x0076;
    const XK_SHIFT_L: u32 = 0xFFE1;
    const XK_SHIFT_R: u32 = 0xFFE2;
    const XK_CONTROL_L: u32 = 0xFFE3;
    const XK_CONTROL_R: u32 = 0xFFE4;
    const XK_META_L: u32 = 0xFFE7;
    const XK_META_R: u32 = 0xFFE8;
    const XK_ALT_L: u32 = 0xFFE9;
    const XK_ALT_R: u32 = 0xFFEA;
    const XK_SUPER_L: u32 = 0xFFEB;
    const XK_SUPER_R: u32 = 0xFFEC;

    /// Every keycode currently mapped to `keysym` (a keysym can live on
    /// several keycodes, e.g. left/right variants share one only rarely).
    fn keycodes_for(keysym: u32, min_keycode: u8, keysyms: &[u32], per_keycode: usize) -> Vec<u8> {
        keysyms
            .chunks(per_keycode.max(1))
            .enumerate()
            .filter(|(_, syms)| syms.contains(&keysym))
            .map(|(i, _)| min_keycode.saturating_add(i as u8))
            .collect()
    }

    fn keycode_down(keymap: &[u8; 32], keycode: u8) -> bool {
        keymap[usize::from(keycode) >> 3] & (1 << (keycode & 7)) != 0
    }

    fn fake_key(conn: &RustConnection, root: u32, keycode: u8, press: bool) -> Result<(), String> {
        let kind = if press {
            KEY_PRESS_EVENT
        } else {
            KEY_RELEASE_EVENT
        };
        conn.xtest_fake_input(kind, keycode, x11rb::CURRENT_TIME, root, 0, 0, 0)
            .map_err(|e| format!("XTEST fake_input: {e}"))?;
        Ok(())
    }

    pub fn paste() -> Result<(), String> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| format!("X11 connect: {e}"))?;
        let setup = conn.setup();
        let root = setup.roots[screen_num].root;
        let (min_kc, max_kc) = (setup.min_keycode, setup.max_keycode);
        let mapping = conn
            .get_keyboard_mapping(min_kc, max_kc - min_kc + 1)
            .map_err(|e| format!("get_keyboard_mapping: {e}"))?
            .reply()
            .map_err(|e| format!("get_keyboard_mapping reply: {e}"))?;
        let per = usize::from(mapping.keysyms_per_keycode);
        let find = |sym: u32| keycodes_for(sym, min_kc, &mapping.keysyms, per);

        let ctrl = find(XK_CONTROL_L);
        let v = find(XK_V_LOWER);
        let (Some(&ctrl_kc), Some(&v_kc)) = (ctrl.first(), v.first()) else {
            return Err("keymap has no Control_L / V keycode".into());
        };
        // Modifiers that must NOT contaminate the injected chord.
        let stray: Vec<u8> = [
            XK_ALT_L, XK_ALT_R, XK_META_L, XK_META_R, XK_SUPER_L, XK_SUPER_R, XK_SHIFT_L,
            XK_SHIFT_R,
        ]
        .iter()
        .flat_map(|&s| find(s))
        .collect();
        // The full physical chord we wait out: every modifier + both chord
        // keys (Ctrl_R too — the hotkey may have been pressed with it).
        let mut watched = stray.clone();
        watched.extend(find(XK_CONTROL_R));
        watched.push(ctrl_kc);
        watched.push(v_kc);

        let deadline = Instant::now() + Duration::from_millis(super::RELEASE_WAIT_MS);
        loop {
            let keymap = conn
                .query_keymap()
                .map_err(|e| format!("query_keymap: {e}"))?
                .reply()
                .map_err(|e| format!("query_keymap reply: {e}"))?
                .keys;
            if !watched.iter().any(|&kc| keycode_down(&keymap, kc)) {
                break;
            }
            if Instant::now() >= deadline {
                // Backstop: fake-release the stray modifiers so the paste
                // still lands as plain Ctrl+V.
                eprintln!("inject: chord still held after wait; forcing modifier releases");
                for &kc in &stray {
                    fake_key(&conn, root, kc, false)?;
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(super::RELEASE_POLL_MS));
        }
        std::thread::sleep(Duration::from_millis(super::SETTLE_MS));

        fake_key(&conn, root, ctrl_kc, true)?;
        fake_key(&conn, root, v_kc, true)?;
        fake_key(&conn, root, v_kc, false)?;
        fake_key(&conn, root, ctrl_kc, false)?;
        conn.flush().map_err(|e| format!("X11 flush: {e}"))?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn keycodes_for_scans_all_columns() {
            // min_keycode 8; two keysyms per keycode; keycode 9 carries 'v'.
            let syms = [0x61, 0x41, 0x76, 0x56, 0xFFE3, 0];
            assert_eq!(keycodes_for(0x76, 8, &syms, 2), vec![9]);
            assert_eq!(keycodes_for(0x56, 8, &syms, 2), vec![9], "shifted column");
            assert_eq!(keycodes_for(0xFFE3, 8, &syms, 2), vec![10]);
            assert!(keycodes_for(0x7A, 8, &syms, 2).is_empty(), "absent keysym");
        }

        #[test]
        fn keycodes_for_reports_every_match() {
            let syms = [0xFFE1, 0, 0xFFE2, 0, 0xFFE1, 0];
            assert_eq!(keycodes_for(0xFFE1, 8, &syms, 2), vec![8, 10]);
        }

        #[test]
        fn keycode_down_reads_the_bitmask() {
            let mut keymap = [0u8; 32];
            keymap[4] = 0b0000_0100; // keycode 34 (byte 4, bit 2)
            assert!(keycode_down(&keymap, 34));
            assert!(!keycode_down(&keymap, 35));
            assert!(!keycode_down(&keymap, 0));
        }
    }
}

#[cfg(target_os = "linux")]
mod portal_impl {
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop};
    use ashpd::desktop::{PersistMode, Session};

    // Linux evdev keycodes (input-event-codes.h) — what NotifyKeyboardKeycode
    // speaks.
    const KEY_LEFTCTRL: i32 = 29;
    const KEY_LEFTSHIFT: i32 = 42;
    const KEY_V: i32 = 47;
    const KEY_RIGHTSHIFT: i32 = 54;
    const KEY_LEFTALT: i32 = 56;
    const KEY_RIGHTCTRL: i32 = 97;
    const KEY_RIGHTALT: i32 = 100;
    const KEY_LEFTMETA: i32 = 125;
    const KEY_RIGHTMETA: i32 = 126;

    /// The lazily-created, long-lived portal session: the proxy it was made
    /// from plus the session handle itself.
    struct Portal {
        proxy: RemoteDesktop,
        session: Session<RemoteDesktop>,
    }

    /// One session per app run. Created on the first paste, reused for every
    /// later paste, closed exactly once by `close_session` on app exit. The
    /// desktop announces every RemoteDesktop session start (KDE pops a
    /// "remote control session started" notification), so this MUST NOT go
    /// back to a per-paste session — that is one notification per paste.
    static PORTAL: Mutex<Option<Portal>> = Mutex::new(None);

    fn portal_slot() -> MutexGuard<'static, Option<Portal>> {
        // A poisoned lock only means a paste panicked mid-flight; the slot's
        // content is still no worse than what the stale-session path already
        // handles, so keep going with it.
        PORTAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Serializes whole pastes, NOT just slot access. Pastes are NOT
    /// serialized upstream: `inject_paste` runs on the transcription worker
    /// in `finish`, after flow's busy flag is released, so paste N can still
    /// be blocked on the portal permission dialog when take N+1 finishes and
    /// pastes concurrently. Unserialized, the second thread would find the
    /// slot empty and Start a SECOND session (a second KDE notification plus
    /// a stacked permission dialog), and the two Starts would interleave
    /// their single-use restore-token reads/writes — last write wins, and if
    /// the loser's token is the live one, the next app launch re-prompts.
    static PASTE_GATE: Mutex<()> = Mutex::new(());

    fn paste_gate() -> MutexGuard<'static, ()> {
        // Poisoning: a previous paste panicked; the gate itself is stateless.
        PASTE_GATE.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Paste via the RemoteDesktop portal.
    ///
    /// The entire critical section — token load, session lookup/Start,
    /// injection, token persist — runs under `PASTE_GATE` (held across the
    /// sync `block_on`, so no lock is held across an await). The refreshed
    /// restore token is persisted in here, under the gate, for the reason on
    /// `PASTE_GATE`; a reused session refreshes nothing.
    pub fn paste(
        load_token: impl FnOnce() -> String,
        save_token: impl FnOnce(&str),
    ) -> Result<(), String> {
        let _gate = paste_gate();
        let loaded = load_token();
        // ashpd is async; ride tauri's tokio runtime from this worker thread.
        let refreshed = tauri::async_runtime::block_on(paste_async(&loaded))
            .map_err(|e| format!("RemoteDesktop portal: {e}"))?;
        persist_token(&loaded, refreshed, save_token);
        Ok(())
    }

    /// Persist the replacement token when the portal handed one back and it
    /// actually changed (no pointless config writes).
    fn persist_token(loaded: &str, refreshed: Option<String>, save: impl FnOnce(&str)) {
        if let Some(t) = refreshed {
            if t != loaded {
                save(&t);
            }
        }
    }

    /// Close the long-lived session (app exit).
    ///
    /// The close MUST be explicit: ashpd's `Session` is a plain D-Bus proxy
    /// with no Drop hook, and ashpd keeps its zbus connection in a
    /// process-global static, so a session that is merely dropped stays
    /// alive (and keeps the desktop's "remote control" indicator lit) until
    /// the whole process exits. No session yet — or a paste currently
    /// holding the slot's session — is a quiet no-op; in the latter case
    /// process exit drops the D-Bus connection moments later and the portal
    /// ends the session with it.
    ///
    /// This runs on the main thread during `RunEvent::Exit`, so the Close is
    /// done on a helper thread with a deadline — a wedged portal must delay
    /// Quit by at most ~2 s, and past that the D-Bus disconnect at process
    /// death ends the session anyway.
    pub fn close_session() {
        let Some(p) = portal_slot().take() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        // The handle is deliberately dropped (detached): on timeout the
        // thread may be stuck inside the portal call, and the process is
        // about to exit either way.
        std::thread::spawn(move || {
            let _ = tx.send(tauri::async_runtime::block_on(p.session.close()));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(())) => eprintln!("inject: portal session closed on exit"),
            Ok(Err(e)) => eprintln!("inject: portal session close on exit failed: {e}"),
            Err(_) => eprintln!("inject: portal session close timed out; exiting anyway"),
        }
    }

    /// Holds a session that is out of the slot. If a panic unwinds past it,
    /// `Drop` puts the session back in the slot so `close_session` at exit
    /// can still end it — a bare drop would leak the session until process
    /// death (no Drop hook in ashpd). Normal paths consume it first: `drop`
    /// after re-stowing is the success path, `close_quietly` the stale path.
    struct Held(Option<Portal>);

    impl Held {
        /// Explicit-name alias for the Drop behavior: session goes back in
        /// the slot.
        fn stow_back(self) {}

        /// Close the held session, best effort — used when it is stale (the
        /// usual close failure is the session already being gone portal-side).
        async fn close_quietly(mut self) {
            if let Some(p) = self.0.take() {
                let _ = p.session.close().await;
            }
        }
    }

    impl Drop for Held {
        fn drop(&mut self) {
            if let Some(p) = self.0.take() {
                *portal_slot() = Some(p);
            }
        }
    }

    /// Reuse the stored session when there is one; (re)create it when there
    /// is none or the stored one turns out to be dead (portal restart,
    /// compositor hiccup, permission revoked) — the recreate happens within
    /// the same paste, so a stale session still lands this take.
    ///
    /// The session is taken OUT of the slot (inside `Held`) for the duration
    /// of the paste — the slot mutex is never held across an await, and
    /// `PASTE_GATE` guarantees no other paste touches the slot meanwhile.
    async fn paste_async(restore_token: &str) -> Result<Option<String>, ashpd::Error> {
        use ashpd::desktop::CreateSessionOptions;

        let held = Held(portal_slot().take());
        if let Some(p) = held.0.as_ref() {
            match inject_chord(&p.proxy, &p.session).await {
                Ok(()) => {
                    eprintln!("inject: portal session reused");
                    held.stow_back();
                    return Ok(None);
                }
                Err(e) => {
                    eprintln!("inject: portal session stale ({e}); recreating");
                    held.close_quietly().await;
                }
            }
        }

        let proxy = RemoteDesktop::new().await?;
        let session = proxy
            .create_session(CreateSessionOptions::default())
            .await?;
        // From here on a live portal session exists on the bus: it must
        // either end up in PORTAL (closed later by `close_session`) or be
        // explicitly closed before returning an error — never merely
        // dropped (see `close_session` on why Drop is not enough). A panic
        // in this stretch WOULD drop it unclosed; that leak lasts until
        // process death ends the D-Bus connection — an accepted panic-path
        // tradeoff (`Held` covers the common taken-from-slot case above).
        let result = match start_session(&proxy, &session, restore_token).await {
            Ok(new_token) => inject_chord(&proxy, &session).await.map(|()| new_token),
            Err(e) => Err(e),
        };
        match result {
            Ok(new_token) => {
                eprintln!("inject: portal session created");
                *portal_slot() = Some(Portal { proxy, session });
                Ok(new_token)
            }
            Err(e) => {
                if let Err(close_err) = session.close().await {
                    eprintln!("inject: portal session close failed: {close_err}");
                }
                Err(e)
            }
        }
    }

    /// Device selection + start for a fresh session. With a valid restore
    /// token the Start resolves silently; without one the desktop shows the
    /// one-time permission dialog here. Returns the replacement token.
    async fn start_session(
        proxy: &RemoteDesktop,
        session: &Session<RemoteDesktop>,
        restore_token: &str,
    ) -> Result<Option<String>, ashpd::Error> {
        use ashpd::desktop::remote_desktop::{SelectDevicesOptions, StartOptions};

        let token = if restore_token.is_empty() {
            None
        } else {
            Some(restore_token)
        };
        proxy
            .select_devices(
                session,
                SelectDevicesOptions::default()
                    .set_devices(ashpd::enumflags2::BitFlags::from(DeviceType::Keyboard))
                    .set_persist_mode(PersistMode::ExplicitlyRevoked)
                    .set_restore_token(token),
            )
            .await?
            .response()?;
        let devices = proxy
            .start(session, None, StartOptions::default())
            .await?
            .response()?;
        Ok(devices.restore_token().map(ToOwned::to_owned))
    }

    /// The Ctrl+V chord on an already-started session. Runs per paste.
    ///
    /// The portal cannot see the physical keyboard, so the held-hotkey
    /// problem is handled blind: give the user's fingers a beat to leave
    /// the chord, then explicitly release every modifier that is not part
    /// of Ctrl+V before pressing it. A release for an already-up key is a
    /// harmless no-op at the compositor.
    /// (block_on drives this future on the calling worker thread, so a
    /// plain thread sleep is safe and avoids a direct tokio dependency.)
    async fn inject_chord(
        proxy: &RemoteDesktop,
        session: &Session<RemoteDesktop>,
    ) -> Result<(), ashpd::Error> {
        std::thread::sleep(std::time::Duration::from_millis(super::SETTLE_MS + 30));
        let key = |code: i32, state: KeyState| {
            proxy.notify_keyboard_keycode(session, code, state, Default::default())
        };
        for stray in [
            KEY_LEFTALT,
            KEY_RIGHTALT,
            KEY_LEFTSHIFT,
            KEY_RIGHTSHIFT,
            KEY_LEFTMETA,
            KEY_RIGHTMETA,
            KEY_RIGHTCTRL,
            KEY_V, // the user's own V must be logically up before we press it
        ] {
            key(stray, KeyState::Released).await?;
        }
        key(KEY_LEFTCTRL, KeyState::Pressed).await?;
        key(KEY_V, KeyState::Pressed).await?;
        key(KEY_V, KeyState::Released).await?;
        key(KEY_LEFTCTRL, KeyState::Released).await?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// `close_session` with no live session must be a silent no-op that
        /// never touches D-Bus (there is no bus in the test environment —
        /// reaching it would error or hang, failing this test).
        #[test]
        fn close_without_session_is_a_noop() {
            assert!(portal_slot().is_none());
            close_session();
            assert!(portal_slot().is_none());
        }

        /// The gate that serializes whole pastes: no two threads may ever be
        /// inside the critical section at once (the D1 race — a paste stuck
        /// on the permission dialog while the next take's finish pastes).
        #[test]
        fn paste_gate_admits_one_paste_at_a_time() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static INSIDE: AtomicBool = AtomicBool::new(false);

            let threads: Vec<_> = (0..4)
                .map(|_| {
                    std::thread::spawn(|| {
                        for _ in 0..25 {
                            let _gate = paste_gate();
                            assert!(
                                !INSIDE.swap(true, Ordering::SeqCst),
                                "two pastes inside the gate at once"
                            );
                            std::thread::sleep(std::time::Duration::from_micros(50));
                            INSIDE.store(false, Ordering::SeqCst);
                        }
                    })
                })
                .collect();
            for t in threads {
                t.join().expect("gate thread panicked");
            }
        }

        /// Token persistence: only a real replacement is written back.
        #[test]
        fn persist_token_saves_only_changed_replacements() {
            let mut saved: Option<String> = None;
            persist_token("old", Some("new".into()), |t| saved = Some(t.into()));
            assert_eq!(saved.as_deref(), Some("new"));

            let mut saved: Option<String> = None;
            persist_token("same", Some("same".into()), |t| saved = Some(t.into()));
            assert_eq!(saved, None, "unchanged token must not be rewritten");

            let mut saved: Option<String> = None;
            persist_token("old", None, |t| saved = Some(t.into()));
            assert_eq!(saved, None, "a reused session refreshes nothing");
        }
    }
}
