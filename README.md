# Tiro

Press a hotkey, talk, press it again. What you said is on your clipboard. Nothing ever leaves your computer.

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
![Platform: Windows | Linux](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-0078D6.svg)
![Rust + Tauri](https://img.shields.io/badge/built%20with-Rust%20%2B%20Tauri-orange.svg)
![Offline](https://img.shields.io/badge/100%25-offline-30d158.svg)

<!-- TODO(launch): docs/demo.gif + docs/panel.png -->

## Why I made this

Every dictation tool I tried types into whatever box you're focused on. The second you click somewhere else it either stops or dumps half a sentence into the wrong window. That's not how I work. I talk while I'm doing other stuff — scrolling, clicking around, switching apps — and I want the words waiting for me when I'm done instead of fighting me for the cursor.

So Tiro doesn't type. It records, transcribes locally with whisper.cpp, and puts the text on your clipboard. You paste it when you want it, wherever you want it. No window you have to be in, no account, no internet.

I mostly use it for journaling and for talking at agents instead of typing paragraphs at them.

## Hotkeys

| Key | What it does |
|---|---|
| `Ctrl+Alt+Space` | Start dictating. Press again to stop and copy. |
| `Ctrl+Alt+V` | Same thing, but it also pastes at your cursor when you stop. |
| `Ctrl+Alt+C` | Show / hide the panel. |
| `Ctrl+Alt+X` | Throw away the current recording. |

Hold either of the first two instead of tapping and you get push-to-talk — it stops when you let go. The key you *finish* on decides delivery: end on Space and it stays clipboard-only, end on V and it also pastes. If the paste can't be injected for some reason, the text is still on your clipboard and the pill says so.

All four are rebindable in Settings → Shortcuts.

## There is no time limit

Talk for as long as you want. There used to be a ten minute cap, and one day it silently ate the back half of a thirty minute journal entry — the pill kept animating, the timer kept counting, and the mic had been dead for twenty minutes. That's the worst thing this app can possibly do, so the cap is gone entirely.

The only real limit now is memory, about 700 MB per hour of talking, and Tiro refuses to open null/dummy input devices, which is what the cap was actually guarding against in the first place.

## Offline

The only time Tiro touches the network is downloading a Whisper model, and only when you ask it to. After that you can pull the ethernet cable out of the wall and nothing changes. No accounts, no telemetry, no cloud anything.

## The panel

`Ctrl+Alt+C` summons a small frameless glass panel over whatever you're doing. Drag the header to move it, pin it to keep it on top, close it and it's gone again.

Compact, it's a mic picker, a record button, and today's transcripts. Click any line to copy it again.

The expand button widens it into four pages:

- **Transcribe** — everything you've dictated, newest first, with a search box and a day pager. It opens on today and streams older days in as you scroll back, keeping about five days in view at a time so it doesn't get heavy after a year of use.
- **Settings** — engine, audio, capture, input, shortcuts, storage, appearance, launch at login.
- **Vocabulary** — hot words and corrections (below).
- **Models** — what's installed and what you can download.

There's a tray icon too. Left click toggles the panel; right click gives you Open, Start/Stop dictation, Restart, and Quit.

## Engine and GPU

Tiro figures out what kind of machine it's on once at startup and only shows you the settings that actually mean something on it.

- **Laptop with a discrete GPU** — Auto Switch. GPU and the bigger model when you're plugged in, CPU and the lighter model on battery.
- **Laptop with integrated graphics** — stays on the GPU, just swaps the model.
- **Desktop** — a single Compute row (GPU or CPU) and a single model. No battery settings, because there's no battery. There's also a "Treat as desktop" toggle for a laptop that lives on a dock.
- **No usable GPU** — none of that shows up at all.

Override it with Always CPU or Always GPU any time and you get one Model row instead. If you have more than one GPU there's a picker for which one to use.

The main process never initializes a GPU context. Not once, not even to list your GPUs. Everything Vulkan happens in a short-lived child process (`tiro --gpu-worker` for inference, `tiro --gpu-enum` for enumeration) that exits when it's done. This matters on a laptop: one Vulkan context in a long-lived process pins the discrete GPU out of its sleep state for as long as the app is open, which is about seven watts, all day, forever. When you unplug, the worker gets killed *before* the CPU model loads.

The chip at the bottom of the panel shows the model and device that are actually running, not the one you asked for.

## Models

The Models page downloads GGUF models from Hugging Face — tiny through large-v3-turbo, with English-only versions of the smaller ones. Defaults are `base.en` (~80 MB) on battery and `small.en` (~250 MB) plugged in. Downloads are cancellable, watchdogged if they stall, and size-checked, so a half-finished file never gets treated as installed.

## Vocabulary

Two lists, both on the Vocabulary page.

**Hot words** are names and jargon fed to Whisper as a prompt so it spells them right on the way in. It's a hint, not a guarantee — Whisper still does whatever it wants sometimes.

**Corrections** are `heard → written` pairs for the stuff it keeps getting wrong anyway. These only touch the copy that goes to your clipboard. The transcript log always keeps the verbatim text, so a bad correction can never destroy what you actually said.

They live in `vocab.txt` and `corrections.txt` next to the config if you'd rather edit them in a text editor.

## Where the transcripts go

`Documents/Tiro` by default, changeable in Settings → Storage:

```
Documents/Tiro/YYYY-MM-DD.jsonl   # verbatim text, cleaned copy, mic, model, device, duration
Documents/Tiro/YYYY-MM-DD.md      # readable, one line per take
```

If that folder ever isn't writable, Tiro falls back to a `logs/` folder next to the app and puts a banner in the panel telling you. You can also turn saving off completely.

Clipboard cleanup (Off / Light / + Fillers) only changes what gets copied. The log is always verbatim.

## Linux

In an X11 session the hotkeys are normal keyboard grabs and just work.

Wayland doesn't let apps grab global keys, so Tiro binds through the `GlobalShortcuts` portal instead. On KDE and GNOME that works and the shortcuts show up in your system shortcut settings. The portal needs to be able to identify the app, which means an installed desktop file — that's a one-time setup for builds you didn't install from a package, see [BUILDING.md](BUILDING.md).

If your compositor has no GlobalShortcuts backend, bind desktop-level shortcuts to the CLI instead. A second launch remote-controls the running instance:

```
tiro --toggle   # start / stop dictating
tiro --paste    # paste the take at the cursor
tiro --panel    # show / hide the panel
tiro --cancel   # throw away the recording
```

Paste-at-cursor on Wayland goes through the remote-desktop portal, so the first paste pops a permission dialog. Allow it and tell it to remember; Tiro stores the token so it doesn't come back.

## Requirements

- Windows 10/11 (WebView2, preinstalled on 11) or Linux with WebKitGTK 4.1
- A microphone
- Optionally a Vulkan GPU, if you build with `--features gpu`

## Building

There are no installers yet — packaging is the last thing left. For now:

```sh
cd src-tauri
cargo build --release                   # CPU only, no GPU SDK needed
cargo build --release --features gpu    # + Vulkan, needs the Vulkan SDK
```

Full prerequisites and the Wayland/portal setup are in [BUILDING.md](BUILDING.md).

## Still to do

- Installers: NSIS/MSI on Windows, .deb and AppImage on Linux.
- A demo GIF and a screenshot in this README.

## The name

Marcus Tullius Tiro was Cicero's secretary. He invented a shorthand system so he could write speech down as fast as people talked, which makes him more or less the first dictation software. Two thousand years later here we are.

## License

[GPLv3](LICENSE) © Beckett Harriman.
