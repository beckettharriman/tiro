# Transcribe a file from the panel — idea branch `mp3-upload-idea`

Exploratory pass, 2026-09-22/23. Not production; a working prototype plus
the questions a real version has to answer. Everything here is on this
branch only.

## The idea in one line

Dictation is a *take*: audio in, text out, copied, logged, shown in the
panel. A file is just a take whose audio came from disk instead of the
microphone. So the branch adds exactly one new input source and nothing
else changes.

```
 mic  ──cpal──▶ f32 @ capture rate ──resample_to_16k──▶ ┐
                                                         ├─▶ transcribe_worker
 file ──ffmpeg (-ac 1 -ar 16000 -f f32le)──▶ f32 @ 16k ─┘      │
                                                                ├─ engine: GPU worker (beam 5) or CPU model (beam 1)
                                                                ├─ vocab prompt, cleanup, corrections
                                                                ├─ clipboard copy
                                                                ├─ transcript log (jsonl + md)
                                                                └─ panel entry (tiroAddEntry), pill, cue
```

`flow::transcribe_worker` did not change except to return its `Outcome`.
The file path builds a `Take { samples, rate: 16_000, mic_name: <file name> }`
and calls it with a fresh session, exactly like `stop_recording` does.

## What was built

| Piece | Where |
|---|---|
| ffmpeg decode to the take buffer, `--transcribe-file` CLI | `src-tauri/src/import.rs` |
| file take through the live pipeline, panel progress pushes | `flow::transcribe_file`, `flow::push_import` |
| Tauri command (native picker or a given path) | `lib.rs::transcribe_file`, `api::transcribe_file` |
| drop a file on the panel | `lib.rs` `on_window_event` → `DragDrop::Drop` |
| ffmpeg availability in `get_state` (`import.available`) | `api::get_state`, `import::ffmpeg_status` |
| file button (compact row + advanced toolbar), import strip | `ui/index.html`, `ui/app.js`, `ui/styles.css` |
| browser-preview mock of the whole flow | `MockApi.transcribe_file` in `ui/app.js` |
| headless pipeline: URL or file → transcript on disk | `scripts/transcribe-file.sh`, BUILDING.md |

### Panel surfaces

The file button sits between the mic capsule and the record button (and
next to the record button in the advanced Transcribe toolbar). It opens
the native picker filtered to audio/video, any file allowed — ffmpeg
decides. A file dropped anywhere on the panel skips the picker.

While a file is in flight a strip above the list shows the phase:
`Decoding…`, then `<length> · Transcribing on GPU…`, and on failure the
reason (ffmpeg's own message for a bad file, "Model not ready", "No
speech found", …) with a dismiss button. The pill shows "transcribing"
and the tray icon changes exactly as for a take; the finished entry
lands at the top of the list with its duration, and the text is on the
clipboard.

Screenshots (browser mock, `ui/index.html` over `file://`):

- `mp3-upload-idea/03-import-transcribing.png` — strip in the compact panel
- `mp3-upload-idea/04-import-done-entry.png` — the 12-minute file as an entry
- `mp3-upload-idea/05-import-error.png` — an unreadable file
- `mp3-upload-idea/06-advanced-transcribe-view.png` — the advanced toolbar

### Refusals

An import is refused (with a reason in the strip) while recording or
while another transcription is in flight. Reason: a take's session id is
what makes an in-flight transcription discard itself when superseded
(the STATE-2 check in `transcribe_worker`), and a file must never cost a
live take.

## What was measured

Test machine: 12-thread laptop, RTX 2070 Max-Q (8 GB) via the Vulkan
worker, Fedora 44. Source: Theo's "Jev is incredible"
(youtube.com/watch?v=F3YXg7AaKWE), 30:29 of speech.

- 20 s clip, base.en on GPU (existing `--gpu-test`): 0.6 s.
- 20 s clip, large-v3 (f16, 3.1 GB) on GPU, beam 5: worker ready in
  6.1 s, transcribed in 16.6 s. large-v3 fixed a real base.en error in
  that clip ("ages that have an inch" → "age instead of an int").
- Full 30:29 video, large-v3 on GPU, beam 5: decoded in 3.0 s, worker
  ready in 3.5 s, transcribed in 513 s (3.56× realtime once warm), 6,615
  words. Whole pipeline from URL to markdown, including the 28 MB
  download, 517 s. Output:
  `transcribe-test/F3YXg7AaKWE-jev-is-incredible.md` (and `.txt`, `.log`).

Decoding a 42 MB mp3 with ffmpeg takes about 3 s; the resulting 16 kHz
f32 buffer is 117 MB for half an hour (~230 MB per hour), which is the
same memory profile as a live take of that length.

## Open questions for a real version

1. **A new take kills a long import.** Starting a recording while a
   file is transcribing bumps the session, and the file's transcription
   discards itself on completion — by design for dictation, ruinous for a
   20-minute file. A real version should either refuse/queue recording
   while an import is in flight, or give imports their own lane that the
   dictation session does not supersede.
2. **Progress.** A half-hour file sits on "Transcribing…" for minutes.
   whisper.cpp has a progress callback; the CPU path can report it
   directly, the worker protocol would need a progress frame. The strip
   already has the slot for a percentage.
3. **Clipboard.** A take copies its text; so does a file, which means a
   30-minute transcript lands on the clipboard. Probably right for short
   files, questionable for long ones. Alternative: skip the copy above
   some length and offer "Copy" / "Open transcript" on the entry.
4. **Quality policy for files.** Files are not latency-sensitive. The
   pipeline uses whatever serves right now (battery → CPU + light model
   + beam 1). A file could always use the plugged-in model and beam 5, or
   offer a model pick in the picker flow.
5. **Mark file entries.** The log records the file name in the `mic`
   column, but the panel does not show mic anywhere, so a file entry
   looks like a take. Adding a `source` field to `store::Rec` (with a
   serde default for old lines) lets the entry show the file name.
6. **Timeouts.** The GPU worker allows 2× realtime + 30 s per request.
   large-v3 on this card runs about 1.2× realtime, fine; a big model on
   an iGPU could exceed it on a long file and fall back to CPU
   mid-take. Files may want a looser factor. The CPU path has no timeout.
7. **ffmpeg.** System ffmpeg on PATH (or `FFMPEG=…`), probed once per
   process; the button explains itself when it is missing. Bundling a
   decoder (symphonia, or ffmpeg next to the worker) is the
   no-dependency alternative.
8. **Very long files.** No chunking: the whole file is one whisper call
   (whisper.cpp windows it internally). Memory is linear (~230 MB/h in
   the app, plus the copy written to the worker's stdin). Multi-hour
   files would want streaming into the worker.

## What was and was not tested

- **Tested:** the decode + engine code through `tiro --transcribe-file`
  on a 20 s clip and the full 30-minute video (GPU, large-v3), and the
  script end to end from a YouTube URL; the whole panel flow in the
  browser mock (button, strip phases, error, dismiss, advanced view);
  the Rust build is warning-free; `node --check` on the JS.
- **Not tested live:** the Tauri command, the native picker, and
  drag-and-drop inside the running app. A production Tiro was running
  from the main worktree during this pass and the single-instance
  plugin forwards any second launch to it, so this build's panel was
  never opened. Those three need a run with the production instance
  quit: `cd src-tauri && cargo build --release && ./target/release/tiro`.
