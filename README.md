<div align="center">

# Tiro

### Dictate anywhere — offline, always-on, system-wide voice-to-clipboard.

**Press a hotkey, talk, press again. Your words land on the clipboard — never auto-pasted — so you can keep clicking around the screen while you speak.** No console, no taskbar button. It just runs. Fully offline: your voice never leaves your machine.

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
![Platform: Windows | Linux](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-0078D6.svg)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust%20%2B%20Tauri-orange.svg)
![Offline](https://img.shields.io/badge/100%25-offline-30d158.svg)

<!-- TODO(launch): drop docs/demo.gif + docs/panel.png here and embed below. -->
_A panel screenshot and a short dictation GIF go here at launch._

</div>

---

## The problem it solves

Most dictation tools type into **the box you're focused on**. The moment you click away, recognition stops — they're tied to a cursor or a text field. But sometimes you want to **talk while you work the screen**: speak a thought, then keep clicking, scrolling, and selecting freely while the transcript waits for you.

Tiro does exactly that. It listens on a **global hotkey that works in any application**, transcribes locally with Whisper, and drops the text on your **clipboard** instead of typing it. You paste it wherever and whenever you want. It's hands-free, system-wide, push-to-talk dictation that never steals your focus.

## Features

- **🎙️ One global hotkey, anywhere** — `Ctrl+Alt+Space` toggles dictation in any app, fullscreen game, or remote session. No focused text box required.
- **📋 Lands on your clipboard, never auto-pasted** — keep clicking, dragging, and navigating while you talk; paste the result on your terms.
- **🔌 100% offline** — transcription runs locally via [whisper.cpp](https://github.com/ggerganov/whisper.cpp). After the one-time model download, no internet, no accounts, no telemetry.
- **👻 Invisible by design** — a summonable panel, a small "Recording" pill, and a quiet tray icon are the only UI, and only when you want them.
- **🔋 Battery-aware** — on laptops with a discrete GPU, Tiro transcribes on the CPU while on battery and only spins up the GPU when you're plugged in (configurable). GPU inference lives in a disposable child process so the discrete GPU can fully power down the moment you unplug.
- **🧠 Smart vocabulary** — prime Whisper with your names, jargon, and slang so it spells them right on capture.
- **📝 Verbatim transcript log** — every utterance is saved to a daily JSONL + Markdown file you own, while a lightly-cleaned copy goes to the clipboard.
- **⌨️ Everything is configurable** — rebind hotkeys, pick your mic, choose models, theme, transparency, and more from an in-app settings panel.

## How it works

```
  hotkey ─▶ record mic ─▶ whisper.cpp (local) ─▶ clipboard
                                  │
                                  └─▶ verbatim log (JSONL + Markdown)
```

A single Rust process (built on [Tauri 2](https://tauri.app)) registers the global hotkeys and renders two frameless, transparent webview windows: a settings/log **panel** and a floating **pill**. Audio is captured with WASAPI (Windows) or ALSA (Linux), transcribed by whisper.cpp on CPU — or on the GPU via Vulkan in a separate worker process — copied to the clipboard, and logged to disk. A single-instance guard makes a second launch summon the running app instead.

## Requirements

- **Windows 10/11** (WebView2 Runtime — preinstalled on 11) or **Linux** (WebKitGTK 4.1; X11 session for global hotkeys, see the Wayland note below)
- A microphone
- *(Optional)* a Vulkan-capable GPU for faster, more accurate transcription when plugged in (requires a build with `--features gpu`)

## Install

Grab an installer from the releases page — `tiro_*_x64-setup.exe` (or the `.msi`) on Windows, the `.deb` or `.AppImage` on Linux — or build from source with plain `cargo`: see **[BUILDING.md](BUILDING.md)**.

The first time you dictate, Tiro downloads the Whisper model into `./models` (≈80 MB for `base.en`, ≈250 MB for `small.en`, quantized GGUF). That download is the only time Tiro needs the internet — everything after is offline.

Copy the samples if you want to customize before first launch:

```sh
cp config.example.ini config.ini
cp vocab.example.txt vocab.txt
```

*(Tiro also creates a default `config.ini` automatically on first run.)*

**Start at login:** toggle **Settings → Launch at login** in the panel.

## Hotkeys

| Shortcut | Action |
|---|---|
| `Ctrl+Alt+Space` | Toggle dictation — talk, then press again to transcribe to the clipboard. Hold it instead for push-to-talk: recording stops when you let go |
| `Ctrl+Alt+V` | Paste-mode toggle — press to start a take, press again to stop and paste the text at your cursor (it is copied to the clipboard too). The finishing key decides: end a take with `Space` and it stays clipboard-only; end it with `V` and it also pastes |
| `Ctrl+Alt+C` | Open / close the Tiro panel |
| `Ctrl+Alt+X` | Cancel the current recording without transcribing |

All four are rebindable in **Settings → Shortcuts**. Everything still lands
on the clipboard exactly as before — paste-at-cursor is an extra delivery,
not a replacement. If the keystroke can't be injected, the text simply stays
on the clipboard and the pill says so.

### Wayland

Wayland compositors don't let apps grab global keys. Bind desktop-level
shortcuts to Tiro's own CLI instead — a second launch remote-controls the
running instance:

```
tiro --toggle   # start/stop dictation
tiro --paste    # paste the take at the cursor
tiro --panel    # show/hide the panel
tiro --cancel   # cancel the current recording
```

GNOME: Settings → Keyboard → Custom Shortcuts; KDE: System Settings →
Shortcuts; sway/hyprland: `bindsym`/`bind`. Details in
[BUILDING.md](BUILDING.md).

Paste-at-cursor on Wayland goes through the desktop's remote-desktop
permission portal: the first paste pops a one-time "allow remote input"
dialog (on KDE: allow and choose to remember). Tiro stores the permission
token so the dialog doesn't come back.

## The panel (`Ctrl+Alt+C`)

A frameless, draggable panel summoned on top of whatever you're doing:

- **Recent transcriptions** (today) — click any line to copy it again.
- **Microphone picker** and a **record button** (same as the hotkey).
- **Pin** to keep it above other windows; **drag** the header to reposition it.
- A live **engine chip** showing the current model, device (CPU/GPU), and power source.
- A full **Settings** page: engine/power mode, models, sound cues, clipboard cleanup, smart vocabulary, microphone, shortcuts, save location, theme, transparency, and launch-at-login.

There is also a tray icon: left-click toggles the panel; right-click offers Open, Start/Stop dictation, Restart, and Quit.

## Where transcripts go

By default, verbatim transcripts are written to **`Documents/Tiro`** (changeable in **Settings → Storage**):

```
Documents/Tiro/YYYY-MM-DD.jsonl   # structured: verbatim text + metadata
Documents/Tiro/YYYY-MM-DD.md      # readable: one line per utterance
```

If that folder is ever unwritable, Tiro falls back to a `logs/` folder next to the app and tells you so in the panel.

## Smart vocabulary

Drop the names, jargon, and slang you use into **`vocab.txt`** (comma- or newline-separated; `#` lines are ignored — see `vocab.example.txt`). Tiro feeds them to Whisper as a hint so it spells them correctly at capture. Toggle it with **Settings → Smart vocabulary**.

## Privacy

Everything runs on your machine. The only network request Tiro ever makes is the one-time model download from Hugging Face on first use. There are no accounts, no telemetry, no cloud transcription — unplug the network after the model download and nothing changes.

## The name

Tiro is named for **Marcus Tullius Tiro**, Cicero's secretary, who invented a system of shorthand (the *notae Tironianae*) to write down speech as fast as it was spoken — arguably history's first real-time dictation system. Two thousand years later, this one runs on Whisper.

## Contributing

Issues and PRs are welcome. This is a personal tool built to solve a real need; if it solves yours too, improvements that keep it lean, offline, and invisible-by-default are the ones most likely to be merged.

## License

[GNU General Public License v3.0](LICENSE) © Beckett Harriman.
