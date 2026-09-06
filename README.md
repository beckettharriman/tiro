# Tiro

Press a hotkey, talk, press it again. Your words are on the clipboard, or pasted at your cursor, whichever you asked for. Nothing ever leaves your computer.

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
![Platform: Windows | Linux](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-0078D6.svg)
![Rust + Tauri](https://img.shields.io/badge/built%20with-Rust%20%2B%20Tauri-orange.svg)
![Offline](https://img.shields.io/badge/100%25-offline-30d158.svg)

<!-- TODO(launch): docs/demo.gif + docs/panel.png -->

## Why I made this

Every dictation tool I tried types into whatever box you're focused on. Click somewhere else and it stops, or it dumps half a sentence into the wrong window. That's the thing I built this to get away from. I talk while I'm doing other stuff, scrolling, clicking around, switching apps, and I don't want the app fighting me for the cursor the whole time.

So Tiro splits the talking from the delivery. It records, transcribes locally with whisper.cpp, and then you decide where the text goes. It'll paste at your cursor if you want it to, or it'll just sit on the clipboard until you're ready. Either way you never have to be focused on a text box while you're actually talking.

I mostly use it for journaling and for talking at agents instead of typing paragraphs at them.

## The ways you can use it

It's meant to be flexible. Same recording, different endings.

**Hold and talk, paste when you let go.** Hold `Ctrl+Alt+V`, say the thing, let go. It lands at your cursor. Fastest way to fire one sentence into a chat box or a terminal.

**Start a take, go do something, then paste.** Tap `Ctrl+Alt+Space`, talk for as long as you want, click around, open the window you actually wanted it in, then end the take on `Ctrl+Alt+V` and it pastes there.

**Just copy it.** End on `Ctrl+Alt+Space` and it stays on the clipboard. Paste it in ten minutes, or three times, or never.

The key you *finish* on is what decides. Space is clipboard only, V is clipboard plus paste. Everything goes to the clipboard either way, so pasting is an extra on top, not a replacement. If the keystroke can't be injected for some reason, the text is still sitting on your clipboard and the pill says so.

## Hotkeys

| Key | What it does |
|---|---|
| `Ctrl+Alt+Space` | Start dictating. Press again to stop and copy. |
| `Ctrl+Alt+V` | Same, but it also pastes at your cursor when you stop. |
| `Ctrl+Alt+C` | Show / hide the panel. |
| `Ctrl+Alt+X` | Throw away the current recording. |

Tap either of the first two to toggle, or hold it for push-to-talk and it stops when you let go. All four are rebindable in Settings, Shortcuts.

## There's no time limit

Talk for as long as you want. There used to be a ten minute cap, and one day it silently ate the back half of a thirty minute journal entry. The pill kept animating, the timer kept counting, and the mic had been dead for twenty minutes. That's the worst thing this app can possibly do, so the cap is gone entirely.

The only real limit now is memory, about 700 MB per hour of talking. And Tiro refuses to open null or dummy input devices, which is what that cap was actually guarding against in the first place.

## Offline

The only time Tiro touches the network is downloading a Whisper model, and only when you ask it to. After that you can pull the ethernet cable out of the wall and nothing changes. No accounts, no telemetry, no cloud anything.

## The panel

`Ctrl+Alt+C` summons a small frameless glass panel over whatever you're doing. Drag the header to move it, pin it to keep it on top, close it and it's gone again.

Compact, it's a mic picker, a record button, and today's transcripts. Click any line to copy it again.

The expand button widens it into four pages:

**Transcribe.** Everything you've dictated, newest first, with a search box and a day pager. It opens on today and streams older days in as you scroll back, holding about five days at a time so it doesn't get heavy after a year of use.

**Settings.** Engine, audio, capture, input, shortcuts, storage, appearance, launch at login.

**Vocabulary.** Hot words and corrections, below.

**Models.** What's installed and what you can download.

There's a tray icon too. Left click toggles the panel, right click gives you Open, Start/Stop dictation, Restart, and Quit.

## Engine and GPU

Tiro figures out what kind of machine it's on once at startup, and then only shows you the settings that actually mean something on it.

**Laptop with a discrete GPU:** Auto Switch. GPU and the bigger model when you're plugged in, CPU and the lighter model on battery.

**Laptop with integrated graphics:** stays on the GPU, just swaps the model.

**Desktop:** one Compute row (GPU or CPU) and one model. No battery settings, because there's no battery. There's a "Treat as desktop" toggle for a laptop that lives on a dock.

**No usable GPU:** none of that shows up at all.

You can override it with Always CPU or Always GPU any time, and then you get a single Model row instead. If you've got more than one GPU there's a picker for which one runs.

The main process never initializes a GPU context. Not once, not even to list your GPUs. Everything Vulkan happens in a short lived child process (`tiro --gpu-worker` for inference, `tiro --gpu-enum` for enumeration) that exits the second it's done. This one matters a lot on a laptop: a single Vulkan context in a long lived process pins the discrete GPU out of its sleep state for as long as the app is open, and that's about seven watts, all day, for an app you leave running all day. When you unplug, the worker gets killed *before* the CPU model loads.

The chip at the bottom of the panel shows the model and device that are actually running, not the one you asked for.

## Models

The Models page downloads GGUF models from Hugging Face. Tiny through large-v3-turbo, with English only versions of the smaller ones. Defaults are `base.en` (~80 MB) on battery and `small.en` (~250 MB) plugged in. Downloads are cancellable, watchdogged if they stall, and size checked, so a half finished file never gets treated as installed.

## Vocabulary

Two lists, both on the Vocabulary page.

**Hot words** are names and jargon fed to Whisper as a prompt so it spells them right on the way in. It's a hint, not a guarantee. Whisper still does whatever it wants sometimes.

**Corrections** are `heard → written` pairs for the stuff it keeps getting wrong anyway. These only touch the copy that goes to your clipboard. The transcript log always keeps the verbatim text, so a bad correction can never destroy what you actually said.

They live in `vocab.txt` and `corrections.txt` next to the config if you'd rather edit them in a text editor.

## Where the transcripts go

`Documents/Tiro` by default, changeable in Settings, Storage:

```
Documents/Tiro/YYYY-MM-DD.jsonl   # verbatim text, cleaned copy, mic, model, device, duration
Documents/Tiro/YYYY-MM-DD.md      # readable, one line per take
```

If that folder ever isn't writable, Tiro falls back to a `logs/` folder next to the app and puts a banner in the panel telling you. You can also turn saving off completely.

Clipboard cleanup (Off, Light, or + Fillers) only changes what gets copied. The log is always verbatim.

## Linux

In an X11 session the hotkeys are normal keyboard grabs and they just work.

Wayland doesn't let apps grab global keys, so Tiro binds through the `GlobalShortcuts` portal instead. On KDE and GNOME that works fine and the shortcuts show up in your system shortcut settings. The portal has to be able to identify the app, which means an installed desktop file, so that's a one time setup for builds you didn't install from a package. See [BUILDING.md](BUILDING.md).

If your compositor has no GlobalShortcuts backend, bind desktop level shortcuts to the CLI instead. A second launch remote controls the running instance:

```
tiro --toggle   # start / stop dictating
tiro --paste    # paste the take at the cursor
tiro --panel    # show / hide the panel
tiro --cancel   # throw away the recording
```

Paste at cursor on Wayland goes through the remote desktop portal, so the first paste pops a permission dialog. Allow it and tell it to remember. Tiro stores the token so it doesn't come back.

## Requirements

- Windows 10/11 (WebView2, preinstalled on 11) or Linux with WebKitGTK 4.1
- A microphone
- Optionally a Vulkan GPU, if you build with `--features gpu`

## Building

There are no installers yet, packaging is the last thing left. For now:

```sh
cd src-tauri
cargo build --release                   # CPU only, no GPU SDK needed
cargo build --release --features gpu    # + Vulkan, needs the Vulkan SDK
```

Full prerequisites and the Wayland portal setup are in [BUILDING.md](BUILDING.md).

## Still to do

- Installers: NSIS/MSI on Windows, .deb and AppImage on Linux.
- A demo GIF and a screenshot in this README.

## The name

Marcus Tullius Tiro was Cicero's secretary. He invented a shorthand system so he could write speech down as fast as people talked, which makes him more or less the first dictation software. Two thousand years later, here we are.

## License

[GPLv3](LICENSE) © Beckett Harriman.
