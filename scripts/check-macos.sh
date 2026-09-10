#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Darwin ]; then
    echo "Run this check on macOS." >&2
    exit 1
fi

backend="${1:-metal}"
features=(--no-default-features)
case "$backend" in
    metal) features=(--features metal); build_script=build:mac ;;
    cpu) build_script=build:mac:cpu ;;
    *) echo "Usage: $0 [metal|cpu]" >&2; exit 1 ;;
esac

cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo test --manifest-path src-tauri/Cargo.toml --locked "${features[@]}"
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets "${features[@]}" -- -D warnings
npm ci
npm run "$build_script"

# Cloud-backed Documents folders can attach FinderInfo after signing.
# Verify a clean copy outside that folder; never change system protection.
stage="$(mktemp -d "${TMPDIR:-/tmp}/tiro-check.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
ditto --norsrc src-tauri/target/release/bundle/macos/tiro.app "$stage/tiro.app"
xattr -dr com.apple.FinderInfo "$stage/tiro.app" 2>/dev/null || true
codesign --verify --deep --strict "$stage/tiro.app"
TIRO_APP_DIR="$stage/data" "$stage/tiro.app/Contents/MacOS/tiro" --gpu-enum
