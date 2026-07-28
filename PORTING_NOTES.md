# Tiro porting spec — complete behavioral inventory of the original

Source of truth: the Python original at `C:\Users\becke\voice-clipboard`
(read-only reference). This document is the behavioral contract the port must
satisfy. Where the port intentionally deviates, the deviation is called out in
**PORT:** notes.

## 1. Feature list

### Global hotkeys (original: Win32 RegisterHotKey)
- **`Ctrl+Alt+Space`** (`dictation_hotkey`) — toggle recording: press to start, press again to stop and transcribe to clipboard
- **`Ctrl+Alt+V`** (`panel_hotkey`) — toggle panel visibility (settings & transcript log)
- **`Ctrl+Alt+X`** (`cancel_hotkey`) — cancel current recording, or in-flight transcription, without writing to log
- All three are rebindable from Settings with a key-capture UI

### Recording flow
1. Hotkey → start recording (audio stream on chosen mic)
2. "start" beep plays (C5→G5 rising)
3. Pill shows "Recording" with 14-bar live waveform animation
4. Panel record button flips to "Stop" state
5. Second hotkey press: stop stream, collect frames, hide pill
6. If audio < 0.3 s: play cancel beep, discard (no empty takes)
7. Otherwise: transcribe on worker thread; pill shows "Transcribing" (spinner + shimmer label)
8. Apply clipboard cleanup, copy cleaned text to clipboard, write verbatim to log
9. Success: "done" beep, pill "Copied to clipboard" + green check, auto-hide after 1.1 s
10. Error: pill "error" with amber warning glyph + message, auto-hide after 2 s

Data-loss guarantees to preserve:
- The verbatim transcript is ALWAYS logged; cleanup only affects the clipboard copy (falls back to verbatim if cleanup yields empty string).
- A **session id** per take prevents a stale transcription finishing late from clobbering a newer one.
- If the GPU worker crashes mid-transcription: latch GPU off, load CPU model, retry the SAME audio — a take is never lost.
- Cancel flag aborts before copy/log.

### Pill (floating recording indicator)
- 300×72 px, frameless, transparent, always-on-top, bottom-centered ~110 px above taskbar on the ACTIVE monitor
- States: `recording` (pulsing red dot + waveform + label), `transcribing` (spinner + shimmer), `done` (green check, "Copied to clipboard", aria-live=polite), `error` (amber circled-!, message, aria-live=assertive), `off`
- Entry animation `.pill.in` ~260 ms ease-out; exit `.pill.out` ~260 ms ease-in, window hidden after 320 ms timeout

### Panel (settings & transcript log)
- 400×560 px, frameless, transparent, NOT on-top unless pinned; hidden at startup
- Header: "Recent transcriptions:" + Pin button (toggles on-top) + Close; draggable via header
- Mic selector (native `<select>` overlaid on styled pill) + Record button (mic/stop icon toggle)
- Today's log: scrollable, up to 200 entries; each entry shows 12-hour clock time, duration M:SS, text clamped ~3 lines with Show more/less, click-to-copy with "Copied" badge 1.5 s
- Empty state: rendered hotkey keycaps + "Press the hotkey anywhere to start dictating"
- Footer: live engine chip (model + CPU/GPU + plugged/battery) + settings gear
- Settings page slides in from the right:
  - Engine: "Now running" row, Power mode (Auto/CPU/GPU), Model on battery, Model when plugged
  - Audio: sound cues toggle, volume slider 0–100 (internally 0.0–1.5)
  - Capture: recording pill toggle, Clipboard cleanup (Off/Light/+Fillers), Smart vocabulary toggle
  - Input: microphone selector
  - Shortcuts: rebindable hotkeys, key-capture UI (Esc cancels)
  - Storage: save path + folder picker, fallback banner when vault unwritable
  - Appearance: theme Light/Dark/System, transparency slider 0–100 (→ opacity 0.95–0.30)
  - System: launch-at-login toggle

### Clipboard text cleanup (clipboard only; verbatim always logged)
- `light` (default): collapse whitespace; trim; capitalize first letter (unless already upper); append `.` if no trailing `.!?`; standalone `i` → `I` (incl. i'm/i'll/i've); fix space-before-punctuation (` ,` → `,`)
- `fillers`: light + strip `\b(um+|uh+|erm+|hmm+)\b[,]?\s*` case-insensitive
- `none`: verbatim

### Sound cues (synthesized sine waves, 44.1 kHz, non-blocking, 6 ms attack + cosine release; volume × `sound_volume` clamped 0.0–1.5)
- `start`: C5 523.25 Hz 55 ms (0.16) + G5 783.99 Hz 75 ms (0.18) — rising
- `stop`: G5 55 ms (0.15) + D5 587.33 Hz 85 ms (0.15) — settling
- `done`: E5+B5 chord 659.25+987.77 Hz 180 ms (0.17) — soft bell, no gap
- `cancel`: G4 392.00 Hz 70 ms (0.11) + Eb4 311.13 Hz 110 ms (0.09) — low fall
- `copy`: C6 1046.50 Hz 45 ms (0.13) — tiny tick, no gap
- `error`: A4 440.00 Hz 70 ms (0.15) + E4 329.63 Hz 120 ms (0.15) — calm attention
- multi-note cues insert a 12 ms silence gap after each note

### Single instance
- Original: bind localhost:53117 + `tiro.pid` with stale-owner reclaim (probe, verify pid is a live pythonw running main.py, terminate, retry bind 15×200 ms).
- **PORT:** use `tauri-plugin-single-instance`; second launch should summon the panel of the running instance. Port/pid machinery not needed.

### Theme
- `theme` = system|light|dark. System detection on Windows: registry `HKCU\...\Themes\Personalize\AppsUseLightTheme`; polled every 20 s; pushes `tiroSetTheme("light"|"dark")` on change.
- **PORT:** use Tauri's theme API/event on both OSes.

### Autostart
- Original: Startup-folder `Tiro.lnk` → base `pythonw.exe main.py`, WindowStyle 7.
- **PORT:** `tauri-plugin-autostart`.

### UI watchdog & auto-restart
- Original probes `evaluate_js("1")` every 15 s (4 s timeout); after 3 consecutive failures and guard checks (not recording/transcribing, panel hidden, `auto_restart` on, ≤3 restarts per 600 s persisted in `tiro_restarts.json`) respawns itself.
- **PORT:** this was a workaround for pywebview/WebView2 event-loop hangs. Do NOT port initially; keep the `auto_restart` config key parsed-but-inert, revisit only if Tauri exhibits hangs.

### System tray
- Original: raw Win32 Shell_NotifyIconW; left-click toggles panel, right-click menu; re-adds icon on Explorer restart ("TaskbarCreated").
- **PORT:** Tauri tray API (`TrayIconBuilder`); same behaviors.

## 2. Config (`config.ini`, all keys in `[general]`, rewritten atomically via temp file + rename)

| Key | Default | Purpose |
|-----|---------|---------|
| `dictation_hotkey` | `ctrl+alt+space` | toggle recording ("+"-joined) |
| `panel_hotkey` | `ctrl+alt+v` | toggle panel |
| `cancel_hotkey` | `ctrl+alt+x` | cancel recording/transcription |
| `device` | `auto` | `auto` (GPU on AC, CPU on battery) / `cpu` / `cuda` — **PORT:** treat `cuda` as generic `gpu` while accepting the old string |
| `compute_type` | `int8` | CPU quantization (GPU float16) — **PORT:** maps to GGUF quantized model choice |
| `model_battery` | `base.en` | model for CPU / on battery |
| `model_ac` | `small.en` | model for GPU / plugged in |
| `model` | `base.en` | legacy fallback |
| `mic_name` | `` | case-insensitive substring match of input device (empty = first) |
| `beeps` | `true` | sound cues |
| `sound_volume` | `1.0` | 0.0–1.5 clamped |
| `pill` | `true` | show pill |
| `clipboard_cleanup` | `light` | `none`/`light`/`fillers` |
| `use_vocab_bias` | `true` | prime Whisper with `vocab.txt` (skip blanks and `#` comments) |
| `theme` | `system` | `system`/`light`/`dark` |
| `panel_transparency` | `45` | 0–100 → alpha 0.95–0.30 |
| `vault_dir` | `~/Documents/Tiro` | transcript folder |
| `fallback_dir` | `logs` (app-relative) | fallback if vault unwritable |
| `auto_restart` | `true` | watchdog kill-switch (**PORT:** parsed, inert) |

## 3. JS ↔ backend bridge

Original: pywebview `window.pywebview.api.*` + `evaluate_js`. **PORT:** write a small
`ui/bridge.js` shim exposing the SAME function names over Tauri `invoke`/events so
`app.js`/`pill.html` stay near-identical.

### Backend → JS pushes
| Call | Payload |
|------|---------|
| `tiroApplyState(state)` | `{ entries, settings, engine, mics, shortcuts, theme, effectiveTheme }` — full snapshot, re-renders UI |
| `tiroAddEntry(entry)` | `{ id, clock, dur, text, device, model, mic, fresh: true }` — prepend with `.fresh` animation |
| `tiroSetEngine(engine)` | `{ model, device, power }` |
| `tiroSetTheme(effective)` | `"light" \| "dark"` |
| `tiroSetRecording(on)` | bool — record button state |
| `tiroSetStorage(obj)` | `{ fallback: bool, path }` |
| `pillSet(state, payload?)` | pill state machine; payload = error message for `error` |

### JS → backend methods (all async)
| Method | Args → Returns |
|--------|----------------|
| `get_state()` | → full state snapshot |
| `copy_text(text)` | copies + plays `copy` cue |
| `set_setting(key, value)` | → `{ ok, engine, theme, effectiveTheme, launchAtLogin }` |
| `list_mics()` | → `[string]` |
| `toggle_record()` / `cancel_record()` | |
| `set_pin(on)` / `close_panel()` / `begin_drag()` | window controls |
| `pick_folder()` | → `{ path }` or null |
| `rebind_shortcut(which, combo)` | `which: dictate\|panel\|cancel`, `combo: {ctrl,alt,shift,meta,code,keys}` → `{ ok, keys }` |

### Shapes
Entry: `{ id: "<iso ts>|<hash>", clock: "3:42 PM", dur: "0:08", text, device: "GPU"|"CPU", model, mic }`
Settings: `{ powerMode: auto|cpu|gpu, modelBattery, modelPlugged, soundCues, volume 0-100, recordingPill, clipboardCleanup: off|light|fillers, smartVocab, micName, launchAtLogin, savePath, transparency 0-100, storageFallback, storagePath }`
Shortcut: `{ ctrl, alt, shift, meta, code: "Space", keys: ["Ctrl","Alt","Space"] }` per `dictate`/`panel`/`cancel`

## 4. Audio pipeline
- 16 kHz mono f32 target; capture tries native rate then 48k/44.1k/16k
- Device enumeration with host-API preference; `mic_name` substring match, fallback first device
- Callback appends frames while recording; < 0.3 s rejected
- Linear-interpolation resample to 16 kHz when needed
- **PORT:** `cpal`; replicate rate-fallback and substring matching; resample with `rubato` or linear interp

## 5. Transcription
- Original: faster-whisper. CPU: in-process, `int8`, `beam_size=1`, `vad_filter=True`, `language="en"`, vocab via `hotwords` (fallback `initial_prompt`). GPU: worker child, `beam_size=5`, float16.
- Warm-up: transcribe 1 s of silence on first load to verify device
- GPU failure latches `_cuda_ok=False`; re-probe allowed after 600 s
- Output: join segment texts with spaces, strip; cleanup for clipboard; verbatim for log
- **PORT:** `whisper-rs` (whisper.cpp), GGUF models (`ggml-base.en.bin`, `ggml-small.en.bin`, quantized variants for the `compute_type` mapping) auto-downloaded to `./models` from huggingface.co/ggerganov/whisper.cpp on first use. Whisper.cpp has integrated Silero VAD — use it to mirror `vad_filter`. Vocab → `initial_prompt` equivalent. Existing CTranslate2 model folders are NOT reusable.

## 6. GPU / power logic — CRITICAL DESIGN, MUST PRESERVE
Why the child process exists (from POWER_AND_DGPU.md): the first CUDA touch
creates a driver context that lives until process exit and keeps the discrete
GPU out of D3cold (~7 W idle drain). The main process must NEVER touch the GPU.
This applies to Vulkan contexts too, so the port keeps the architecture:

- GPU inference runs in a child process — **PORT:** re-exec our own binary with a `--gpu-worker` subcommand (no separate script)
- Framed stdin/stdout protocol (original, keep the same idea): startup readiness JSON line `{"ready":true,...}`/`{"ready":false,"error":...}`; request = `u32 le` header-len + header JSON `{"samples","beam","vocab","language"}` + f32-le PCM @16 kHz; response = `u32 le` len + JSON `{"ok":true,"segments":[...]}` or `{"ok":false,"error"}`; EOF on stdin → clean exit (orphan safety)
- Spawn timeout 30 s (cached model) / 120 s (downloading); per-request timeout 2× realtime + 30 s; stop = close stdin, terminate after 3 s, then kill; atexit safety net
- Power semantics: `resolve_target()` — `cpu`→cpu; `gpu`→gpu if not latched-off; `auto`→gpu iff GPU ok AND on AC
- Power watcher: 20 s poll; on AC→battery flip kill worker FIRST then load CPU model (lets dGPU sleep in seconds); on battery→AC spawn worker
- Battery detection: Windows `GetSystemPowerStatus` (ACLineStatus); **PORT:** Linux = `/sys/class/power_supply/*/online` (AC adapters) with "assume battery" as safe default; consider `starship-battery` crate
- Engine chip reports the ACTUAL device (GPU only while worker alive)

## 7. Windowing details
- Panel 400×560 / pill 300×72; both frameless, transparent, fixed-size; opaque fallback colors #1E1E21 (panel) / #1C1C1E (pill)
- Taskbar suppression (original: WS_EX_TOOLWINDOW) → **PORT:** `skip_taskbar(true)`
- DWM rounded-corner clipping (Win11) → **PORT:** Tauri handles transparency; CSS radius 14 px does the rest
- Placement on the ACTIVE monitor (monitor of foreground window, else monitor of cursor), work-area aware; pill bottom-centered ~110 px up; panel centered unless the user has dragged it this session (`_panel_moved` disables auto-centering)
- Placement retried ~6× over 300 ms after show (races the async show) — may be unnecessary in Tauri; verify
- Pin = toggle always-on-top; summon = briefly topmost + focus
- Panel drag via `begin_drag()` → **PORT:** Tauri `start_dragging()`

## 8. Startup / shutdown
- Startup: single-instance → config load (backfill missing keys) → create both windows hidden → start UI → then: spawn device boot (load model per `resolve_target()`), power watcher (20 s), tray, hotkeys
- Shutdown: stop GPU worker, remove tray, exit; never block exit on cleanup
- Logs: `tiro.log` (timestamped app log; original also uses faulthandler), `gpu_worker.log` (child), daily transcripts JSONL + Markdown in vault dir with fallback to `logs/`
- **PORT:** `tracing` + `tracing-appender` for logs; keep same transcript file formats and naming as original (inspect original `write_log_entry` when implementing)

## 9. UI visual details worth preserving
- Panel: rgba(30,30,33,α) bg, backdrop blur 34 px + saturate 180%, 0.5 px border rgba(255,255,255,.09), 14 px radius, system-ui font stack, 4/8/12/16/20/24/32 px spacing scale
- Text levels txt-1..txt-4; accent #8E8E93; ok #30d158; warn #e6a23c (calm, not loud)
- Pill: capsule (980 px radius), rgba(28,28,30,.82) dark / rgba(252,252,254,.86) light, 14 animated waveform bars (sine + jitter)
- Entry `.fresh` entrance animation settles 600 ms; copy feedback `.copied` chip 1.5 s; clamped text with overflow detection (`scrollHeight > clientHeight`); transparency slider live-previews, commits on release; settings pages slide via `style.left`; list scrollTop preserved across copy re-render; aria attributes throughout
- **The `ui/` folder is copied from the original nearly verbatim; only the bridge shim changes.**

## Out of scope for the port
- `transcribe_video.py` / `diarize_video.py` — standalone side utilities, not part of the app. Ignore.
- The `.bat`/`.vbs`/`.ps1`/`.lnk` launcher scripts — replaced by a real packaged app + autostart plugin.

## Port deviations (recorded during the parity audit, task 5.1)

Beyond the **PORT:** notes above, the shipped port intentionally deviates
from the original in these ways:

- **Engine**: whisper.cpp (whisper-rs) with GGUF models replaces
  faster-whisper/CTranslate2; the GPU backend is Vulkan, not CUDA.
  `device = cuda` is still accepted in config and means "the GPU".
- **Linux theme detection** does not use the windowing layer's theme API:
  tao latches the boot-time portal value and delivers OS ThemeChanged
  events with a dummy window id that never reaches handlers. Instead the
  XDG Settings portal is read directly and the 20 s watcher pushes
  changes — behaviorally identical to the original's registry poll.
  Windows keeps the native event path.
- **Logging**: no `tracing` stack. Release builds redirect stderr to
  `tiro.log` (start-banner + everything the app prints; on Unix this also
  captures whisper.cpp's C-level output). Dev builds log to the terminal.
  The original's `faulthandler` has no equivalent.
- **Wayland hotkeys**: the shortcut plugin has no Wayland backend, so the
  documented setup is a DE-level shortcut bound to `tiro --toggle` /
  `--panel` / `--cancel`, forwarded to the running instance over the
  single-instance IPC (see BUILDING.md). The GlobalShortcuts portal is a
  future enhancement.
- **Panel drag tracking**: `start_dragging()` has no end-of-drag
  callback, so "user moved the panel" is detected as drift from the last
  position the app set (>3 px) at the next summon — same observable
  behavior as the original's drag-loop flag.
- **Active monitor** = the monitor under the cursor (falling back to the
  panel's monitor, then primary). The original preferred the foreground
  window's monitor — a Win32-only signal whose own fallback was the
  cursor.
- **GPU worker responses** carry one pre-joined segment string in the
  `segments` array rather than per-segment texts; the parent joins
  identically either way, so the wire shape is unchanged.
- **Hotkey validation** is structural (modifiers + one known key) rather
  than a Win32 VK-map lookup; the OS-level registration remains the real
  arbiter and failures are reported per hotkey.
- **`sound_volume`** is written in Python float repr ("0.5", "1.0") so a
  config.ini carried over from the original stays byte-compatible.
- **Tray**: the Windows icon lands in the taskbar overflow flyout by
  default (OS behavior); Explorer-restart re-add is handled by the tray
  library rather than a hand-rolled TaskbarCreated hook.
- **Pill window height** is 300x88 (not the original 300x72): the pill layout
  (44 px pill + 38 px bottom dock inset) needs 82 px, so the original clipped
  the pill's top 10 px — a latent bug there. The extra height is transparent
  headroom; the pill's on-screen position and size are unchanged (placement
  anchors the window's bottom edge).
