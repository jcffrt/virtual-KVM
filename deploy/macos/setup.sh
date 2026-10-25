#!/bin/sh
# RawKVM on macOS, from scratch or again (ADR 0026). Run it as yourself,
# not with sudo; it asks for your password (in this terminal) when it
# needs it. Idempotent: run it again after a restart or after giving a
# permission and it carries on where it stopped.
#
#   sh setup.sh                    install or update, pair if needed, check
#   sh setup.sh --check            only check (changes nothing)
#
# Options:
#   --from DIR          the release tarball and SHA256SUMS, or the unpacked
#                       package, instead of downloading the latest release
#   --version vX.Y.Z    a given release instead of the latest
#   --hub ADDRESS       the hub (else asked; Enter: find it with mDNS)
#   --token-file FILE   the pairing token (else asked, hidden)
#   --pair              pair again even if already paired
#   --skip-led          do not wait for Input Monitoring (Caps Lock LED)
#
# Exit status: 0 done, 1 something failed, 2 restart the Mac and run it
# again.
set -eu

repo=jcffrt/virtual-KVM
share=/usr/local/share/rawkvm
# Where install.sh puts the programs. RAWKVM_BIN: other copies, for --check
# only (tests).
bin=${RAWKVM_BIN:-/usr/local/bin}
agent_label=cc.olitec.rawkvm.agent
edge_label=cc.olitec.rawkvm.edge
daemon_label=org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon
widget_app="$HOME/Applications/RawKVM.app"
widget_exe="$widget_app/Contents/MacOS/rawkvm-widget"
# The file the widget's "Abrir RawKVM al iniciar sesión" switch looks at
# (tauri-plugin-autostart, LaunchAgent mode, named after the app).
widget_autostart="$HOME/Library/LaunchAgents/RawKVM.plist"
widget_log="$HOME/Library/Logs/rawkvm-widget.log"

# Karabiner-DriverKit-VirtualHIDDevice, pinned (docs/macos-instalacion.md).
karabiner_version=8.6.0
karabiner_sha256=ff8c7fdc5e25387c7805fc7509a0fa9cf98f69ba582704f717fddcae47424387
karabiner_team=G43BCU2T37
karabiner_pkg_id=org.pqrs.Karabiner-DriverKit-VirtualHIDDevice
karabiner_manager=/Applications/.Karabiner-VirtualHIDDevice-Manager.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Manager

from=
version=
hub=
token_file=
pair=no
skip_led=no
check_only=no
while [ $# -gt 0 ]; do
    case "$1" in
    --from) from=$2; shift ;;
    --version) version=$2; shift ;;
    --hub) hub=$2; shift ;;
    --token-file) token_file=$2; shift ;;
    --pair) pair=yes ;;
    --skip-led) skip_led=yes ;;
    --check) check_only=yes ;;
    -h | --help) sed -n '2,21p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (sh setup.sh --help)" >&2; exit 1 ;;
    esac
    shift
done

step() { printf '\n== %s\n' "$1"; }
say() { printf '   %s\n' "$1"; }
die() { printf '\nFAIL: %s\n' "$1" >&2; exit 1; }
# A --check line: level (ok, info, warn, FAIL), what, detail.
worst=ok
line() {
    printf '%-4s  %-15s %s\n' "$1" "$2" "$3"
    case "$1" in
    FAIL) worst=FAIL ;;
    warn) [ "$worst" = FAIL ] || worst=warn ;;
    esac
}
here=$(cd "$(dirname "$0")" && pwd)
uid=$(id -u)

# ---------------------------------------------------------------- checks

[ "$uid" -ne 0 ] || die "run it as yourself, without sudo: sh setup.sh"
macos=$(sw_vers -productVersion)
case "${macos%%.*}" in
1[3-9] | [2-9][0-9]) ;;
*) die "macOS $macos: RawKVM needs macOS 13 or later" ;;
esac
if [ "$(uname -m)" != arm64 ] && [ "$check_only" = no ]; then
    die "this package is for Apple silicon; on an Intel Mac build from source (docs/macos-instalacion.md, annex)"
fi

# ---------------------------------------------------------------- check

# The state of every piece, one line each (also at the end of setup).
check() {
    worst=ok
    line info macos "$macos ($(uname -m))"

    ext=$(systemextensionsctl list 2>/dev/null | grep "$karabiner_pkg_id" || true)
    if printf '%s\n' "$ext" | grep -q 'activated enabled'; then
        line ok karabiner "$(pkgutil --pkg-info "$karabiner_pkg_id" 2>/dev/null | sed -n 's/^version: /driver /p'), extension activated and enabled"
    elif [ -n "$ext" ]; then
        line FAIL karabiner "extension $(printf '%s\n' "$ext" | sed -n 's/.*\(\[[^]]*\]\).*/\1/p' | head -1): approve it (setup.sh)"
    else
        line FAIL karabiner "Karabiner-DriverKit-VirtualHIDDevice not installed or not activated (setup.sh)"
    fi

    if security find-identity -v -p codesigning 2>/dev/null | grep -q '"RawKVM Local"'; then
        line ok signing '"RawKVM Local" in the login keychain'
    else
        line warn signing 'no "RawKVM Local": permissions are lost on every update (setup.sh)'
    fi

    # The agent's own lines: version, config (root-only: sudo), hub,
    # Karabiner, LEDs.
    if [ -x "$bin/rawkvm-agent" ]; then
        if sudo -n true 2>/dev/null || [ -t 0 ]; then
            out=$(sudo "$bin/rawkvm-agent" --check 2>&1 || true)
        else
            out=$("$bin/rawkvm-agent" --check 2>&1 || true)
        fi
        printf '%s\n' "$out" | while IFS= read -r l; do
            lvl=${l%% *}
            rest=${l#"$lvl"}
            rest=$(printf '%s' "$rest" | sed 's/^ *//')
            what=${rest%% *}
            detail=$(printf '%s' "${rest#"$what"}" | sed 's/^ *//')
            case "$what" in
            version | config | hub | karabiner | leds) printf '%-4s  %-15s %s\n' "$lvl" "agent $what" "$detail" ;;
            esac
        done
        printf '%s\n' "$out" | grep -q '^FAIL' && worst=FAIL
        printf '%s\n' "$out" | grep -q '^warn' && [ "$worst" != FAIL ] && worst=warn
        printf '%s\n' "$out" | grep -q '^info  version' ||
            line warn agent "$bin/rawkvm-agent has no --check (an older version): setup.sh updates it"
        if launchctl print "system/$agent_label" 2>/dev/null | grep -q 'state = running'; then
            line ok service "$agent_label running"
        else
            line FAIL service "$agent_label not running (setup.sh)"
        fi
    else
        line FAIL agent "$bin/rawkvm-agent missing (setup.sh)"
    fi
    if [ ! -d /Applications/Karabiner-Elements.app ]; then
        if launchctl print "system/$daemon_label" 2>/dev/null | grep -q 'state = running'; then
            line ok daemon "Karabiner VirtualHIDDevice daemon running"
        else
            line FAIL daemon "Karabiner VirtualHIDDevice daemon not running (setup.sh)"
        fi
    fi

    if [ -x "$bin/rawkvm-edge" ]; then
        out=$("$bin/rawkvm-edge" --check 2>&1 || true)
        printf '%s\n' "$out" | while IFS= read -r l; do
            lvl=${l%% *}
            rest=$(printf '%s' "${l#"$lvl"}" | sed 's/^ *//')
            case "$rest" in
            "for node"*) printf '%-4s  %-15s %s\n' "$lvl" "edge node" "$(printf '%s' "${rest#for node}" | sed 's/^ *//')" ;;
            config* | hub*)
                what=${rest%% *}
                printf '%-4s  %-15s %s\n' "$lvl" "edge $what" "$(printf '%s' "${rest#"$what"}" | sed 's/^ *//')"
                ;;
            esac
        done
        printf '%s\n' "$out" | grep -q '^FAIL' && worst=FAIL
        printf '%s\n' "$out" | grep -q '^info  version' ||
            line warn edge "$bin/rawkvm-edge has no --check (an older version): setup.sh updates it"
        if launchctl print "gui/$uid/$edge_label" 2>/dev/null | grep -q 'state = running'; then
            line ok "edge svc" "$edge_label running"
        else
            line FAIL "edge svc" "$edge_label not running (setup.sh)"
        fi
    else
        line FAIL edge "$bin/rawkvm-edge missing (setup.sh)"
    fi

    if [ -x "$widget_exe" ]; then
        line ok widget "$widget_app"
        if [ -f "$widget_autostart" ]; then
            line ok autostart "$widget_autostart"
        else
            line warn autostart 'the widget does not start at login (its switch in Opciones, or setup.sh)'
        fi
        if pgrep -xq rawkvm-widget; then
            line ok running "rawkvm-widget ($(pgrep -x rawkvm-widget | head -1))"
        else
            line warn running "the widget is not running: open $widget_app"
        fi
        state=$(widget_state)
        line "${state%% *}" "widget hub" "${state#* }"
    else
        line FAIL widget "$widget_app missing (setup.sh)"
    fi
    [ "$worst" != FAIL ]
}

# The widget's connection, from its log since it last started (and past
# line $widget_from, set when setup.sh restarts it): "ok ...", "info ...",
# "warn ..." or "FAIL ...".
widget_from=0
widget_state() {
    [ -f "$widget_log" ] || { echo "warn no log yet ($widget_log)"; return; }
    run=$(tail -n +$((widget_from + 1)) "$widget_log" |
        awk '/rawkvm-widget starting/ { buf = "" } { buf = buf $0 "\n" } END { printf "%s", buf }')
    last=$(printf '%s' "$run" | grep -E 'widget connected|No route to host|rejected|certificate' | tail -1)
    case "$last" in
    *"widget connected"*) echo "ok ${last#*INFO }" ;;
    *"No route to host"*)
        if "$bin/rawkvm-edge" --check 2>/dev/null | grep -q '^ok    hub'; then
            echo "FAIL no route to the hub, while the edge helper reaches it: allow Local Network for RawKVM (System Settings > Privacy & Security > Local Network)"
        else
            echo "warn no route to the hub: is it on and on this network?"
        fi
        ;;
    "")
        if printf '%s' "$run" | grep -q 'rawkvm-widget starting'; then
            echo "info connecting (no connection lines yet in $widget_log; check again in a minute)"
        else
            echo "warn no connection lines yet in $widget_log"
        fi
        ;;
    *) echo "FAIL $last" ;;
    esac
}

if [ "$check_only" = yes ]; then
    if check; then exit 0; else exit 1; fi
fi

echo "RawKVM setup for macOS $macos. Your password is asked here, in this terminal, when needed."
sudo -v || die "sudo is needed (your password, in this terminal)"

# ---------------------------------------------------------------- programs

step Programs

# A package folder: the two programs, the app and the install scripts.
is_package() {
    [ -n "$1" ] && [ -f "$1/rawkvm-agent" ] && [ -f "$1/rawkvm-edge" ] && [ -d "$1/RawKVM.app" ] &&
        [ -f "$1/install.sh" ]
}

tmp=$(mktemp -d)
cleanup() {
    stty echo 2>/dev/null || true
    rm -rf "$tmp"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# unpack TARBALL: check it against SHA256SUMS next to it, unpack into
# $tmp and print the package folder.
unpack() {
    sums="$(dirname "$1")/SHA256SUMS"
    name=$(basename "$1")
    if [ -f "$sums" ]; then
        want=$(awk -v n="$name" '$2 == n || $2 == "*" n { print $1 }' "$sums")
        [ -n "$want" ] || die "$sums has no line for $name"
        got=$(shasum -a 256 "$1" | awk '{ print $1 }')
        [ "$got" = "$want" ] || die "$name: SHA-256 $got, SHA256SUMS says $want"
        say "$name: SHA-256 matches SHA256SUMS" >&2
    else
        say "no SHA256SUMS next to $name: not checked" >&2
    fi
    tar -xzf "$1" -C "$tmp"
    dir=$(find "$tmp" -maxdepth 2 -name install.sh -exec dirname {} \; | head -1)
    is_package "$dir" || die "$name is not a RawKVM package for macOS"
    echo "$dir"
}

# download: the release tarball and SHA256SUMS into $tmp/dl; prints the
# tarball's path.
download() {
    mkdir -p "$tmp/dl"
    tag=$version
    if [ -z "$tag" ]; then
        url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest" || true)
        tag=${url##*/tag/}
        case "$url" in */tag/*) ;; *) tag= ;; esac
    fi
    if [ -n "$tag" ]; then
        name="RawKVM-macos-${tag#v}.tar.gz"
        say "downloading $name ($tag)" >&2
        if curl -fsSL -o "$tmp/dl/$name" "https://github.com/$repo/releases/download/$tag/$name" &&
            curl -fsSL -o "$tmp/dl/SHA256SUMS" "https://github.com/$repo/releases/download/$tag/SHA256SUMS"; then
            echo "$tmp/dl/$name"
            return
        fi
    fi
    if command -v gh >/dev/null 2>&1; then
        say "trying gh release download" >&2
        if gh release download ${tag:+"$tag"} -R "$repo" -p 'RawKVM-macos-*.tar.gz' -p SHA256SUMS -D "$tmp/dl" >&2; then
            find "$tmp/dl" -maxdepth 1 -name 'RawKVM-macos-*.tar.gz' | head -1
            return
        fi
    fi
    die "no release of $repo found${tag:+ ($tag)}. Pass --from with the tarball and SHA256SUMS, or the unpacked package"
}

if [ -n "$from" ]; then
    [ -e "$from" ] || die "--from $from: no such file or folder"
    from=$(cd "$from" && pwd)
    if is_package "$from"; then
        pkg=$from
    else
        tarball=$(find "$from" -maxdepth 1 -name 'RawKVM-macos-*.tar.gz' | sort | tail -1)
        [ -n "$tarball" ] || die "$from has neither the package nor RawKVM-macos-*.tar.gz"
        pkg=$(unpack "$tarball")
    fi
elif [ -z "$version" ] && is_package "$here"; then
    # The unpacked package, or the copy in $share (setup.sh run again).
    pkg=$here
else
    tarball=$(download)
    pkg=$(unpack "$tarball")
fi
say "from $pkg"
[ ! -f "$pkg/COMMIT.txt" ] || say "commit $(cut -c1-7 "$pkg/COMMIT.txt")"
# A download carries the quarantine mark: Gatekeeper would stop the
# unsigned programs.
xattr -dr com.apple.quarantine "$pkg" 2>/dev/null || true

# Keep the package where it survives a restart: run setup.sh from there
# to carry on, to check, and to uninstall.
if [ "$pkg" != "$share" ]; then
    sudo rm -rf "$share.new"
    sudo ditto "$pkg" "$share.new"
    sudo rm -rf "$share"
    sudo mv "$share.new" "$share"
    pkg=$share
    say "package kept in $share"
fi
# CI (test-setup.sh): a runner cannot approve a system extension.
[ "${RAWKVM_SETUP_ONLY:-}" != programs ] || exit 0

# ---------------------------------------------------------------- Karabiner

step 'Karabiner-DriverKit-VirtualHIDDevice (virtual keyboard and mouse)'

if [ -d /Applications/Karabiner-Elements.app ]; then
    say "Karabiner-Elements is installed: using its driver and its daemon"
elif ! pkgutil --pkg-info "$karabiner_pkg_id" >/dev/null 2>&1 || [ ! -x "$karabiner_manager" ]; then
    name="Karabiner-DriverKit-VirtualHIDDevice-$karabiner_version.pkg"
    say "downloading $name"
    curl -fsSL -o "$tmp/$name" \
        "https://github.com/pqrs-org/Karabiner-DriverKit-VirtualHIDDevice/releases/download/v$karabiner_version/$name" ||
        die "could not download $name"
    got=$(shasum -a 256 "$tmp/$name" | awk '{ print $1 }')
    [ "$got" = "$karabiner_sha256" ] || die "$name: SHA-256 $got, expected $karabiner_sha256"
    pkgutil --check-signature "$tmp/$name" | grep -q "($karabiner_team)" ||
        die "$name is not signed by its author (team $karabiner_team)"
    say "SHA-256 and signature checked; installing (sudo)"
    sudo installer -pkg "$tmp/$name" -target / >/dev/null
else
    say "driver $(pkgutil --pkg-info "$karabiner_pkg_id" | sed -n 's/^version: //p') installed"
fi

ext_state() {
    systemextensionsctl list 2>/dev/null | grep "$karabiner_pkg_id" || true
}
if ! ext_state | grep -q 'activated enabled' && [ ! -d /Applications/Karabiner-Elements.app ]; then
    say "activating the driver extension"
    sudo "$karabiner_manager" activate >/dev/null 2>&1 || true
    shown=no
    i=0
    while ! ext_state | grep -q 'activated enabled'; do
        s=$(ext_state)
        case "$s" in
        *reboot*)
            say "macOS needs a restart to finish the driver."
            say "Restart the Mac and run again: sh $share/setup.sh"
            exit 2
            ;;
        esac
        if [ "$shown" = no ]; then
            say "macOS asks you to allow it. In System Settings > General > Login Items &"
            say "Extensions > Driver Extensions, switch on \"Karabiner-DriverKit-VirtualHIDDevice\""
            say "(or click Allow in the notice), and enter your password there."
            open "x-apple.systempreferences:com.apple.LoginItems-Settings.extension" 2>/dev/null || true
            say "Waiting for it (up to 5 minutes; Ctrl+C and run setup.sh again later)..."
            shown=yes
        fi
        i=$((i + 1))
        [ "$i" -le 100 ] || die "the driver extension is still not enabled ($s). Allow it and run setup.sh again"
        sleep 3
    done
fi
say "driver extension activated and enabled"

# ---------------------------------------------------------------- signing

step 'Signing identity "RawKVM Local" (keeps the permissions across updates)'

if security find-identity -v -p codesigning 2>/dev/null | grep -q '"RawKVM Local"'; then
    say '"RawKVM Local" already in your login keychain'
else
    sh "$pkg/make-signing-identity.sh"
fi
say 'the first signature asks for the key: choose "Always Allow" (Permitir siempre)'

# ---------------------------------------------------------------- agent

step 'Agent (LaunchDaemon, receives the keyboard and the mouse)'

sudo sh "$pkg/install.sh" "$pkg/rawkvm-agent" | sed 's/^/   /'

agent_paired() {
    sudo "$bin/rawkvm-agent" --check 2>/dev/null | grep -q '^ok    config'
}
if [ "$pair" = yes ] || [ -n "$hub" ] || [ -n "$token_file" ] || ! agent_paired; then
    step Pairing
    if [ -z "$hub" ] && [ -t 0 ]; then
        printf '   Hub address (Enter: find it with mDNS): '
        read -r hub
    fi
    if [ -z "$token_file" ]; then
        [ -t 0 ] || die "no pairing token: pass --token-file FILE (or run in a terminal to type it)"
        while :; do
            printf '   Pairing token (on the hub: sudo rawkvm-hub --show-token; hidden): '
            stty -echo
            read -r token
            stty echo
            echo
            token=$(printf '%s' "$token" | tr -d ' \t' | tr 'A-F' 'a-f')
            if printf '%s' "$token" | grep -Eqx '[0-9a-f]{64}'; then break; fi
            say "That is not 64 hex characters; try again."
        done
        token_file="$tmp/token"
        (umask 077 && printf '%s\n' "$token" > "$token_file")
        token=
    fi
    [ -f "$token_file" ] || die "no token file $token_file"
    sudo launchctl bootout "system/$agent_label" 2>/dev/null || true
    set -- --pair --token-file "$token_file"
    [ -z "$hub" ] || set -- "$@" --hub "$hub"
    # Its verdict is agent_paired below.
    sudo /usr/local/bin/rawkvm-agent "$@" 2>&1 | sed 's/^/   /' || true
    rm -f "$tmp/token"
    # bootout returns before the job is gone: launchd_reload waits for it.
    # shellcheck disable=SC2016
    sudo sh -c '. "$1" && launchd_reload system "$2" "$3"' sh "$pkg/signing.sh" \
        "$agent_label" "/Library/LaunchDaemons/$agent_label.plist" 2>&1 | sed 's/^/   /' || true
    agent_paired || die "pairing failed (see above). With \"No route to host\", allow Local Network for this terminal app in System Settings > Privacy & Security > Local Network; then run setup.sh again"
    say "paired"
fi

# ---------------------------------------------------------------- LEDs

step 'Input Monitoring for the agent (Caps Lock LED on the Logitech keyboard)'

leds_ok() {
    "$bin/rawkvm-agent" --check 2>/dev/null | grep -q '^ok    leds'
}
sudo launchctl kickstart -k "system/$agent_label" 2>/dev/null || true
# Let the new run start its log, or the old run would answer.
sleep 2
i=0
while ! leds_ok && [ "$i" -lt 8 ]; do
    sleep 1
    i=$((i + 1))
done
if leds_ok; then
    say "granted: the agent watches the keyboard LEDs"
elif [ "$skip_led" = yes ]; then
    say "not granted; skipped (--skip-led). Everything else works without it."
else
    # Ad hoc -> "RawKVM Local": the old entry keeps the old code
    # requirement and macOS never asks a daemon again (ADR 0016).
    if log show --last 15m --style compact \
        --predicate 'process == "tccd" AND eventMessage CONTAINS "rawkvm-agent"' 2>/dev/null |
        grep -q 'Failed to match existing code requirement'; then
        say "macOS keeps an Input Monitoring entry for an older signature of the agent"
        say "and will not accept the new one until it is reset. The reset clears Input"
        say "Monitoring for EVERY app (you will have to allow the others again)."
        printf '   Reset Input Monitoring now (sudo tccutil reset ListenEvent)? [y/N] '
        read -r answer
        case "$answer" in
        y | Y | s | S) sudo tccutil reset ListenEvent ;;
        esac
    fi
    say "macOS does not let a script grant it. In the window that opens:"
    say "  System Settings > Privacy & Security > Input Monitoring:"
    say "  drag rawkvm-agent from the Finder window (/usr/local/bin) into the list"
    say "  and switch it on. (The + button cannot enter /usr/local/bin.)"
    open "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent" 2>/dev/null || true
    open -R /usr/local/bin/rawkvm-agent 2>/dev/null || true
    say "Waiting for it (up to 3 minutes; Ctrl+C to stop, or --skip-led)..."
    i=0
    while ! leds_ok && [ "$i" -lt 36 ]; do
        sleep 5
        i=$((i + 1))
        # The agent tries again every 5 s; a restart makes it sure.
        [ $((i % 6)) -ne 0 ] || sudo -n launchctl kickstart -k "system/$agent_label" 2>/dev/null || true
    done
    if leds_ok; then
        say "granted: the agent watches the keyboard LEDs"
    else
        say "still not granted. Everything else works; run setup.sh again after allowing it."
    fi
fi

# ---------------------------------------------------------------- edge

step 'Edge helper (LaunchAgent, switches at the screen edges; no permission needed)'

sudo sh "$pkg/install-edge.sh" "$pkg/rawkvm-edge" | sed 's/^/   /'
i=0
while ! "$bin/rawkvm-edge" --check 2>/dev/null | grep -q '^ok    hub' && [ "$i" -lt 15 ]; do
    sleep 1
    i=$((i + 1))
done

# ---------------------------------------------------------------- widget

step 'Widget (menu bar app)'

# Start at login: the same LaunchAgent the widget's own switch writes, so
# the switch shows it on.
mkdir -p "$HOME/Library/LaunchAgents"
cat > "$widget_autostart" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
  <key>Label</key>
  <string>RawKVM</string>
  <key>AssociatedBundleIdentifiers</key>
  <array></array>
  <key>ProgramArguments</key>
  <array><string>$widget_exe</string></array>
  <key>RunAtLoad</key>
  <true/>
  </dict>
</plist>
PLIST
# Only the new run answers below, not the one install-widget.sh stops.
[ ! -f "$widget_log" ] || widget_from=$(wc -l < "$widget_log")
sh "$pkg/install-widget.sh" "$pkg/RawKVM.app" | sed 's/^/   /'
say "starts at login ($widget_autostart)"

# It takes the pairing from the edge helper (edge.toml) when it starts.
i=0
shown=no
while :; do
    state=$(widget_state)
    case "$state" in ok*) break ;; esac
    case "$state" in
    *"Local Network"*)
        if [ "$shown" = no ]; then
            say "macOS asks \"Allow RawKVM to find devices on local networks?\": Allow."
            say "If it was denied: System Settings > Privacy & Security > Local Network > RawKVM."
            open "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork" 2>/dev/null || true
            say "Waiting for the widget to connect (up to 2 minutes)..."
            shown=yes
        fi
        ;;
    esac
    i=$((i + 1))
    [ "$i" -lt 60 ] || break
    sleep 2
done

# ---------------------------------------------------------------- check

step Check
ok=yes
check || ok=no
echo
echo "Check again at any time:  sh $share/setup.sh --check"
echo "Uninstall:                sh $share/uninstall.sh   (--purge also removes the pairing)"
[ "$ok" = yes ] || exit 1
