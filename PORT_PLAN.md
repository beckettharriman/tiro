# Port plan

Rules: work top to bottom, one task per iteration where practical. A task is
done only when it builds (`cargo build` in `src-tauri`, and `cargo clippy -- -D
warnings` clean) and its "verify" note is satisfied. Check it off and commit in
the same iteration. If blocked, record the blocker under **Blockers** at the
bottom and move on. Behavior spec lives in `PORTING_NOTES.md`; ground truth is
the original app at `C:\Users\becke\voice-clipboard` (read-only).

## Phase 0 — scaffold

- [x] **0.1 Scaffold Tauri 2 app.** `npm create tauri-app@latest` (vanilla JS
  template, app name `tiro`, identifier `dev.tiro.app`) in this repo, laid out
  as `ui/` (frontend) + `src-tauri/`. No frontend framework, no bundler
  (Tauri's plain static-dir mode). Verify: `cargo tauri dev` opens a window on
  Windows.
- [x] **0.2 Import original UI.** Copy `ui/index.html`, `ui/pill.html`,
  `ui/app.js`, `ui/styles.css` from the original repo. Add `ui/bridge.js`
  implementing the pywebview-shaped API (`window.pywebview.api.*` +
  `pywebviewready` event) over Tauri `invoke`/`listen`, with stub commands in
  Rust so the panel renders with dummy state. Verify: panel window shows the
  real UI, settings page slides, no console errors.
- [x] **0.3 Two windows.** Configure `panel` (400×560) and `pill` (300×72):
  frameless, transparent, fixed-size, skip-taskbar, hidden at start; pill
  always-on-top. Verify: both windows can be shown/hidden from Rust, look
  right, no taskbar buttons.

## Phase 1 — core backend (CPU-only path first)

- [x] **1.1 Config.** `config.ini` load/save with the exact keys/defaults from
  PORTING_NOTES §2, atomic write (temp + rename), missing-key backfill.
  Include `config.example.ini`. Unit tests for defaults + round-trip.
- [ ] **1.2 Audio capture.** cpal input stream: device enumeration,
  `mic_name` substring match, native-rate capture with 48k/44.1k/16k fallback,
  f32 mono frames into a buffer, resample to 16 kHz, <0.3 s rejection.
  Verify with a temporary test command that records 2 s and logs sample count.
- [ ] **1.3 Sound cues.** Synthesize the six cues exactly per PORTING_NOTES §1
  (freqs/durations/volumes, 6 ms attack, cosine release, 44.1 kHz), play
  non-blocking via cpal/rodio, `sound_volume` clamp 0.0–1.5, `beeps` toggle.
- [ ] **1.4 Clipboard + cleanup.** arboard copy; port the `light`/`fillers`
  cleanup rules exactly (unit tests: casing, trailing period, standalone i,
  space-before-punctuation, filler stripping, empty-result fallback).
- [ ] **1.5 Transcription (CPU).** whisper-rs: model manager that maps
  `base.en`/`small.en`(/`medium.en`) to GGUF files under `./models`, downloads
  from huggingface ggerganov/whisper.cpp on first use with progress logged;
  transcribe 16 kHz f32 with language=en, beam 1 on CPU, VAD enabled, vocab
  from `vocab.txt` when `use_vocab_bias`; join+strip segments. Warm-up on 1 s
  silence. Verify: feed a recorded WAV, get sane text.
- [ ] **1.6 Transcript store.** Daily JSONL + Markdown files in `vault_dir`
  with fallback to app-local `logs/` when unwritable (match original file
  names/format — read the original's log-writing code first), entry ids
  `<iso>|<hash>`, 200-entry day list for the panel.

## Phase 2 — wire the app together

- [ ] **2.1 Recording state machine.** toggle/cancel semantics, session ids,
  stale-finish protection, the full flow from PORTING_NOTES §1 including pill
  states, beeps, engine chip updates, verbatim-vs-clean split.
- [ ] **2.2 Bridge, for real.** Implement every JS→backend method and
  backend→JS push from PORTING_NOTES §3 (real state snapshot, `set_setting`
  side effects, `list_mics`, `pick_folder` via tauri dialog plugin,
  `set_pin`, `close_panel`, `begin_drag` → `start_dragging`). Verify: panel
  fully functional against the real backend.
- [ ] **2.3 Global hotkeys.** Register the three hotkeys
  (tauri-plugin-global-shortcut), dispatch to toggle/panel/cancel, and
  `rebind_shortcut` with validation + persist + live re-register.
- [ ] **2.4 Window placement.** Active-monitor detection (monitor with focus,
  else monitor under cursor), work-area math: pill bottom-center ~110 px up,
  panel centered until user drags it; pin/summon behavior.
- [ ] **2.5 Theme + transparency.** system/light/dark via Tauri theme events,
  `tiroSetTheme` push, `panel_transparency` mapping 0–100 → 0.95–0.30.

## Phase 3 — platform integration

- [ ] **3.1 Single instance + tray.** tauri-plugin-single-instance (second
  launch summons panel); tray icon: left-click toggles panel, right-click menu
  (Open, Start/Stop dictation, Restart, Quit).
- [ ] **3.2 Autostart.** tauri-plugin-autostart wired to the `launchAtLogin`
  setting on both OSes.
- [ ] **3.3 Power detection.** AC/battery: `GetSystemPowerStatus` on Windows
  (via `windows` crate) and `/sys/class/power_supply` on Linux (or
  starship-battery if it's cleaner); 20 s watcher; battery default when
  unknown.
- [ ] **3.4 GPU worker.** `tiro --gpu-worker` subcommand: whisper-rs with
  Vulkan feature, framed stdin/stdout protocol, readiness line, timeouts,
  EOF-exit orphan safety (PORTING_NOTES §6 — read POWER_AND_DGPU.md in the
  original first; the main process must NEVER initialize a GPU context).
  Feature-gate so plain `cargo build` works without the Vulkan SDK.
- [ ] **3.5 Device orchestration.** `resolve_target()` semantics (auto/cpu/gpu,
  600 s failure latch + re-probe), power-flip handling (kill worker BEFORE CPU
  load on AC→battery), GPU-crash → CPU retry of the same audio, engine chip
  truthfulness, beam 5 on GPU / 1 on CPU.

## Phase 4 — cross-OS verification

(Development is Linux-native in WSL2, so the Linux build/runtime is exercised
continuously from Phase 0 onward. This phase closes the gaps on the other
side and the hard platform corners.)

- [ ] **4.1 Windows build.** Pull the repo in the Windows clone
  (`C:\Users\becke\tiro`), get `cargo build` + `cargo tauri dev` green there.
  Fix portability fallout. Document build prerequisites for both OSes.
- [ ] **4.2 Windows runtime parity.** On Windows: hotkeys, tray, transparent
  frameless windows, taskbar suppression, audio capture, clipboard, power
  detection, autostart — all verified against PORTING_NOTES.
- [ ] **4.3 Hotkeys on Wayland.** Global shortcuts work on X11/WSLg today;
  implement/document the Wayland story (portal GlobalShortcuts where
  available; DE-level shortcut → `tiro --toggle` CLI fallback wired through
  the single-instance IPC).
- [ ] **4.4 Platform features cross-check.** Theme detection, power
  detection, autostart on both OSes; note WSLg-specific quirks separately
  from real-Linux issues.

## Phase 5 — parity, packaging, release prep

- [ ] **5.1 Parity audit.** Walk PORTING_NOTES top to bottom against the
  running app; fix gaps; record intentional deviations at the bottom of
  PORTING_NOTES.
- [ ] **5.2 Packaging.** `cargo tauri build` installers: NSIS/MSI on Windows,
  .deb + AppImage on Linux. App icon (port tiro.ico, add Linux sizes).
- [ ] **5.3 Docs.** Rewrite README.md for the new app (install, build, usage,
  hotkeys, offline/privacy story, Wayland notes). LICENSE carried over.
  `vocab.example.txt` + `config.example.ini` included.
- [ ] **5.4 Pre-release hygiene sweep.** Run the attribution greps from
  CLAUDE.md over full history and tree; scrub logs/temp files; verify
  .gitignore covers runtime artifacts; confirm repo is clean for an eventual
  public flip (stays private until owner says otherwise).

## Blockers

(none yet)
