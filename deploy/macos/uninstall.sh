#!/bin/sh
# Remove the RawKVM agent and the edge helper (keeps the configs).
set -eu
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
for label in cc.olitec.rawkvm.agent org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon; do
    if [ -f "/Library/LaunchDaemons/$label.plist" ]; then
        launchctl bootout system "/Library/LaunchDaemons/$label.plist" 2>/dev/null || true
        rm -f "/Library/LaunchDaemons/$label.plist"
    fi
done
rm -f /usr/local/bin/rawkvm-agent
# Edge helper (per-user LaunchAgent), if installed by this user.
if [ -n "${SUDO_USER:-}" ]; then
    uid=$(id -u "$SUDO_USER")
    home=$(dscl . -read "/Users/$SUDO_USER" NFSHomeDirectory | awk '{print $2}')
    launchctl bootout "gui/$uid/cc.olitec.rawkvm.edge" 2>/dev/null || true
    rm -f "$home/Library/LaunchAgents/cc.olitec.rawkvm.edge.plist"
fi
rm -f /usr/local/bin/rawkvm-edge
echo "removed"
