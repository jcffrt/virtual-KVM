#!/bin/sh
# CI test of setup.sh and uninstall.sh on a macOS runner (macos-tar.yml),
# from a folder with the release tarball and SHA256SUMS:
#   sh deploy/macos/test-setup.sh dist
# A runner cannot approve the Karabiner system extension, so setup.sh
# stops after the programs step (RAWKVM_SETUP_ONLY=programs); the rest is
# tested by --check failing cleanly and by uninstall.sh.
set -u
dist=$(cd "$1" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
share=/usr/local/share/rawkvm
failures=0
pass() { echo "pass  $1"; }
fail() { echo "FAIL  $1"; failures=$((failures + 1)); }
# expect WHAT CMD...: CMD must succeed. refuse WHAT CMD...: it must fail.
expect() {
    what=$1
    shift
    if "$@"; then pass "$what"; else fail "$what"; fi
}
refuse() {
    what=$1
    shift
    if "$@"; then fail "$what"; else pass "$what"; fi
}
# run LOG CMD...: run CMD without stdin, show its output; CMD's status.
run() {
    log=$1
    shift
    "$@" </dev/null >"$log" 2>&1
    code=$?
    sed 's/^/    | /' "$log"
    return "$code"
}
tarball=$(find "$dist" -maxdepth 1 -name 'RawKVM-macos-*.tar.gz' | head -1)
tmp=$(mktemp -d)

echo "== a corrupt tarball is refused"
mkdir "$tmp/bad"
cp "$tarball" "$dist/SHA256SUMS" "$tmp/bad/"
printf 'x' >>"$tmp/bad/$(basename "$tarball")"
refuse "exit code is not 0" run "$tmp/bad.log" sh "$here/setup.sh" --from "$tmp/bad"
expect "says SHA-256" grep -q 'SHA-256' "$tmp/bad.log"
refuse "nothing installed" test -e "$share"

echo "== the programs step from the tarball"
expect "exit code 0" run "$tmp/ok.log" env RAWKVM_SETUP_ONLY=programs sh "$here/setup.sh" --from "$dist"
expect "tarball checked against SHA256SUMS" grep -q 'matches SHA256SUMS' "$tmp/ok.log"
for f in setup.sh uninstall.sh install.sh install-edge.sh install-widget.sh signing.sh \
    make-signing-identity.sh rawkvm-agent rawkvm-edge RawKVM.app/Contents/MacOS/rawkvm-widget \
    cc.olitec.rawkvm.agent.plist cc.olitec.rawkvm.edge.plist \
    org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon.plist LEEME.md COMMIT.txt; do
    expect "$share/$f" test -e "$share/$f"
done
refuse "no quarantine mark" sh -c "xattr -r '$share' 2>/dev/null | grep -q com.apple.quarantine"
expect "rawkvm-agent --version: $("$share/rawkvm-agent" --version)" sh -c \
    "'$share/rawkvm-agent' --version | grep -Eq '^rawkvm-agent-macos [0-9]+\.[0-9]+\.[0-9]+ \([0-9a-f]{7}\)$'"
expect "rawkvm-edge --version: $("$share/rawkvm-edge" --version)" sh -c \
    "'$share/rawkvm-edge' --version | grep -Eq '^rawkvm-edge-macos [0-9]+\.[0-9]+\.[0-9]+ \([0-9a-f]{7}\)$'"

echo "== the kept copy runs again from its own folder"
expect "exit code 0" run "$tmp/again.log" env RAWKVM_SETUP_ONLY=programs sh "$share/setup.sh"
expect "uses $share" grep -q "from $share" "$tmp/again.log"

echo "== --check on a Mac without RawKVM fails cleanly"
refuse "exit code is not 0" run "$tmp/check.log" env RAWKVM_BIN="$share" sh "$share/setup.sh" --check
expect "karabiner line" grep -q '^FAIL  karabiner' "$tmp/check.log"
expect "agent version line" grep -q '^info  agent version' "$tmp/check.log"
expect "widget line" grep -q '^FAIL  widget' "$tmp/check.log"
refuse "no shell errors" grep -qiE 'syntax error|command not found|parameter not set' "$tmp/check.log"

echo "== uninstall.sh --purge"
expect "exit code 0" run "$tmp/uninstall.log" sh "$share/uninstall.sh" --purge
refuse "$share removed" test -e "$share"

rm -rf "$tmp"
[ "$failures" -eq 0 ] || { echo "$failures check(s) failed"; exit 1; }
echo "all checks passed"
