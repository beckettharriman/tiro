# Tiro on iOS — prompt for the agent on Trevor's Mac

Beckett: everything below the line is the prompt. Copy from `---BEGIN PROMPT---` to
`---END PROMPT---` and paste it into the agent session on Trevor's Mac.
Two things only you can decide, and the agent will ask if you don't pre-answer them
in the paste:

1. **Apple ID / signing route.** The vault has no record of you owning a paid Apple
   Developer account (checked mail, wiki, journals 2026-09-16). Two real routes:
   - **Free Apple ID (yours or Trevor's), Xcode personal team.** Zero cost. The build
     installs only onto a phone that has been plugged into Trevor's Mac once (USB, then
     Wi-Fi works). App expires after 7 days and must be reinstalled. You bring your
     iPhone to Trevor's.
   - **Paid Apple Developer Program ($99/yr, enroll from the Apple Developer app on
     your iPhone, usually approved within a day).** Builds go out via TestFlight; you
     install from anywhere, no cable, 90-day builds, and later a real App Store path.
   Tell the agent which one. Never type an Apple ID password into the chat; the
   sign-in happens once in Xcode's own Accounts window on Trevor's Mac.
2. **Repo write access.** The agent will push to branch `ios-keyboard-whisper` on
   `github.com/beckettharriman/tiro`. Either add Trevor as a collaborator or let it
   push to a fork and open a PR (the prompt already allows both).

---BEGIN PROMPT---

You are running on Trevor's Apple Silicon Mac. Trevor is lending the machine; the
work is for Beckett. You have full control and autonomy on this Mac to install
whatever you need and make every technical call yourself. Do not wait for more
instructions. Start by getting oriented, then write yourself a /loop goal and run it
self-paced until the stop condition below is met.

## The goal, in Beckett's own words (verbatim)

"get tiro to work on ios, i want to be able to tap the globe keyboard switcher icon
when typing on the native ios keyboard and switch to a tiro record start/stop with
whisper flow running on whisper models directly on ios. dont stop until this is
proven testable by me on my iphone"

Unpacked precisely, the deliverable is:

- A real **iOS Custom Keyboard Extension** (the standard mechanism that puts a
  third-party keyboard in the globe-icon rotation next to the native keyboard),
  shipped inside a small container app called Tiro.
- The keyboard shows a **record start/stop control**. Tap to start, speak, tap to
  stop, and the transcribed text is inserted at the cursor in whatever app the user
  is typing in.
- Transcription runs **on-device with a Whisper model**. No cloud API, no network
  call. It must work with the phone in airplane mode.
- It is **installed and running on Beckett's physical iPhone**, not just a simulator.
  Simulators cannot record audio and do not enforce the extension memory limit, so
  nothing counts as proven until it runs on real hardware.

## Stop condition

You are done only when Beckett can, on his own iPhone: add "Tiro" under Settings >
General > Keyboard > Keyboards, enable Allow Full Access, open any text field, tap
the globe until the Tiro keyboard appears, tap record, speak a sentence, tap stop,
and see the sentence appear at the cursor, with airplane mode on. Until you have
either (a) done that yourself on a real iPhone connected to this Mac, or (b) produced
a TestFlight build Beckett confirms he installed and used, you are not done. Write
the exact on-phone steps for him when you hand off.

## Trevor's one hard constraint: stay out of his way

This is Trevor's daily machine and he is using it. Everything you run must stay in
the background and never commandeer his screen:

- Use the CLI for everything: `xcodebuild`, `xcrun devicectl`, `xcrun simctl`,
  `xcrun coremlcompiler`, `xcodes` for installing Xcode itself. Do not launch the
  Xcode GUI, Simulator.app, or any window. Never use `osascript` or accessibility
  automation to click things. Never take focus.
- Keep all your files under one directory (suggest `~/tiro-ios/`). Homebrew installs
  and Rust toolchains are fine; do not touch Trevor's shell config, dotfiles, login
  items, or global git config beyond what a build strictly needs.
- Some steps genuinely require a human once: accepting the Xcode license (`sudo`),
  signing an Apple ID into Xcode > Settings > Accounts, trusting the Mac on the
  iPhone, enabling Developer Mode on the iPhone. Batch these into ONE short list,
  present it once, and do everything else without interrupting. Ask for `sudo` only
  when unavoidable and say exactly what it is for.
- No noisy notifications. Report progress in the chat, not on the desktop.

## What already exists: reuse, do not rebuild

Clone the real repo; do not work from this description alone:

    git clone https://github.com/beckettharriman/tiro.git
    cd tiro && git checkout ios-keyboard-whisper

Tiro is Beckett's local/offline dictation tool. The core is Rust (`src-tauri/src/`,
about 13.6k lines): `transcribe.rs` runs Whisper through `whisper-rs` 0.16 (bindings
to whisper.cpp) with a `metal` cargo feature; `flow.rs`, `vocab.rs`, `cues.rs`,
`config.rs` hold the dictation flow, custom vocabulary and corrections; `gpu_worker.rs`
runs inference in a separate process. Windows and Linux builds ship; macOS is a Tauri
menu-bar app using Metal. Read `MACOS.md`, `BUILDING.md`, `PORTING_NOTES.md` and
`README.md` first. The macOS Metal path is the closest relative of what you need:
whisper.cpp with the Metal backend already builds against Apple hardware there.

Decide deliberately how much to reuse. The keyboard extension must be a native Swift
target no matter what (Tauri does not make extensions). Options, roughly in order of
plausibility: whisper.cpp directly via its own Swift package or a static library;
the Rust core cross-compiled for `aarch64-apple-ios` as a staticlib with a C ABI and
the flow/vocab logic reused; WhisperKit (Core ML). Research which is actually proven
on iPhone hardware before committing, then pick one and go.

## Verified facts to build on (checked 2026-09-16; re-verify anything that matters)

1. **Keyboard extensions can record from the microphone**, but only if all three are
   true: the extension's Info.plist has `RequestsOpenAccess = YES`; the user has
   turned on Allow Full Access for the keyboard; and the container app has already
   requested and been granted microphone permission (extensions cannot show
   permission dialogs). Wispr Flow's iOS keyboard works exactly this way. If any piece
   is missing, `AVAudioSession` activation fails with `561145187` / `561015905` (Apple
   forum thread 775077 documents this). Build the container app's first-run flow to
   collect mic permission before anything else.
2. **The keyboard extension memory ceiling is roughly 50-60 MB phys_footprint.** Cross
   it and jetsam kills the extension silently: no crash log, iOS just flips back to
   the previous keyboard. This is the central engineering risk of the whole project.
   Measure `phys_footprint` on the device early with a model loaded, not at the end.
   Two designs exist; choose by measurement:
   - In-extension inference with a small quantized model (whisper tiny or base,
     q5/q8 ggml, or a Core ML encoder). Simplest UX, everything happens in the
     keyboard. Must fit the ceiling with audio buffers and compute scratch included.
   - The split used by github.com/fmachta/WhisperBoard: the extension only records
     (~20 MB), writes PCM to an App Group container, and the container app
     transcribes with WhisperKit. The catch is that iOS suspends the container app in
     the background, so this only works if you solve keep-alive or accept a hop into
     the app. Do not pick this without a real answer to that.
3. **Signing without a paid account is possible.** A free Apple ID in Xcode gives a
   personal team: `xcodebuild -allowProvisioningUpdates` with automatic signing can
   build and `xcrun devicectl device install app` can install onto an iPhone that has
   been paired with this Mac. Limits: 7-day expiry, the device must be registered by
   plugging it in, and some capabilities (App Groups in particular) may be unavailable
   on personal teams. Verify that before designing around an App Group; bundling the
   model inside the extension avoids the need. A paid Developer Program account lifts
   all of that and enables TestFlight. Beckett will tell you which route (see the
   header he pasted, or ask him once).
4. Model files: whisper.cpp ggml models come from Hugging Face
   (`ggerganov/whisper.cpp`); tiny is ~75 MB fp16 / ~31 MB q5_1, base is ~142 MB fp16
   / ~57 MB q5_1. Do not commit model files to git. Decide whether to bundle a model
   in the extension or download it in the container app on first run, keeping the
   memory ceiling and the App Group question in mind.

## Working method

- Work on branch `ios-keyboard-whisper`. Commit early and often with clear messages.
  Push to origin if you have access; if not, push to a fork and open a PR against
  `beckettharriman/tiro`. Put the Xcode project under `ios/` in the repo and document
  the build in a new `IOS.md` in the same style as `MACOS.md`.
- Prove each layer on real hardware before stacking the next: (1) empty keyboard
  appears in the globe rotation; (2) keyboard records audio with Full Access;
  (3) Whisper transcribes a fixed WAV inside the extension under the memory ceiling;
  (4) end-to-end record, transcribe, insert; (5) airplane-mode test; (6) install path
  Beckett can repeat.
- Never put a password, token or certificate into the chat or into git. Apple ID
  sign-in happens in Xcode's Accounts window once; App Store Connect API keys, if
  used, live in `~/.config` with mode 600 and are referenced by path.
- If something is blocked (account, hardware, an Apple limitation), finish every part
  that is not blocked, say precisely what is blocked and what Beckett or Trevor must
  do, and keep going on the rest. Do not narrow the goal on your own.
- When you hand off, give Beckett: the on-phone steps, what was proven on which
  device and iOS version, the measured memory footprint and transcription latency for
  the model you shipped, the reinstall cadence (7-day or TestFlight), and the known
  limitations.

---END PROMPT---
