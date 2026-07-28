// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
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
