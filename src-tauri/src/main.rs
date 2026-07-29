// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Release builds run without a console (windows_subsystem on Windows, a
/// desktop launch on Linux), so diagnostics would vanish — send stderr to
/// tiro.log in the app dir instead (NOT the CWD: an autostart launch runs
/// with CWD = $HOME), the port's answer to the original's tiro.log. On Unix
/// dup2 also captures whisper.cpp's C-level stderr; on Windows SetStdHandle
/// covers the Rust side (C runtime output latched its handle at startup and
/// is not recoverable there).
#[cfg(not(debug_assertions))]
fn stderr_to_log() {
    use std::fs::OpenOptions;
    let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(tiro_lib::flow::app_dir().join("tiro.log"))
    else {
        return;
    };
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::dup2(file.as_raw_fd(), 2) };
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
        unsafe { SetStdHandle(STD_ERROR_HANDLE, file.as_raw_handle()) };
    }
    // The fd/handle must outlive the process's logging.
    std::mem::forget(file);
    eprintln!(
        "---- tiro start {} ----",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
}

/// A panic must leave evidence: log its message + location to stderr (which
/// release builds redirect to tiro.log), then hand off to the previous hook
/// so the standard report — including a backtrace when RUST_BACKTRACE is
/// set — still prints. Reads fd 2 at panic time, so it composes with
/// `stderr_to_log` regardless of install order.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = payload_str(info.payload());
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        eprintln!(
            "PANIC {} at {loc}: {msg}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        previous(info);
    }));
}

/// The panic payload as text: `panic!` carries a `&str` or `String`;
/// anything else is opaque.
fn payload_str(payload: &dyn std::any::Any) -> &str {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "<non-string panic payload>"
    }
}

fn main() {
    // KDE-taskbar fix, and it must run before anything touches GTK: the GTK
    // Wayland backend has no concept of skip-taskbar / taskhint (xdg-shell
    // offers no such state), so tao's skipTaskbar — and several other window
    // hints — are silently dropped and both frameless windows show up as
    // regular apps in the Plasma task switcher. Under X11/XWayland the
    // _NET_WM_STATE_SKIP_TASKBAR hint exists and KWin honors it, so on a
    // Wayland session we steer GDK to the X11 backend — but only when
    // XWayland is actually there (DISPLAY set) and the user hasn't chosen a
    // backend themselves.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var_os("DISPLAY").is_some()
        && std::env::var_os("GDK_BACKEND").is_none()
    {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    install_panic_hook();
    #[cfg(not(debug_assertions))]
    stderr_to_log();
    // CLI subcommands run headless, before any window/GPU machinery exists.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--gpu-worker") {
        std::process::exit(tiro_lib::gpu_worker::run(&args));
    }
    if std::env::args().any(|a| a == "--record-test") {
        tiro_lib::audio::record_test();
        return;
    }
    if std::env::args().any(|a| a == "--cue-test") {
        tiro_lib::cues::cue_test();
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--autostart-test") {
        // Exercises the same auto-launch config the autostart plugin builds
        // (app name = productName "tiro", path = current exe, no args).
        let mode = args.get(i + 1).map(String::as_str).unwrap_or("status");
        let exe = std::env::current_exe().expect("current_exe");
        let al = auto_launch::AutoLaunchBuilder::new()
            .set_app_name("tiro")
            .set_app_path(&exe.display().to_string())
            .build()
            .expect("auto-launch build");
        let result = match mode {
            "on" => al.enable().map_err(|e| e.to_string()),
            "off" => al.disable().map_err(|e| e.to_string()),
            _ => Ok(()),
        };
        match result {
            Ok(()) => eprintln!(
                "autostart {mode}: is_enabled = {:?}",
                al.is_enabled().map_err(|e| e.to_string())
            ),
            Err(e) => eprintln!("autostart {mode} FAILED: {e}"),
        }
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--copy-test") {
        match args.get(i + 1) {
            Some(text) => match tiro_lib::clipboard::copy(text) {
                Ok(()) => eprintln!("copied {} chars to the clipboard", text.chars().count()),
                Err(e) => eprintln!("clipboard copy failed: {e}"),
            },
            None => eprintln!("usage: tiro --copy-test <text>"),
        }
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--gpu-test") {
        match args.get(i + 1) {
            Some(wav) => {
                let device = args.get(i + 2).map(String::as_str).unwrap_or("cpu");
                tiro_lib::gpu::gpu_test(wav, device);
            }
            None => eprintln!("usage: tiro --gpu-test <wav> [gpu|cpu]"),
        }
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--transcribe-test") {
        match args.get(i + 1) {
            Some(wav) => tiro_lib::transcribe::transcribe_test(wav),
            None => eprintln!("usage: tiro --transcribe-test <wav>"),
        }
        return;
    }
    tiro_lib::run()
}

#[cfg(test)]
mod tests {
    use super::payload_str;

    #[test]
    fn payload_str_covers_panic_shapes() {
        let s: Box<dyn std::any::Any> = Box::new("boom");
        assert_eq!(payload_str(s.as_ref()), "boom");
        let s: Box<dyn std::any::Any> = Box::new(String::from("boom 42"));
        assert_eq!(payload_str(s.as_ref()), "boom 42");
        let s: Box<dyn std::any::Any> = Box::new(42_u32);
        assert_eq!(payload_str(s.as_ref()), "<non-string panic payload>");
    }
}
