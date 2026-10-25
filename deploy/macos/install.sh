#!/bin/sh
# Install the RawKVM agent on macOS. Run with sudo from the repository root:
#   sudo deploy/macos/install.sh [path/to/rawkvm-agent-macos]
# Requires Karabiner-DriverKit-VirtualHIDDevice (see docs/macos-instalacion.md).
set -eu
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
bin=${1:-target/release/rawkvm-agent-macos}
vhid="/Library/Application Support/org.pqrs/Karabiner-DriverKit-VirtualHIDDevice"
daemon_label=org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon
agent_label=cc.olitec.rawkvm.agent

[ -f "$bin" ] || { echo "binary not found: $bin (cargo build --release -p rawkvm-agent-macos)" >&2; exit 1; }
[ -d "$vhid" ] || { echo "Karabiner-DriverKit-VirtualHIDDevice is not installed" >&2; exit 1; }
# Check every plist before stopping anything: a bootout followed by a failed
# install leaves Karabiner's daemon stopped and the Mac without a keyboard.
[ -f "$here/signing.sh" ] || { echo "missing $here/signing.sh: copy the whole deploy/macos folder" >&2; exit 1; }
. "$here/signing.sh"
for plist in "$here/$daemon_label.plist" "$here/$agent_label.plist"; do
    [ -f "$plist" ] || { echo "missing $plist: copy the whole deploy/macos folder" >&2; exit 1; }
    plutil -lint -s "$plist" || { echo "invalid plist: $plist" >&2; exit 1; }
done

# Binary, signed with "RawKVM Local" if the user has it (keeps the Input
# Monitoring grant across versions), ad hoc otherwise.
install -d -m 0755 /usr/local/bin
was_adhoc=no
signed_adhoc /usr/local/bin/rawkvm-agent && was_adhoc=yes
install_signed "$bin" /usr/local/bin/rawkvm-agent "$agent_label"

# System config: reuse the invoking user's pairing if there is one.
sysdir="/Library/Application Support/rawkvm"
install -d -m 0755 "$sysdir"
if [ ! -f "$sysdir/agent.toml" ] && [ -n "${SUDO_USER:-}" ]; then
    user_home=$(dscl . -read "/Users/$SUDO_USER" NFSHomeDirectory | awk '{print $2}')
    user_cfg="$user_home/Library/Application Support/rawkvm/agent.toml"
    if [ -f "$user_cfg" ]; then
        install -m 0600 "$user_cfg" "$sysdir/agent.toml"
        echo "copied pairing from $user_cfg"
    fi
fi
grep -q "^token = \"[0-9a-f]" "$sysdir/agent.toml" 2>/dev/null || echo "not paired yet: setup.sh pairs it (or sudo /usr/local/bin/rawkvm-agent --pair --hub <pi-ip>, which asks for the token)"

reload() { # label plist
    launchd_reload system "$1" "/Library/LaunchDaemons/$1.plist" \
        install -m 0644 -o root -g wheel "$2" "/Library/LaunchDaemons/$1.plist"
}

# Karabiner's daemon does not start by itself. Karabiner-Elements runs its
# own copy; do not start a second one next to it.
if [ -d /Applications/Karabiner-Elements.app ]; then
    echo "Karabiner-Elements found: leaving the VirtualHIDDevice daemon to it"
else
    reload "$daemon_label" "$here/$daemon_label.plist"
fi
reload "$agent_label" "$here/$agent_label.plist"

echo "installed; log: tail -f /var/log/rawkvm-agent.log"

# Ad hoc -> "RawKVM Local": TCC keeps the old code requirement for the
# Input Monitoring entry and never asks a daemon again, so the grant stays
# denied with the switch on (ADR 0016). One-time manual reset.
if [ "$was_adhoc" = yes ] && ! signed_adhoc /usr/local/bin/rawkvm-agent; then
    cat >&2 <<'MSG'
note: the agent was signed ad hoc before; its Input Monitoring grant (Caps Lock LED)
      no longer matches and macOS will not ask again. Once:
        sudo tccutil reset ListenEvent   (clears Input Monitoring for every app)
        drag /usr/local/bin/rawkvm-agent into Input Monitoring again, switch on
        sudo launchctl kickstart -k system/cc.olitec.rawkvm.agent
MSG
fi

# Flush to disk now: boards fed from a PC lose power abruptly, and the SD
# card may otherwise keep these writes in its cache for minutes.
sync
