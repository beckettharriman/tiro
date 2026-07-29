//! Hardware class detection and the engine policy table.
//!
//! The machine's class — what GPU silicon it has crossed with whether it
//! runs on a battery — decides how the Auto power mode behaves and which
//! Engine settings the panel shows. Two facts feed it:
//!
//! - **gpu_class** (none | integrated | discrete): from Vulkan device
//!   enumeration. HARD CONSTRAINT (design rule 1, POWER_AND_DGPU.md): the
//!   main process must NEVER initialize a GPU/Vulkan context — even a bare
//!   enumeration creates a driver context that pins the discrete GPU out of
//!   D3cold until process exit. So enumeration runs in a short-lived child
//!   (`tiro --gpu-enum`, the same re-exec pattern as `--gpu-worker`): the
//!   child creates the Vulkan instance, prints the device list as one JSON
//!   line, and exits — the context dies with it. The parent caches the
//!   result for the life of the process (re-enumerated each app start).
//!
//! - **battery_present**: from the OS power layer (see `power`). PRESENCE,
//!   not charge state — a desktop has no battery to flap on.
//!
//! A machine is a **desktop** when it has no battery OR the user set the
//! `treat_as_desktop` config override. Desktops have no battery/AC split:
//! the single `model` key serves everywhere and the power watcher's
//! AC/battery logic is inert.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How long the parent waits for `--gpu-enum` before assuming no usable
/// GPU. Vulkan instance creation is normally well under a second; a driver
/// wedged longer than this should not stall app startup.
const ENUM_TIMEOUT: Duration = Duration::from_secs(20);

/// The GPU silicon class. `Unified` is reserved for a future Apple port —
/// it exists so the policy table's shape doesn't churn then; nothing
/// constructs it today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuClass {
    None,
    Integrated,
    Discrete,
    #[allow(dead_code)] // future Apple-silicon variant; policy-table-ready
    Unified,
}

impl GpuClass {
    pub fn as_str(self) -> &'static str {
        match self {
            GpuClass::None => "none",
            GpuClass::Integrated => "integrated",
            GpuClass::Discrete => "discrete",
            GpuClass::Unified => "unified",
        }
    }
}

/// One usable Vulkan device as `--gpu-enum` reports it. `index` is the
/// whisper.cpp `gpu_device` index: ggml counts only GPU/IGPU-type backend
/// devices, in registry order — the enumeration below counts identically,
/// so this index is exactly what the worker's `--gpu-device` selects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GpuDevice {
    pub index: usize,
    pub name: String,
    /// "discrete" | "integrated" (ggml's GPU vs IGPU device type).
    pub kind: String,
    /// Total device-local memory in bytes; 0 when the driver hides it.
    pub vram_bytes: u64,
}

/// The cached per-process hardware snapshot.
pub struct Hardware {
    pub gpus: Vec<GpuDevice>,
    pub class: GpuClass,
    pub battery_present: bool,
}

static HW: OnceLock<Hardware> = OnceLock::new();

/// The hardware snapshot, detected once per app start. The first call
/// spawns the `--gpu-enum` child and reads the battery — callers must not
/// hold main-thread-reachable locks across it (it can take a second or
/// two); `warm_up()` at startup makes later calls effectively free.
pub fn snapshot() -> &'static Hardware {
    HW.get_or_init(|| {
        let gpus = enumerate_gpus();
        let class = classify(&gpus);
        let battery_present = crate::power::battery_present();
        eprintln!(
            "hardware: gpu_class={} ({} device{}), battery_present={}",
            class.as_str(),
            gpus.len(),
            if gpus.len() == 1 { "" } else { "s" },
            battery_present
        );
        Hardware {
            gpus,
            class,
            battery_present,
        }
    })
}

/// Kick the detection off a background thread at startup so the first
/// get_state / resolve never pays the child-spawn latency inline.
pub fn warm_up() {
    std::thread::spawn(|| {
        let _ = snapshot();
    });
}

/// gpu_class from the device list: discrete wins over integrated; an empty
/// list (no Vulkan, CPU-only build, enum failure) is `None`.
pub fn classify(gpus: &[GpuDevice]) -> GpuClass {
    if gpus.iter().any(|g| g.kind == "discrete") {
        GpuClass::Discrete
    } else if gpus.is_empty() {
        GpuClass::None
    } else {
        GpuClass::Integrated
    }
}

// ---- the --gpu-enum child --------------------------------------------------

/// Entry point for `tiro --gpu-enum`: enumerate ggml backend devices (this
/// initializes the Vulkan instance — in THIS disposable process only),
/// print them as one JSON line on stdout, exit. Diagnostics go to stderr;
/// stdout carries only the JSON.
pub fn gpu_enum_main() -> i32 {
    let devices = enum_devices_in_process();
    match serde_json::to_string(&devices) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(e) => {
            eprintln!("gpu-enum: serialize failed: {e}");
            1
        }
    }
}

/// The in-process enumeration the child runs. Counts only GPU/IGPU-type
/// devices, in ggml registry order — the same walk whisper.cpp's
/// `whisper_backend_init_gpu` does against `params.gpu_device`, so the
/// reported `index` maps 1:1 onto the worker's device choice. Without the
/// Vulkan feature ggml registers no GPU devices and this returns empty.
fn enum_devices_in_process() -> Vec<GpuDevice> {
    use std::ffi::CStr;
    use whisper_rs::whisper_rs_sys as sys;
    let mut out = Vec::new();
    let mut gpu_index = 0usize;
    unsafe {
        let count = sys::ggml_backend_dev_count();
        for i in 0..count {
            let dev = sys::ggml_backend_dev_get(i);
            let kind = match sys::ggml_backend_dev_type(dev) {
                sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU => "discrete",
                sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU => "integrated",
                _ => continue,
            };
            let name = {
                let desc = sys::ggml_backend_dev_description(dev);
                if desc.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(desc).to_string_lossy().into_owned()
                }
            };
            let mut free = 0usize;
            let mut total = 0usize;
            sys::ggml_backend_dev_memory(dev, &mut free, &mut total);
            out.push(GpuDevice {
                index: gpu_index,
                name,
                kind: kind.to_string(),
                vram_bytes: total as u64,
            });
            gpu_index += 1;
        }
    }
    out
}

/// Parent side: run `tiro --gpu-enum` (re-exec, like the GPU worker spawn)
/// and parse its JSON line. Any failure — spawn error, timeout, bad JSON —
/// degrades to an empty list (gpu_class none): the battery-safe reading,
/// and the CPU engine always works.
fn enumerate_gpus() -> Vec<GpuDevice> {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("gpu-enum: current_exe failed: {e}");
            return Vec::new();
        }
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--gpu-enum")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        // CREATE_NO_WINDOW, matching the worker spawn.
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gpu-enum: spawn failed: {e}");
            return Vec::new();
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Vec::new();
    };
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(ENUM_TIMEOUT) {
        Ok(buf) => {
            let _ = child.wait();
            parse_enum_output(&buf)
        }
        Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
            eprintln!(
                "gpu-enum: no reply within {}s; assuming no usable GPU",
                ENUM_TIMEOUT.as_secs()
            );
            let _ = child.kill();
            let _ = child.wait();
            Vec::new()
        }
    }
}

/// The child's stdout should be exactly one JSON line, but be lenient: take
/// the first line that parses as a device array (a driver spewing onto
/// stdout must not read as "no GPU" if the real line is there too).
fn parse_enum_output(raw: &str) -> Vec<GpuDevice> {
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(devs) = serde_json::from_str::<Vec<GpuDevice>>(line) {
            return devs;
        }
    }
    if !raw.trim().is_empty() {
        eprintln!("gpu-enum: unparseable output; assuming no usable GPU");
    }
    Vec::new()
}

// ---- multi-GPU choice --------------------------------------------------------

/// The default device when the user never picked one: prefer discrete,
/// then the largest VRAM among the preferred kind, else the first device.
pub fn default_gpu_index(gpus: &[GpuDevice]) -> usize {
    let discrete: Vec<&GpuDevice> = gpus.iter().filter(|g| g.kind == "discrete").collect();
    let pool: Vec<&GpuDevice> = if discrete.is_empty() {
        gpus.iter().collect()
    } else {
        discrete
    };
    pool.iter()
        .max_by(|a, b| {
            // largest VRAM wins; ties (and all-zero VRAM) keep the LOWEST
            // index, hence the reversed index tiebreak inside max_by
            (a.vram_bytes, std::cmp::Reverse(a.index))
                .cmp(&(b.vram_bytes, std::cmp::Reverse(b.index)))
        })
        .map(|g| g.index)
        .unwrap_or(0)
}

/// Resolve the configured device (stored as index + name so a hardware
/// change invalidates sanely) against the live list. A stored name that no
/// longer matches falls back to the default; the second return is a log
/// message explaining any deviation from the stored choice.
pub fn resolve_gpu_device(
    stored_index: &str,
    stored_name: &str,
    gpus: &[GpuDevice],
) -> (usize, Option<String>) {
    if gpus.is_empty() {
        return (0, None);
    }
    if stored_name.is_empty() {
        return (default_gpu_index(gpus), None);
    }
    if let Ok(idx) = stored_index.trim().parse::<usize>() {
        if gpus.iter().any(|g| g.index == idx && g.name == stored_name) {
            return (idx, None);
        }
    }
    // Same card, different slot (hardware order changed): follow the name.
    if let Some(g) = gpus.iter().find(|g| g.name == stored_name) {
        return (
            g.index,
            Some(format!(
                "gpu picker: '{stored_name}' moved to device index {}",
                g.index
            )),
        );
    }
    let fallback = default_gpu_index(gpus);
    (
        fallback,
        Some(format!(
            "gpu picker: stored device '{stored_name}' is no longer present; \
             using the default (index {fallback})"
        )),
    )
}

// ---- the policy table --------------------------------------------------------

/// The user's power-mode preference (cfg `device`: auto | cpu | cuda).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerPref {
    Auto,
    ForceCpu,
    ForceGpu,
}

impl PowerPref {
    pub fn from_cfg(dev: &str) -> Self {
        match dev.to_lowercase().as_str() {
            "cpu" => PowerPref::ForceCpu,
            "cuda" => PowerPref::ForceGpu,
            _ => PowerPref::Auto,
        }
    }
}

/// Which config key the serving model comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSlot {
    /// The single `model` key (forced modes, and every mode on a desktop).
    Single,
    /// `model_ac` — the plugged-in model of a laptop's pair.
    Ac,
    /// `model_battery` — the lighter on-battery model of a laptop's pair.
    Battery,
}

/// THE POLICY TABLE (owner-specified, one cell per machine class):
///
/// | class      | machine | Auto behavior                                      |
/// |------------|---------|----------------------------------------------------|
/// | discrete   | laptop  | AC: GPU+model_ac; battery: CPU+model_battery       |
/// |            |         | (kill the worker BEFORE the CPU load — D3cold law) |
/// | discrete   | desktop | always GPU + `model`                               |
/// | integrated | laptop  | GPU ALWAYS (no D3cold prize on an iGPU);           |
/// |            |         | AC: model_ac, battery: model_battery               |
/// | integrated | desktop | always GPU + `model`                               |
/// | none       | laptop  | CPU; AC: model_ac, battery: model_battery          |
/// |            |         | (the battery model still saves CPU watts)          |
/// | none       | desktop | CPU + `model`                                      |
/// | unified    | —       | reserved (treated like integrated for now)        |
///
/// Forced modes (Always CPU / Always GPU) run the single `model` key on
/// every machine class. Pure function of its inputs so every cell is unit-
/// tested; GPU health (`gpu_ok`) and the worker-child mechanics live in
/// `flow::resolve_target` / `flow::load_engine`, not here.
pub fn policy(
    class: GpuClass,
    desktop: bool,
    on_ac: bool,
    pref: PowerPref,
) -> (&'static str, ModelSlot) {
    match pref {
        PowerPref::ForceCpu => ("cpu", ModelSlot::Single),
        // Forced GPU keeps its meaning on every class: attempt the GPU and
        // let the normal failure path fall back (a class-none machine just
        // fails fast and serves CPU honestly).
        PowerPref::ForceGpu => ("gpu", ModelSlot::Single),
        PowerPref::Auto => {
            let slot = if desktop {
                ModelSlot::Single
            } else if on_ac {
                ModelSlot::Ac
            } else {
                ModelSlot::Battery
            };
            let device = match class {
                GpuClass::None => "cpu",
                GpuClass::Integrated | GpuClass::Unified => "gpu",
                GpuClass::Discrete => {
                    if desktop || on_ac {
                        "gpu"
                    } else {
                        // The Blade 14 cell: on battery the worker dies and
                        // the CPU model serves — the dGPU reaches D3cold.
                        "cpu"
                    }
                }
            };
            (device, slot)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(index: usize, name: &str, kind: &str, vram: u64) -> GpuDevice {
        GpuDevice {
            index,
            name: name.into(),
            kind: kind.into(),
            vram_bytes: vram,
        }
    }

    #[test]
    fn classify_covers_all_lists() {
        assert_eq!(classify(&[]), GpuClass::None);
        assert_eq!(
            classify(&[dev(0, "iGPU", "integrated", 0)]),
            GpuClass::Integrated
        );
        assert_eq!(
            classify(&[dev(0, "RTX 2070", "discrete", 8 << 30)]),
            GpuClass::Discrete
        );
        assert_eq!(
            classify(&[
                dev(0, "iGPU", "integrated", 0),
                dev(1, "RTX 2070", "discrete", 8 << 30)
            ]),
            GpuClass::Discrete,
            "any discrete device makes the machine discrete-class"
        );
    }

    /// Every cell of the policy table, exactly as specified.
    #[test]
    fn policy_table_every_cell() {
        use GpuClass::*;
        use ModelSlot::*;
        use PowerPref::*;
        // (class, desktop, on_ac, pref) -> (device, slot)
        let cells: &[(GpuClass, bool, bool, PowerPref, &str, ModelSlot)] = &[
            // discrete + laptop: TODAY'S BEHAVIOR EXACTLY (the Blade 14 cell)
            (Discrete, false, true, Auto, "gpu", Ac),
            (Discrete, false, false, Auto, "cpu", Battery),
            // discrete + desktop: always GPU + single model
            (Discrete, true, true, Auto, "gpu", Single),
            (Discrete, true, false, Auto, "gpu", Single),
            // integrated + laptop: GPU always, model follows the power source
            (Integrated, false, true, Auto, "gpu", Ac),
            (Integrated, false, false, Auto, "gpu", Battery),
            // integrated + desktop
            (Integrated, true, true, Auto, "gpu", Single),
            (Integrated, true, false, Auto, "gpu", Single),
            // none: CPU only everywhere; laptop keeps the two model slots
            (None, false, true, Auto, "cpu", Ac),
            (None, false, false, Auto, "cpu", Battery),
            (None, true, true, Auto, "cpu", Single),
            (None, true, false, Auto, "cpu", Single),
            // unified (future Apple): rides the integrated cells
            (Unified, false, true, Auto, "gpu", Ac),
            (Unified, false, false, Auto, "gpu", Battery),
            (Unified, true, true, Auto, "gpu", Single),
            (Unified, true, false, Auto, "gpu", Single),
        ];
        for &(class, desktop, on_ac, pref, want_dev, want_slot) in cells {
            let (dev, slot) = policy(class, desktop, on_ac, pref);
            assert_eq!(
                (dev, slot),
                (want_dev, want_slot),
                "auto cell {class:?} desktop={desktop} on_ac={on_ac}"
            );
        }
        // Forced modes: single model key on ALL machine classes.
        for class in [None, Integrated, Discrete, Unified] {
            for desktop in [false, true] {
                for on_ac in [false, true] {
                    assert_eq!(
                        policy(class, desktop, on_ac, ForceCpu),
                        ("cpu", Single),
                        "forced-cpu cell {class:?} desktop={desktop} on_ac={on_ac}"
                    );
                    assert_eq!(
                        policy(class, desktop, on_ac, ForceGpu),
                        ("gpu", Single),
                        "forced-gpu cell {class:?} desktop={desktop} on_ac={on_ac}"
                    );
                }
            }
        }
    }

    #[test]
    fn power_pref_parses_cfg_values() {
        assert_eq!(PowerPref::from_cfg("auto"), PowerPref::Auto);
        assert_eq!(PowerPref::from_cfg("cpu"), PowerPref::ForceCpu);
        assert_eq!(PowerPref::from_cfg("cuda"), PowerPref::ForceGpu);
        assert_eq!(PowerPref::from_cfg("CUDA"), PowerPref::ForceGpu);
        assert_eq!(PowerPref::from_cfg("garbage"), PowerPref::Auto);
    }

    #[test]
    fn default_gpu_prefers_discrete_then_vram_then_first() {
        // discrete beats integrated even with less VRAM
        let gpus = vec![
            dev(0, "iGPU", "integrated", 16 << 30),
            dev(1, "RTX", "discrete", 8 << 30),
        ];
        assert_eq!(default_gpu_index(&gpus), 1);
        // among discretes, largest VRAM wins
        let gpus = vec![
            dev(0, "RTX A", "discrete", 8 << 30),
            dev(1, "RTX B", "discrete", 24 << 30),
        ];
        assert_eq!(default_gpu_index(&gpus), 1);
        // all-zero VRAM (driver hides it): first device
        let gpus = vec![dev(0, "A", "discrete", 0), dev(1, "B", "discrete", 0)];
        assert_eq!(default_gpu_index(&gpus), 0);
        // integrated-only machine: the iGPU
        let gpus = vec![dev(0, "iGPU", "integrated", 0)];
        assert_eq!(default_gpu_index(&gpus), 0);
        assert_eq!(default_gpu_index(&[]), 0, "empty list degrades to 0");
    }

    #[test]
    fn resolve_gpu_device_honors_and_invalidates_the_stored_choice() {
        let gpus = vec![
            dev(0, "iGPU", "integrated", 0),
            dev(1, "RTX 2070", "discrete", 8 << 30),
        ];
        // nothing stored -> default (the discrete card), no note
        assert_eq!(resolve_gpu_device("", "", &gpus), (1, None));
        // stored and still matching -> honored exactly, no note
        assert_eq!(resolve_gpu_device("0", "iGPU", &gpus), (0, None));
        // same name at a different index -> follow the name, with a note
        let (idx, note) = resolve_gpu_device("0", "RTX 2070", &gpus);
        assert_eq!(idx, 1);
        assert!(note.is_some(), "index drift is reported");
        // stored card gone -> default, with a note
        let (idx, note) = resolve_gpu_device("2", "GTX 1080", &gpus);
        assert_eq!(idx, 1);
        assert!(note.unwrap().contains("no longer present"));
        // no gpus at all -> harmless 0
        assert_eq!(resolve_gpu_device("1", "RTX 2070", &[]), (0, None));
    }

    #[test]
    fn enum_output_parses_leniently() {
        let devs = vec![dev(0, "RTX 2070", "discrete", 8 << 30)];
        let json = serde_json::to_string(&devs).unwrap();
        assert_eq!(parse_enum_output(&json), devs, "clean line");
        let noisy = format!("driver spew\n{json}\n");
        assert_eq!(parse_enum_output(&noisy), devs, "noise before the line");
        assert_eq!(parse_enum_output(""), Vec::<GpuDevice>::new());
        assert_eq!(parse_enum_output("not json at all"), Vec::new());
        assert_eq!(parse_enum_output("[]"), Vec::new(), "no devices");
    }
}
