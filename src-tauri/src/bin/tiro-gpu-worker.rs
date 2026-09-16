//! `tiro-gpu-worker`: the GPU side of Tiro as its own executable.
//!
//! This is the ONE binary that links a GPU backend (Vulkan with
//! `--features gpu`, Metal with `--features metal`). Keeping it out of
//! `tiro` means the app always starts — a Vulkan-linked executable fails
//! to load at all on a machine without the Vulkan runtime — and that the
//! main process never initializes a GPU context (design rule 1,
//! POWER_AND_DGPU.md). The app spawns this file next to itself for two
//! jobs, both headless:
//!
//!   --gpu-worker ...   the framed-protocol inference child (gpu_worker.rs)
//!   --gpu-enum         print the usable GPU devices as one JSON line (hw.rs)
//!
//! stdout carries only protocol bytes / the JSON line; a missing or
//! unlaunchable worker reads as "GPU unavailable" in the app, which then
//! serves on CPU.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = if args.iter().any(|a| a == "--gpu-enum") {
        tiro_lib::hw::gpu_enum_main()
    } else if args.iter().any(|a| a == "--gpu-worker") {
        tiro_lib::gpu_worker::run(&args)
    } else {
        eprintln!(
            "usage: tiro-gpu-worker --gpu-enum | --gpu-worker --model NAME --models-dir DIR [...]"
        );
        2
    };
    std::process::exit(code);
}
