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
//!   dialog never repeats.
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
/// `restore_token` is the persisted RemoteDesktop-portal token ("" = none).
/// On success the portal backend may return a NEW token that the caller must
/// persist for the next session; the other backends return `None`.
#[cfg(windows)]
pub fn paste_at_cursor(_restore_token: &str) -> Result<Option<String>, String> {
    windows_impl::paste().map(|()| None)
}

/// See the Windows variant for the contract.
#[cfg(target_os = "linux")]
pub fn paste_at_cursor(restore_token: &str) -> Result<Option<String>, String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        // A Wayland session, even though Tiro itself runs as an XWayland
        // client: only the portal reaches native Wayland apps.
        portal_impl::paste(restore_token)
    } else {
        x11_impl::paste().map(|()| None)
    }
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
    use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop};
    use ashpd::desktop::PersistMode;

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

    /// Paste via the RemoteDesktop portal. Returns the (possibly refreshed)
    /// restore token to persist — portal restore tokens are single-use, so
    /// every successful Start hands back a replacement.
    pub fn paste(restore_token: &str) -> Result<Option<String>, String> {
        // ashpd is async; ride tauri's tokio runtime from this worker thread.
        tauri::async_runtime::block_on(paste_async(restore_token))
            .map_err(|e| format!("RemoteDesktop portal: {e}"))
    }

    async fn paste_async(restore_token: &str) -> Result<Option<String>, ashpd::Error> {
        use ashpd::desktop::remote_desktop::{SelectDevicesOptions, StartOptions};
        use ashpd::desktop::CreateSessionOptions;

        let proxy = RemoteDesktop::new().await?;
        let session = proxy
            .create_session(CreateSessionOptions::default())
            .await?;
        let token = if restore_token.is_empty() {
            None
        } else {
            Some(restore_token)
        };
        proxy
            .select_devices(
                &session,
                SelectDevicesOptions::default()
                    .set_devices(ashpd::enumflags2::BitFlags::from(DeviceType::Keyboard))
                    .set_persist_mode(PersistMode::ExplicitlyRevoked)
                    .set_restore_token(token),
            )
            .await?
            .response()?;
        // With a valid restore token this resolves silently; without one the
        // desktop shows the one-time permission dialog here.
        let devices = proxy
            .start(&session, None, StartOptions::default())
            .await?
            .response()?;
        let new_token = devices.restore_token().map(ToOwned::to_owned);

        // The portal cannot see the physical keyboard, so the held-hotkey
        // problem is handled blind: give the user's fingers a beat to leave
        // the chord, then explicitly release every modifier that is not part
        // of Ctrl+V before pressing it. A release for an already-up key is a
        // harmless no-op at the compositor.
        // (block_on drives this future on the calling worker thread, so a
        // plain thread sleep is safe and avoids a direct tokio dependency.)
        std::thread::sleep(std::time::Duration::from_millis(super::SETTLE_MS + 30));
        let key = |code: i32, state: KeyState| {
            proxy.notify_keyboard_keycode(&session, code, state, Default::default())
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
        session.close().await?;
        Ok(new_token)
    }
}
