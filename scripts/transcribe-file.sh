#!/usr/bin/env bash
# transcribe-file.sh — any audio file (or a YouTube URL) -> transcript on
# disk, through Tiro's own whisper pipeline.
#
#   scripts/transcribe-file.sh <file-or-url> [--model NAME] [--device gpu|cpu]
#                              [--beam N] [--out DIR]
#
# What happens:
#   1. A URL is fetched with yt-dlp as an mp3 (ffmpeg does the conversion).
#      A local file of any format is used as-is.
#   2. `tiro --transcribe-file` decodes it in-process to the 16 kHz mono
#      buffer a live take uses and runs it through the same engine code as
#      dictation (CPU in-process, or the tiro-gpu-worker child on GPU).
#   3. The verbatim transcript lands in <out>/<stem>.txt, the run log in
#      <out>/<stem>.log, and <out>/<stem>.md wraps the text with a header
#      (source, model, device, timing).
#
# Needs: a built tiro (src-tauri/target/release/tiro, or $TIRO_BIN), ffmpeg,
# and yt-dlp for URLs (`pipx install yt-dlp`). Models live under the app dir
# (src-tauri/models for a source build; TIRO_APP_DIR relocates it); a model
# that is not there yet downloads first. Defaults: the config's model, CPU.
set -euo pipefail

usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

[ $# -ge 1 ] || usage 1
INPUT=$1; shift
MODEL=""; DEVICE="cpu"; BEAM=""; OUT="$PWD/transcribe-out"
while [ $# -gt 0 ]; do
  case "$1" in
    --model)  MODEL=$2; shift 2 ;;
    --device) DEVICE=$2; shift 2 ;;
    --beam)   BEAM=$2; shift 2 ;;
    --out)    OUT=$2; shift 2 ;;
    -h|--help) usage ;;
    *) echo "unknown option: $1" >&2; usage 1 ;;
  esac
done

HERE=$(cd "$(dirname "$0")" && pwd)
TIRO=${TIRO_BIN:-"$HERE/../src-tauri/target/release/tiro"}
[ -x "$TIRO" ] || { echo "tiro binary not found at $TIRO (build with: cd src-tauri && cargo build --release), or set TIRO_BIN" >&2; exit 1; }
command -v ffmpeg >/dev/null || { echo "ffmpeg is required" >&2; exit 1; }
mkdir -p "$OUT"

SOURCE="$INPUT"
if [[ "$INPUT" =~ ^https?:// ]]; then
  command -v yt-dlp >/dev/null || { echo "yt-dlp is required for URLs (pipx install yt-dlp)" >&2; exit 1; }
  YTARGS=()
  # yt-dlp wants a JS runtime for YouTube; deno is its default, node works too.
  if ! command -v deno >/dev/null && command -v node >/dev/null; then YTARGS+=(--js-runtimes node); fi
  echo "fetching audio: $INPUT" >&2
  ID=$(yt-dlp "${YTARGS[@]}" --print id "$INPUT")
  META="$OUT/$ID.meta"
  yt-dlp "${YTARGS[@]}" -x --audio-format mp3 --audio-quality 0 \
    -o "$OUT/%(id)s.%(ext)s" --no-simulate \
    --print-to-file "title: %(title)s" "$META" \
    --print-to-file "uploader: %(uploader)s" "$META" \
    --print-to-file "upload_date: %(upload_date)s" "$META" \
    --print-to-file "duration_s: %(duration)s" "$META" \
    --print-to-file "url: %(webpage_url)s" "$META" \
    "$INPUT" >&2
  AUDIO="$OUT/$ID.mp3"
else
  [ -f "$INPUT" ] || { echo "no such file: $INPUT" >&2; exit 1; }
  AUDIO="$INPUT"
  META=""
fi

STEM=$(basename "${AUDIO%.*}")
TXT="$OUT/$STEM.txt"; LOG="$OUT/$STEM.log"; MD="$OUT/$STEM.md"
ARGS=(--transcribe-file "$AUDIO" --device "$DEVICE")
[ -n "$MODEL" ] && ARGS+=(--model "$MODEL")
[ -n "$BEAM" ] && ARGS+=(--beam "$BEAM")

# A release tiro sends its stderr to tiro.log in the app dir (see main.rs),
# so the run's diagnostics are captured from there: the lines appended
# during this run become <stem>.log. A debug build prints to stderr instead.
APPDIR=${TIRO_APP_DIR:-"$(cd "$(dirname "$TIRO")/../.." && pwd)"}
[ -d "$APPDIR/models" ] || APPDIR="${XDG_DATA_HOME:-$HOME/.local/share}/tiro"
TLOG="$APPDIR/tiro.log"
BEFORE=0; [ -f "$TLOG" ] && BEFORE=$(wc -l < "$TLOG")

echo "transcribing $AUDIO on $DEVICE${MODEL:+ with $MODEL} ..." >&2
START=$(date +%s)
RC=0; "$TIRO" "${ARGS[@]}" > "$TXT" 2> "$LOG.stderr" || RC=$?
TOOK=$(( $(date +%s) - START ))
{ cat "$LOG.stderr"; [ -f "$TLOG" ] && tail -n +"$((BEFORE + 1))" "$TLOG"; } > "$LOG" 2>/dev/null || true
rm -f "$LOG.stderr"
[ "$RC" = 0 ] || { echo "transcription failed (exit $RC); see $LOG" >&2; grep -E 'ERROR|failed' "$LOG" | tail -5 >&2; exit 1; }

# Header + text. The log's summary lines carry the model/device/timing.
{
  echo "# Transcript: $STEM"
  echo
  echo "- source: $SOURCE"
  [ -n "$META" ] && sed 's/^/- /' "$META"
  echo "- audio: $AUDIO"
  grep -E '^(decoded|model|GPU worker|CPU model|transcribed) ' "$LOG" | sed 's/^/- /' || true
  echo "- wall clock: ${TOOK}s"
  echo "- generated: $(date '+%Y-%m-%d %H:%M %Z') by scripts/transcribe-file.sh"
  echo
  echo "## Text"
  echo
  # One paragraph per sentence-ish chunk so a 30-minute wall of text is readable.
  sed -E 's/([.?!]) +/\1\n\n/g' "$TXT"
} > "$MD"

echo "done in ${TOOK}s -> $MD" >&2
echo "$MD"
