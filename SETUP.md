# Setting Tiro up

**This file is the source of truth for installing and configuring Tiro**, and it
ships with the repo, so it moves with the code: if the build changes, this file
changes in the same commit.

There are two ways to use it. Follow the steps below by hand, or paste the
prompt in the [README](README.md#install) and let a terminal agent work through
the same steps for you. The steps are the steps either way. If you are an agent,
read [Rules for an agent](#rules-for-an-agent) at the bottom first.

Repo: <https://github.com/beckettharriman/tiro>

> If anything here turns out to be wrong on a real machine, fix **this file** and
> open a pull request. That is the whole point of it living in the repo.

Tagged releases ship installers on the [Releases page](https://github.com/beckettharriman/tiro/releases):
a setup `.exe` and an `.msi` on Windows, a `.deb`, an `.rpm` and an AppImage on
Linux, all CPU only. Install one of those, launch Tiro from the app menu, and
pick up at [step 4](#4-first-run). Steps 1 to 3 are building from source, which
is also the only way to get a GPU build. It's two commands once the
prerequisites are in place.

## macOS

For a Mac, follow [MACOS.md](MACOS.md) for the native app bundle, Metal
acceleration, microphone and Accessibility permissions, and verification.
The Linux and Windows steps below use a different packaging and data layout.

## Before you start

Two things decide how the rest goes.

**1. CPU or GPU build.** A GPU build needs the [Vulkan SDK](https://vulkan.lunarg.com/)
(headers and `glslc`) at build time. Without it, build CPU only: Tiro runs fine
that way and reports the GPU as unavailable. You can rebuild with the GPU
feature later without losing anything.

**2. On Linux, X11 or Wayland.** X11 needs nothing extra. Wayland needs the
one-time desktop-file step in [step 5](#5-linux-hotkeys-on-wayland). Check with:

```sh
echo $XDG_SESSION_TYPE
```

Useful to know before you start, and worth having on hand if you end up filing
an issue:

| Fact | How to check |
|---|---|
| Linux distro | `cat /etc/os-release` |
| Desktop | `echo $XDG_CURRENT_DESKTOP` |
| GPU | `lspci -nn \| grep -i vga`, or `wmic path win32_VideoController get name` |
| Vulkan | `vulkaninfo --summary` |
| Rust | `cargo --version` |

## 1. Prerequisites

Every OS needs **Rust** (stable, via [rustup](https://rustup.rs)), **CMake**
(whisper.cpp is compiled from source), and a C/C++ toolchain.

**Fedora / RHEL**

```sh
sudo dnf install gcc-c++ cmake pkgconf-pkg-config \
    webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel \
    librsvg2-devel alsa-lib-devel libxdo-devel
```

**Debian / Ubuntu**

```sh
sudo apt install build-essential cmake pkg-config \
    libwebkit2gtk-4.1-dev libgtk-3-dev \
    libayatana-appindicator3-dev librsvg2-dev \
    libasound2-dev libxdo-dev
```

**Arch**

```sh
sudo pacman -S base-devel cmake webkit2gtk-4.1 gtk3 \
    libappindicator-gtk3 librsvg alsa-lib xdotool
```

**Windows**

- Visual Studio 2022 or Build Tools, with the *Desktop development with C++*
  workload (MSVC and the Windows SDK).
- A real Windows CMake that knows the Visual Studio generator. An MSYS/MinGW
  cmake earlier on `PATH` breaks the build (see [Troubleshooting](#troubleshooting)).
- LLVM, for `libclang.dll`: `scoop install llvm`, or the LLVM installer.
- WebView2 Runtime. Preinstalled on Windows 11; on Windows 10, install the
  Evergreen runtime.

## 2. Get the source

```sh
git clone https://github.com/beckettharriman/tiro.git
cd tiro
```

## 3. Build

```sh
cd src-tauri
cargo build --release                   # CPU only
cargo build --release --features gpu    # + Vulkan
```

The first build compiles whisper.cpp and takes a while. The binary lands at
`src-tauri/target/release/tiro` (`tiro.exe` on Windows).

Check it before going further:

```sh
./target/release/tiro --gpu-enum
```

That prints the Vulkan devices as one line of JSON and exits. An empty list means
no usable GPU, which is expected on a CPU build.

## 4. First run

```sh
./target/release/tiro
```

Tiro starts hidden with a tray icon. Press `Ctrl+Alt+C` to bring up the panel.

It writes `config.ini` next to the app on first launch, and downloads the Whisper
model the first time you dictate: roughly 80 MB for `base.en`, 250 MB for
`small.en`. **That download is the only network access Tiro ever performs.**

For a cargo build, "next to the app" is the crate directory, so
`src-tauri/config.ini`, `src-tauri/models/`, `src-tauri/vocab.txt`,
`src-tauri/corrections.txt`. A `.deb`, `.rpm`, AppImage or `.msi` install can't
write next to the app, so those use `~/.local/share/tiro` on Linux and
`%LOCALAPPDATA%\tiro` on Windows. Set `TIRO_APP_DIR` to put them somewhere else.

## 5. Linux: hotkeys on Wayland

Skip this on X11 and on Windows, and after a `.deb` or `.rpm` install, which
ships the desktop file. For an AppImage, symlink the AppImage itself as
`~/.local/bin/tiro` in the second command.

Wayland compositors don't let apps grab global keys, so Tiro binds through the
`org.freedesktop.portal.GlobalShortcuts` portal. The portal refuses callers it
can't identify, which means an installed desktop file whose `Exec` resolves on
the portal's `PATH`. Both halves are required:

```sh
cp src-tauri/linux/dev.tiro.app.desktop ~/.local/share/applications/
ln -s "$PWD/src-tauri/target/release/tiro" ~/.local/bin/tiro
```

Restart Tiro afterwards. On KDE and GNOME the shortcuts then fire regardless of
focus, and KDE lists them under System Settings → Shortcuts → Tiro. `tiro.log`
prints the resolved app id (`app scope: …`) and names whichever half is missing
if the portal still refuses.

If your compositor ships no GlobalShortcuts backend at all, bind desktop-level
shortcuts to the CLI instead. A second launch remote-controls the running one:

| Command | Action |
|---|---|
| `tiro --toggle` | start / stop dictating |
| `tiro --paste` | paste the take at the cursor |
| `tiro --panel` | show / hide the panel |
| `tiro --cancel` | throw away the recording |

GNOME: Settings → Keyboard → Custom Shortcuts. KDE: System Settings →
Shortcuts → Add Command. sway/hyprland: `bindsym` / `bind`.

## 6. Configure

Everything is in the panel (`Ctrl+Alt+C`, then the expand button, then Settings),
and every change is written straight back to `config.ini`. Worth setting on a
fresh install:

| Setting | Where | Note |
|---|---|---|
| Microphone | Settings → Input | The Test button feeds a live level meter |
| Compute device | Settings → Engine | Auto Switch is right for most laptops |
| Models | Models page | `base.en` and `small.en` cover most people |
| Hot words | Vocabulary | Names and jargon Whisper keeps misspelling |
| Launch at login | Settings → System | |
| Save location | Settings → Storage | Defaults to `Documents/Tiro` |

Don't hand-edit `config.ini` while Tiro is running. It rewrites the file on every
settings change, and strips comments when it does.

## 7. Check it works

- [ ] Tray icon is present.
- [ ] `Ctrl+Alt+C` opens the panel, and the chip at the bottom names a model and a device.
- [ ] `Ctrl+Alt+Space`, say a sentence, press again. The text shows up in the panel and pastes out of your clipboard.
- [ ] Hold `Ctrl+Alt+V`, talk, release. It pastes at the cursor.
- [ ] A `YYYY-MM-DD.jsonl` and `.md` pair appeared in `Documents/Tiro`.
- [ ] `Ctrl+Alt+X` during a recording throws it away.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `Could not create named generator Visual Studio 17 2022` | An MSYS/MinGW `cmake` is shadowing the Windows one. Point at the right one: `set CMAKE=C:\path\to\cmake.exe` |
| bindgen fails, `libclang.dll` not found | LLVM isn't on `PATH`. Set `LIBCLANG_PATH` to the directory holding `libclang.dll` |
| Build fails on `glslc` or Vulkan headers | GPU build without the Vulkan SDK. Install it, or drop `--features gpu` |
| `failed to remove file … tiro.exe, Access is denied` | Tiro is running and holding its own binary. Quit it, then rebuild |
| Hotkeys do nothing on Wayland | Step 5. Check `tiro.log` for `NotAllowed: An app id is required` |
| Hotkeys do nothing on X11 | Another app already owns the combo. Rebind in Settings → Shortcuts |
| No tray icon | The desktop has no StatusNotifier host. Use the hotkeys or `tiro --panel` |
| Panel or pill won't render on Linux | Force X11: `GDK_BACKEND=x11 ./tiro` |
| Recording produces silence | Wrong input device. Settings → Input, pick the mic, press Test and watch the meter |
| Transcripts aren't being written | The save folder is unwritable. The panel shows a banner naming where they went instead |

## Sending feedback back

Issues and pull requests are welcome, and the whole loop works from a terminal
with the [GitHub CLI](https://cli.github.com):

```sh
gh issue create --repo beckettharriman/tiro --title "…" --body "…"

gh repo fork beckettharriman/tiro --clone
git switch -c my-fix
# change something, then:
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
gh pr create --fill
```

Those three checks are what the code is held to, so run them before opening a PR.
If it's a build or install failure, say which OS, which distro, and which step of
this file it happened on.

## Rules for an agent

Only relevant if a terminal agent is doing the steps above.

- **Ask before anything privileged.** Package installs, `sudo`, systemd units,
  registry writes, and login items all need a yes from the person first. Show the
  exact command you intend to run.
- **Never overwrite existing user data.** `config.ini`, `vocab.txt`,
  `corrections.txt` and `models/` belong to the user. If one already exists,
  leave it alone and say so.
- **Don't guess at failures.** Every failure mode we have actually hit is in
  [Troubleshooting](#troubleshooting) with its real cause. Look there before
  inventing a fix.
- **Report at the end**: what you installed, where the binary is, whether the
  build was CPU or GPU, and the four hotkeys to try.

## Related docs

- [README.md](README.md): what Tiro is and what it does.
- [BUILDING.md](BUILDING.md): development detail: per-OS build internals,
  packaging, and the platform quirks behind some of the code.
