<div align="center">

# Tiro

[![License: GPL v3](https://img.shields.io/badge/license-GPLv3-blue.svg)](LICENSE) ![Windows | Linux](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-444.svg) ![Rust + Tauri](https://img.shields.io/badge/built%20with-Rust%20%2B%20Tauri-orange.svg) ![Offline](https://img.shields.io/badge/100%25-offline-30d158.svg) ![Work in progress](https://img.shields.io/badge/status-work%20in%20progress-e8a33d.svg)

*Offline, on device transcription tool. Turns out you never had to send your voice to a tech bro in Silicon Valley to get a decent transcript.*

<img src="docs/tour.gif" width="720" alt="The Tiro panel opening and moving through its four pages">

<img src="docs/pill.gif" width="480" alt="The recording pill, with the waveform following your voice">

**Press a hotkey, talk, press it again.**<br>
Your words land on the clipboard, or at your cursor. Nothing ever leaves your computer.

</div>

> [!IMPORTANT]
> **Work in progress.** I use this every day on Linux and Windows for my real work and it's stable for me, but it isn't complete yet, and I'm still changing things. Consider this application in development beta and not a polished final product.

## Why I made this

Every dictation tool I tried types into whatever box you're focused on. Click somewhere else and it stops, or it dumps half a sentence into the wrong window. I talk while I'm doing other stuff, clicking around, reviewing recent work, switching apps, and I don't want the app fighting me for the cursor.

So Tiro splits the talking from the delivery. It records, transcribes locally with whisper.cpp, and you pick where the text goes.

|  |  |
|---|---|
| **Quick transcribe** | Hold `Ctrl+Alt+V`, say something, let go. It lands at your cursor. |
| **Toggle dictation, then paste it** | Tap `Ctrl+Alt+Space`, talk as long as you want, click around, find the window you actually wanted, and finish on `Ctrl+Alt+V`. |
| **Just copy it** | Stop with `Ctrl+Alt+Space` and the output is stored in the clipboard and in the Tiro panel. |

The key you *finish* on decides. Everything reaches the clipboard either way, so pasting is an extra on top, never a replacement.

## Hotkeys

| Key | Action |
|---|---|
| `Ctrl+Alt+Space` | Dictate. Tap to toggle, hold for push-to-talk. |
| `Ctrl+Alt+V` | Same, and it pastes at your cursor. |
| `Ctrl+Alt+C` | Show / hide the panel. |
| `Ctrl+Alt+X` | Throw away the recording. |

All four are rebindable.

## The panel

`Ctrl+Alt+C` puts a panel over whatever you're doing: a mic picker, a record button, and recent transcriptions. Click any line to copy it again. Expand the panel for more controls and settings.

|  |  |
|---|---|
| <img src="docs/transcribe.png" alt="Transcribe"> | <img src="docs/settings.png" alt="Settings"> |
| **Transcribe.** Search back through everything you've dictated. Opens on today and streams older days in as you scroll. | **Settings.** Engine, audio, capture, input, shortcuts, storage, appearance, login. |
| <img src="docs/vocabulary.png" alt="Vocabulary"> | <img src="docs/models.png" alt="Models"> |
| **Vocabulary.** Hot words prime Whisper so it spells your names right. Corrections fix what it still misses, and only touch the copy you paste. | **Models.** Manage the transcription models installed on device, tiny through large-v3-turbo from Hugging Face. |

There's a tray icon too: left click toggles the panel, right click gives you Open, Start/Stop, Restart, Quit.

## Engine, GPU, laptop power savings

Tiro works out what machine it's on at startup and shows only the settings that apply.

| Machine | What you get |
|---|---|
| Laptop, discrete GPU | Auto Switch: GPU and the bigger model plugged in, CPU and the lighter one on battery |
| Laptop, integrated | Stays on the GPU, just swaps the model |
| Desktop | One compute row, one model, no battery settings |
| No usable GPU | None of it shows up |

The same install should do the right thing on every kind of machine without you configuring it. A desktop just uses the GPU and never thinks about batteries, because it doesn't have one. A laptop shouldn't burn power on the discrete card to transcribe one sentence, so on battery it drops to the CPU and a lighter model, and every bit of GPU work lives in a child process that exits when it's done, because a GPU context held open inside a long lived app keeps that card awake and costs you watts all day. A machine with no usable GPU shouldn't be shown settings about one at all.

## Nothing gets lost

Every take is written verbatim to `Documents/Tiro`, one JSONL and one Markdown file per day. Clipboard cleanup and corrections change only the copy you paste, never the log. If the folder isn't writable, Tiro falls back next to the app and says so in the panel.

The one time it touches the network is downloading a model, and only when you ask. No accounts, no telemetry.

## Install

**macOS:** install the native menu bar app from a DMG, or build your own
installer with `npm ci && npm run package:mac`. See [the macOS guide](MACOS.md)
for installation, Metal acceleration, permissions, and the CPU-only alternative.

**One click agentic install.** Paste this prompt at your terminal agent and get Tiro working with little effort:

```text
Set up Tiro on my machine.

Read https://raw.githubusercontent.com/beckettharriman/tiro/main/SETUP.md and
follow it end to end for my OS. That file is maintained in this repo and is the
source of truth, so trust it over anything you already know about the project.
If you can't fetch it, clone https://github.com/beckettharriman/tiro and read
SETUP.md out of the checkout.

Ask me before anything that needs sudo or changes system settings. When you're
done, run the verification checklist at the end and tell me which keys to press.
```

**Install instructions.** The same steps written out in full, with per-distro package lines and troubleshooting, are in [SETUP.md](SETUP.md). It lives in the repo, so it changes in the same commit the build does and neither path goes stale. The short version:

**1. Prerequisites.** Rust, CMake, and a C/C++ toolchain everywhere. On Linux, also your distro's WebKitGTK 4.1, GTK 3, appindicator, librsvg, ALSA and libxdo dev packages. On Windows, VS 2022 with the C++ workload, a real Windows CMake, LLVM for `libclang.dll`, and the WebView2 runtime. Exact package lines per distro are in [SETUP.md](SETUP.md#1-prerequisites).

**2. Clone and build.** A GPU build additionally needs the [Vulkan SDK](https://vulkan.lunarg.com/); without it, build CPU only and add the GPU later.

```sh
git clone https://github.com/beckettharriman/tiro.git
cd tiro/src-tauri
cargo build --release                   # CPU only
cargo build --release --features gpu    # + Vulkan
```

**3. Run it.**

```sh
./target/release/tiro
```

It starts hidden with a tray icon. `Ctrl+Alt+C` opens the panel. `config.ini` is written next to the app, and the first dictation downloads a model, about 80 MB.

**4. Wayland only.** Install the desktop file so the shortcuts portal can identify Tiro, then restart it:

```sh
cp src-tauri/linux/dev.tiro.app.desktop ~/.local/share/applications/
ln -s "$PWD/src-tauri/target/release/tiro" ~/.local/bin/tiro
```

**5. Check it.** Press `Ctrl+Alt+Space`, say a sentence, press it again. The text should appear in the panel and paste out of your clipboard.

Anything that goes wrong is probably in [SETUP.md](SETUP.md#troubleshooting).

<details>
<summary><b>Linux hotkeys, X11 and Wayland</b></summary>

X11 hotkeys are normal keyboard grabs. Wayland doesn't allow those, so Tiro binds through the `GlobalShortcuts` portal, which works on KDE and GNOME once the desktop file above is installed. Paste at cursor goes through the remote desktop portal, so the first one asks permission.

If your compositor has no portal backend, bind desktop shortcuts to the CLI instead, since a second launch remote controls the running one:

```
tiro --toggle   # start / stop dictating
tiro --paste    # paste the take at the cursor
tiro --panel    # show / hide the panel
tiro --cancel   # throw away the recording
```

</details>

## Status

Working on both Windows and Linux: dictation, paste at cursor, hotkeys on X11 and Wayland, the panel, the engine and GPU policy, models, vocabulary, transcripts, tray, autostart. It is still a development beta and not a polished final product, and I'm still changing things.

Not done: installers. NSIS/MSI on Windows, `.deb` and AppImage on Linux.

## The name

[Marcus Tullius Tiro](https://en.wikipedia.org/wiki/Marcus_Tullius_Tiro) was Cicero's secretary. He invented a shorthand system so he could write speech down as fast as people talked, which makes him more or less the first dictation software. Two thousand years later, here we are.

## License

[GPLv3](LICENSE) © Beckett Harriman.
