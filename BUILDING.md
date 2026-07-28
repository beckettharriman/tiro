# Building Tiro

Tiro is a Tauri 2 app: a Rust backend (`src-tauri/`) serving the static
vanilla-JS UI in `ui/`. Development builds run with plain `cargo` — no
Node.js, no bundler.

## Common prerequisites (both OSes)

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

Global hotkeys are X11 keyboard grabs. In an X11 session they just work.
In a **Wayland** session the compositor owns the keyboard: the grabs only
fire while an XWayland window has focus, and not at all without an X
server — Tiro logs a notice at startup when it detects this.

The supported Wayland setup is a **desktop-level shortcut bound to Tiro's
CLI**. A second `tiro` launch is forwarded to the running instance
(single-instance IPC), so these commands act as a remote control:

| Command         | Action                                |
|-----------------|---------------------------------------|
| `tiro --toggle` | start / stop dictation                |
| `tiro --panel`  | show / hide the panel                 |
| `tiro --cancel` | cancel the current recording          |
| `tiro`          | summon the panel (bring to front)     |

Examples: GNOME → Settings → Keyboard → Custom Shortcuts; KDE → System
Settings → Shortcuts → Add Command; sway/hyprland → `bindsym`/`bind`
to `tiro --toggle`.

The `org.freedesktop.portal.GlobalShortcuts` portal is the eventual
native answer, but the shortcut plugin Tiro uses has no Wayland backend
yet, so the CLI route is the documented, everywhere-working fallback.

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

## Packaging

Installer builds (`cargo tauri build` — NSIS/MSI on Windows, .deb and
AppImage on Linux) are covered by PORT_PLAN task 5.2 and additionally
need the Tauri CLI: `cargo install tauri-cli`.
