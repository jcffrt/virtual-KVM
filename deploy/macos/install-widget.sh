#!/bin/sh
# Install the RawKVM widget for yourself (no sudo):
#   deploy/macos/install-widget.sh [path/to/RawKVM.app]
# Copies the app to ~/Applications, signs it with "RawKVM Local" if you have
# it (make-signing-identity.sh; keeps the Local Network grant across
# versions, ADR 0016) and starts it again.
set -eu
[ "$(id -u)" -ne 0 ] || { echo "run as yourself, without sudo" >&2; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
app=${1:-widget/src-tauri/target/release/bundle/macos/RawKVM.app}
dest="$HOME/Applications/RawKVM.app"

[ -d "$app" ] || { echo "app not found: $app (cd widget && npx tauri build --bundles app)" >&2; exit 1; }
[ -f "$here/signing.sh" ] || { echo "missing $here/signing.sh: copy the whole deploy/macos folder" >&2; exit 1; }
. "$here/signing.sh"

# Sign a copy first: if signing fails, the installed app is left alone.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
ditto "$app" "$tmp/RawKVM.app"
xattr -dr com.apple.quarantine "$tmp/RawKVM.app" 2>/dev/null || true
sign "$tmp/RawKVM.app"

osascript -e 'tell application id "cc.olitec.rawkvm.widget" to quit' 2>/dev/null || true
for _ in 1 2 3 4 5 6 7 8 9 10; do
    pgrep -xq rawkvm-widget || break
    sleep 0.5
done
mkdir -p "$HOME/Applications"
rm -rf "$dest"
ditto "$tmp/RawKVM.app" "$dest"
[ ! -d /Applications/RawKVM.app ] || echo "note: an older copy is still in /Applications; remove it so that only $dest is used"
open "$dest"
echo "installed $dest"
