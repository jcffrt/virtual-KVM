#!/bin/sh
# Remove the RawKVM agent (keeps the config in /Library/Application Support/rawkvm).
set -eu
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
for label in cc.olitec.rawkvm.agent org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon; do
    if [ -f "/Library/LaunchDaemons/$label.plist" ]; then
        launchctl bootout system "/Library/LaunchDaemons/$label.plist" 2>/dev/null || true
        rm -f "/Library/LaunchDaemons/$label.plist"
    fi
done
rm -f /usr/local/bin/rawkvm-agent
echo "removed"
