# Building Tiro

Tiro is a Tauri 2 app: a Rust backend (`src-tauri/`) serving the static
vanilla-JS UI in `ui/`. Development builds run with plain `cargo` — no
Node.js, no bundler.

## macOS

See [MACOS.md](MACOS.md) for the native build and permissions. From the repo
root, `npm ci && npm run build:mac` produces an ad-hoc-signed `tiro.app`
with Metal acceleration. `npm run build:mac:cpu` produces a CPU-only app.
`npm run package:mac` creates the distributable DMG and ZIP in `dist/macos/`,
including the license and corresponding source. The transparent windows
require Tauri's `macos-private-api` feature; this feature and the matching
configuration are inert on Linux and Windows.

## Common prerequisites

- **Rust** (stable, via [rustup](https://rustup.rs)) — edition 2021.
- **CMake** — whisper.cpp is built from source by `whisper-rs-sys`.
- A C/C++ toolchain (see per-OS notes below).

```sh
cd src-tauri
cargo build          # CPU-only build; no GPU SDK needed
cargo run            # launches the app (panel shows in debug builds)
cargo test           # unit tests
```

On first launch the app downloads the Whisper GGUF model(s) it needs into
`src-tauri/models/` (the only network access the app ever performs).

## Windows

- **Visual Studio 2022** (or Build Tools) with the "Desktop development
  with C++" workload (MSVC, Windows SDK).
- **CMake** — a real *Windows* CMake that knows the Visual Studio
  generator. If an MSYS/MinGW cmake shadows it on `PATH` (a devkitPro or
  Git-for-Windows install, say), whisper.cpp's configure fails with
  `Could not create named generator Visual Studio 17 2022`; point the
  build at the right one with `set CMAKE=C:\path\to\cmake.exe`.
- **LLVM** (for `libclang.dll` — whisper-rs generates bindings with
  bindgen): `scoop install llvm` or the
  [LLVM installer](https://releases.llvm.org/). If it is not on `PATH`,
  set `LIBCLANG_PATH` to the directory containing `libclang.dll`.
- **WebView2 Runtime** — preinstalled on Windows 11; on Windows 10 install
  the [Evergreen runtime](https://developer.microsoft.com/microsoft-edge/webview2/).

## Linux

Debian/Ubuntu package names (other distros: the equivalents):

```sh
sudo apt install build-essential cmake pkg-config \
    libwebkit2gtk-4.1-dev libgtk-3-dev \
    libayatana-appindicator3-dev librsvg2-dev \
    libasound2-dev libxdo-dev
```

- `libwebkit2gtk-4.1-dev` / `libgtk-3-dev` — the Tauri webview shell.
- `libayatana-appindicator3-dev` — tray icon.
- `libasound2-dev` — ALSA, for microphone capture (cpal).

### Global hotkeys on Wayland

In an X11 session global hotkeys are X11 keyboard grabs and just work.
In a **Wayland** session the compositor owns the keyboard, so Tiro binds
its combos through the `org.freedesktop.portal.GlobalShortcuts` portal
instead: on KDE and GNOME the shortcuts fire regardless of which app has
focus, and KDE lists them under System Settings → Shortcuts → Tiro. The
X11 grabs stay registered as a bridge until the portal bind succeeds; if
the portal is absent or denied (some compositors ship no GlobalShortcuts
backend), Tiro keeps the grabs — which only fire while an XWayland
window has focus — and logs the failure.

The portal only talks to callers it can identify: it reads the app id
from the process's systemd user unit (an `app-…` scope or service) and
requires a matching `<app id>.desktop` in the XDG applications dirs —
otherwise every request is refused with `NotAllowed: An app id is
required`. Tiro handles the first half itself by moving into an
`app-dev.tiro.app-<pid>.scope` at startup when the launcher didn't
provide one (terminal/script launches; menu launches are already scoped).
The second half is a one-time install for non-packaged builds — and note
that GLib only accepts a desktop file whose `Exec` binary resolves in
the portal's PATH, so the pair is required:

```sh
cp src-tauri/linux/dev.tiro.app.desktop ~/.local/share/applications/
ln -s "$PWD/src-tauri/target/release/tiro" ~/.local/bin/tiro
```

(`~/.local/bin` is on the systemd user session's PATH on Fedora by
default; if your distro's isn't, add it and run
`systemctl --user import-environment PATH`, then restart the portal or
re-log-in so it sees the change.)

tiro.log shows the resolved scope/app id (`app scope: …` lines) and
names the missing piece — absent desktop file or unresolvable Exec — if
the portal would still refuse.

The everywhere-working Wayland fallback is a **desktop-level shortcut
bound to Tiro's CLI**. A second `tiro` launch is forwarded to the
running instance (single-instance IPC), so these commands act as a
remote control:

| Command         | Action                                |
|-----------------|---------------------------------------|
| `tiro --toggle` | start / stop dictation                |
| `tiro --paste`  | paste the take at the cursor          |
| `tiro --panel`  | show / hide the panel                 |
| `tiro --cancel` | cancel the current recording          |
| `tiro`          | summon the panel (bring to front)     |

Examples: GNOME → Settings → Keyboard → Custom Shortcuts; KDE → System
Settings → Shortcuts → Add Command; sway/hyprland → `bindsym`/`bind`
to `tiro --toggle`.

Rebinding a hotkey in the panel re-runs the portal bind (the portal has
no unbind, so Tiro closes the old session and binds a fresh one); the
new combo is offered as the preferred trigger, which KDE accepts without
a dialog.

## GPU builds (optional)

```sh
cargo build --features gpu
```

The `gpu` feature compiles whisper.cpp's Vulkan backend for the
`tiro --gpu-worker` child process and requires the
[Vulkan SDK](https://vulkan.lunarg.com/) (headers + `glslc`) at build
time. Plain builds never need it: the app then serves CPU inference and
reports the GPU as unavailable. The main process never initializes a GPU
context in either build — GPU inference always lives in the worker child
(see PORTING_NOTES §6).

## Development notes: WSLg quirks vs real-Linux issues

Development happens Linux-native under WSL2/WSLg. Some behaviors there
are **WSLg artifacts**, not app or real-Linux bugs:

- **Keyboard death after hiding the focused window**: WSLg's XWayland
  stops delivering keyboard events to every X client (even fresh grabs)
  once the last focused surface unmaps — e.g. hiding the panel by
  hotkey with no other app window open. Focusing any surface (or
  relaunching the app) revives input. App threads stay healthy
  throughout.
- **Synthetic window drags are ignored**: the compositor does not honor
  `_NET_WM_MOVERESIZE` driven by XTEST pointer input, so interactive
  drag behavior can't be exercised there.
- **Audio**: only ALSA's null device exists — capture yields silence
  without real-time pacing, and sound cues are inaudible.
- **No Settings portal by default**: `XDG_CURRENT_DESKTOP` is empty, so
  xdg-desktop-portal selects no backend and theme detection degrades to
  the boot-time value. Fix for development:
  `~/.config/xdg-desktop-portal/portals.conf` with
  `[preferred]` / `default=gtk`, then restart the portal services.
- **`GDK_BACKEND=x11` required** to render (WSLg's Wayland/EGL path
  fails), and there is no StatusNotifier host, so the tray icon has
  nowhere to appear.

Real-Linux notes that are **not** WSLg-specific (and are handled in
code):

- The windowing layer latches the boot-time portal color-scheme and its
  OS ThemeChanged events carry a dummy window id — Tiro reads the
  Settings portal directly and polls on the 20 s watcher instead.
- WebKitGTK reports a ~200 px minimum widget height; the pill window
  clears GTK size requests at startup to reach its 300x72 size.
- Wayland global-hotkey limits and the CLI fallback: see the section
  above.

## Packaging

Installers come out of the Tauri bundler. The CLI is pinned as an npm
dev dependency so every machine (and CI) runs the same one:

```sh
npm ci
NO_STRIP=true npx tauri build   # from the repo root; CPU-only, like releases
```

(`cargo install tauri-cli` and `cargo tauri build` work too.) `NO_STRIP`
is for the AppImage: linuxdeploy's bundled `strip` is too old for the
`.relr.dyn` sections in current distro libraries and aborts the whole
bundle when it fails on one; skipping it costs a few MB. Output
lands under `src-tauri/target/release/bundle/`: `deb/`, `rpm/` and
`appimage/` on Linux, `nsis/` (per-user setup .exe) and `msi/` on
Windows. The AppImage step downloads linuxdeploy into `~/.cache/tauri`
the first time; NSIS and WiX are fetched the same way on Windows.

Everything the bundles contain is declared in `src-tauri/tauri.conf.json`:
the UI (`build.frontendDist`, embedded into the binary at compile time),
the icons (`bundle.icon`; `icon.ico` carries 16/32/128/256, the PNGs
become the hicolor sizes on Linux), and the portal identity file
`linux/dev.tiro.app.desktop` (`bundle.linux.{deb,rpm}.files`) next to
the launcher entry the bundler generates. Adding a UI file needs no
packaging change. The runtime dependencies the bundler cannot see
(ALSA, the appindicator library the tray loads with dlopen) are listed
under `depends`; WebKitGTK and GTK are added by the bundler itself.

The version lives in `src-tauri/Cargo.toml` **only**; `tauri.conf.json`
has no `version` key so it inherits that one. Bump it there and nowhere
else.

`.github/workflows/release.yml` builds the whole set on a `v*` tag
(Linux and Windows runners), checks the tag against Cargo.toml, and
attaches every `*.deb`, `*.rpm`, `*.AppImage`, `*.exe` and `*.msi` it
finds to a draft GitHub Release for the tag. The Actions tab can also run
it by hand, which builds without releasing. Released builds are CPU-only:
a Vulkan-linked binary will not load at all on a machine without the
Vulkan runtime, and it is the one binary for the main process and the
worker, so a GPU build stays a from-source option.

An installed binary sits somewhere the user cannot write (`/usr/bin`,
the AppImage mount, Program Files), so `flow::app_dir()` keeps the app
files in `$XDG_DATA_HOME/tiro` (`~/.local/share/tiro`) or
`%LOCALAPPDATA%\tiro` in that case; a cargo build and the per-user NSIS
install keep them next to the app as before.
