// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // CLI subcommands run headless, before any window/GPU machinery exists.
    if std::env::args().any(|a| a == "--record-test") {
        tiro_lib::audio::record_test();
        return;
    }
    if std::env::args().any(|a| a == "--cue-test") {
        tiro_lib::cues::cue_test();
        return;
    }
    tiro_lib::run()
}
