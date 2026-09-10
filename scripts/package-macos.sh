#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Darwin ]; then
    echo "Build the macOS installer on a Mac." >&2
    exit 1
fi

backend="${1:-metal}"
case "$backend" in
    metal) build_script=build:mac ;;
    cpu) build_script=build:mac:cpu ;;
    *) echo "Usage: $0 [metal|cpu]" >&2; exit 1 ;;
esac

# Ship the corresponding source alongside the app. A clean checkout keeps
# that source snapshot consistent with the executable being distributed.
if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
    echo "Commit or stash source changes before creating a distributable installer." >&2
    exit 1
fi
npm run "$build_script"
if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
    echo "The build changed source files; commit them and package again." >&2
    exit 1
fi

stage="$(mktemp -d "${TMPDIR:-/tmp}/tiro-package.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/volume" "$stage/artifacts" dist/macos
app="$stage/volume/tiro.app"

# Stage outside cloud-synced source folders, which can attach FinderInfo
# after signing. Only remove the two attributes that invalidate a signature.
ditto --norsrc src-tauri/target/release/bundle/macos/tiro.app "$app"
xattr -dr com.apple.FinderInfo "$app" 2>/dev/null || true
xattr -dr com.apple.ResourceFork "$app" 2>/dev/null || true
codesign --verify --deep --strict "$app"

version="$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")"
case "$version" in
    ''|*[!0-9A-Za-z.+-]*) echo "Invalid bundle version: $version" >&2; exit 1 ;;
esac
arch="$(lipo -archs "$app/Contents/MacOS/tiro")"
case "$arch" in
    arm64|x86_64) ;;
    'x86_64 arm64'|'arm64 x86_64') arch=universal ;;
    *) echo "Unsupported bundle architecture: $arch" >&2; exit 1 ;;
esac
stem="Tiro-${version}-macOS-${arch}-${backend}"
revision="$(git rev-parse HEAD)"

cp LICENSE "$stage/volume/LICENSE.txt"
cp macos/Install.txt "$stage/volume/Read Me.txt"
git archive --format=tar.gz --prefix="tiro-${version}/" HEAD > "$stage/volume/Source.tar.gz"
printf 'Version: %s\nArchitecture: %s\nBackend: %s\nSource commit: %s\n' \
    "$version" "$arch" "$backend" "$revision" > "$stage/volume/Build.txt"

# A self-contained ZIP alternative, including the license and source.
ditto --norsrc -c -k "$stage/volume" "$stage/artifacts/$stem.zip"
ln -s /Applications "$stage/volume/Applications"
hdiutil create -quiet -volname "Tiro $version" -fs HFS+ -format UDZO \
    -srcfolder "$stage/volume" "$stage/artifacts/$stem.dmg"
hdiutil verify -quiet "$stage/artifacts/$stem.dmg"
(
    cd "$stage/artifacts"
    shasum -a 256 "$stem.dmg" "$stem.zip" > "$stem.sha256"
)
cp "$stage/artifacts/$stem.dmg" "$stage/artifacts/$stem.zip" \
    "$stage/artifacts/$stem.sha256" dist/macos/
printf '\nInstaller: %s/dist/macos/%s.dmg\nZIP: %s/dist/macos/%s.zip\n' \
    "$PWD" "$stem" "$PWD" "$stem"
