# Tiro on macOS

Tiro builds as a native macOS menu bar app. Apple Silicon uses Metal for
local Whisper inference; a CPU-only build is also available. macOS 12 or
newer is required. Build on the Mac architecture you want to run.

## Install a built application

Open the `.dmg` for your Mac and drag `tiro.app` onto its **Applications**
shortcut. Eject the disk image and open Tiro from Applications. Alternatively,
extract the ZIP and move `tiro.app` to Applications. The `arm64` package is
for Apple Silicon; `x86_64` is for Intel. A personal `~/Applications` folder
also works without an administrator install.

The app includes its executable, interface, and native transcription engine.
No build tools or terminal setup are required to run it. The speech model
downloads on first use. The installer includes a readme, license, exact source
snapshot, and build revision. Settings and downloaded models are stored
separately, so replacing the app preserves them.

These local packages are ad-hoc signed, not notarized. macOS may block a build
downloaded from the internet; a maintainer needs Developer ID signing and
notarization for normal public distribution. Do not disable Gatekeeper.

## Build an installer

Install Xcode or its Command Line Tools, stable Rust, CMake, and Node.js.
If you use Homebrew, `brew install cmake rust node` provides the build tools;
`xcode-select --install` installs Apple's Command Line Tools when absent.
No Vulkan SDK, Python runtime, or separate webview runtime is needed.

From the repository root:

```sh
npm ci
npm run package:mac
```

The command builds a Metal app and writes a DMG, ZIP, and SHA-256 manifest to
`dist/macos/`, for example `Tiro-0.1.0-macOS-arm64-metal.dmg`. To verify the
downloads, run `shasum -a 256 -c Tiro-0.1.0-macOS-arm64-metal.sha256` in that
folder. Use `npm run package:mac:cpu` for CPU-only installers.

Packaging requires a clean Git checkout so its included `Source.tar.gz`
matches the executable. Build artifacts are ignored by Git. The DMG is
created and verified using macOS's built-in `hdiutil`; it does not need
Finder automation or additional packaging utilities.

For an app bundle without an installer, use `npm run build:mac` (Metal) or
`npm run build:mac:cpu`, then open
`src-tauri/target/release/bundle/macos/tiro.app`. For development and checks,
use the ordinary Cargo commands in `src-tauri`:

```sh
cargo fmt --check
cargo clippy --all-targets --features metal -- -D warnings
cargo test --features metal
```

The app bundle is self-contained; Rust, Node, and CMake are needed only to
build it. You can move `tiro.app` to Applications before granting permissions.

The installer packager stages the app outside cloud-synced folders before
checking its signature. When launching a bare app build instead, note that
if your checkout is in an iCloud-synced Documents folder, Finder may add
metadata that makes signature verification fail with `resource fork, Finder
information, or similar detritus not allowed`. Copy your locally built app
to a non-synced Applications folder and remove only that Finder metadata:

```sh
mkdir -p ~/Applications
# Only when ~/Applications/tiro.app does not already contain your install:
ditto --norsrc src-tauri/target/release/bundle/macos/tiro.app ~/Applications/tiro.app
xattr -dr com.apple.FinderInfo ~/Applications/tiro.app
codesign --verify --deep --strict ~/Applications/tiro.app
open ~/Applications/tiro.app
```

`scripts/check-macos.sh metal` (or `cpu`) runs formatting, tests, strict
linting, app packaging, signature verification, and GPU discovery. The
optional GitHub Actions template is `ci/macos.yml`; copy it to
`.github/workflows/macos.yml` using a GitHub login with workflow permission
to run these checks on pushes and pull requests.

## Permissions and first dictation

The panel opens at launch. Close it to keep Tiro in the menu bar. Click its
menu bar icon to show it again, or use the panel shortcut below.

1. Press **Control+Option+Space** to start dictation. On first use, allow
   Tiro's microphone request. If permission was denied, enable Tiro in
   **System Settings → Privacy & Security → Microphone**.
2. Say a sentence and press the same shortcut again. Tiro transcribes it
   locally, saves it in the transcript history, and copies it. Paste with
   **Command+V**.
3. To paste automatically into another app, finish a take with
   **Control+Option+V**, or hold that shortcut while speaking. The first
   automatic paste asks for **Accessibility** access. Enable Tiro in
   **System Settings → Privacy & Security → Accessibility**, then retry.
   The text remains on the clipboard when automatic paste is unavailable.

| Shortcut | Action |
| --- | --- |
| Control+Option+Space | Toggle dictation, or hold for push-to-talk; copy result |
| Control+Option+V | Dictate and paste at the cursor |
| Control+Option+C | Show or hide the panel |
| Control+Option+X | Cancel the recording |

Option is the key called Alt in the default configuration. Shortcuts are
rebindable in Settings. VoiceOver also uses Control+Option; if you use
VoiceOver, choose different Tiro shortcuts. Command shortcuts are supported.

## Data and acceleration

Configuration, downloaded models, vocabulary, corrections, and diagnostic
logs live in `~/Library/Application Support/dev.tiro.app/`. Transcripts go
to `~/Documents/Tiro/` by default. `TIRO_APP_DIR` overrides the app data
directory, including for headless tests. Nothing is written inside the
signed `.app` bundle.

The first use downloads the selected Whisper model and its voice-activity
model from Hugging Face. Subsequent transcription runs offline. In a Metal
build, Apple Silicon is classified as unified-memory hardware. The existing
power policy keeps the GPU available on battery and switches model sizes
according to your settings. GPU inference stays in a separate worker process.

Launch at login is off by default. Enable it in Settings only after placing
the app in its final location; the login entry refers to that installed copy.

## Verify a build

- The panel and menu bar icon appear, and expand/collapse keeps the panel usable.
- The engine reports the actual CPU/GPU device and power state.
- A spoken sentence reaches both the clipboard and transcript history.
- Cancel discards the take, and push-to-talk stops on release.
- Automatic paste works in another app after granting Accessibility access.
- Relaunching preserves settings and does not create data inside `tiro.app`.

Headless GPU discovery is available without opening a window:

```sh
src-tauri/target/release/bundle/macos/tiro.app/Contents/MacOS/tiro --gpu-enum
```

A Metal build on Apple Silicon should report an Apple GPU with kind
`unified`. A CPU-only build should report an empty array. Diagnostics go to
`tiro.log` in the app data directory.
