#!/bin/sh
# Remove RawKVM from this Mac (ADR 0026). Run it as yourself; it asks for
# your password (in this terminal) for the system parts.
#
#   sh uninstall.sh                agent, edge helper, widget, start at login
#   sh uninstall.sh --purge        also the pairing (configs) and the logs
#   sh uninstall.sh --karabiner    also Karabiner-DriverKit-VirtualHIDDevice
#
# Keeps the signing identity "RawKVM Local" (Keychain Access removes it).
# Never removes Karabiner-Elements.
set -eu
purge=no
karabiner=no
for a in "$@"; do
    case "$a" in
    --purge) purge=yes ;;
    --karabiner) karabiner=yes ;;
    -h | --help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "unknown option: $a" >&2; exit 1 ;;
    esac
done
[ "$(id -u)" -ne 0 ] || { echo "run it as yourself, without sudo: sh uninstall.sh" >&2; exit 1; }
uid=$(id -u)
sudo -v

# Widget: quit it, then the app and its start at login.
osascript -e 'tell application id "cc.olitec.rawkvm.widget" to quit' 2>/dev/null || true
for _ in 1 2 3 4 5 6 7 8 9 10; do
    pgrep -xq rawkvm-widget || break
    sleep 0.5
done
pkill -x rawkvm-widget 2>/dev/null || true
rm -rf "$HOME/Applications/RawKVM.app"
rm -f "$HOME/Library/LaunchAgents/RawKVM.plist"

# Edge helper (LaunchAgent of this user).
launchctl bootout "gui/$uid/cc.olitec.rawkvm.edge" 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/cc.olitec.rawkvm.edge.plist"

# Agent and the Karabiner daemon we started (Karabiner-Elements runs its
# own, which install.sh never touches).
for label in cc.olitec.rawkvm.agent org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon; do
    if [ -f "/Library/LaunchDaemons/$label.plist" ]; then
        sudo launchctl bootout system "/Library/LaunchDaemons/$label.plist" 2>/dev/null || true
        sudo rm -f "/Library/LaunchDaemons/$label.plist"
    fi
done
sudo rm -f /usr/local/bin/rawkvm-agent /usr/local/bin/rawkvm-edge
sudo rm -rf /usr/local/share/rawkvm
echo "Removed the agent, the edge helper, the widget and their start at login."

if [ "$purge" = yes ]; then
    sudo rm -rf "/Library/Application Support/rawkvm"
    sudo rm -f /var/log/rawkvm-agent.log
    user_dir="$HOME/Library/Application Support/rawkvm"
    rm -f "$user_dir/agent.toml" "$user_dir/edge.toml" "$user_dir/widget.toml"
    rmdir "$user_dir" 2>/dev/null || true
    rm -f "$HOME/Library/Logs/rawkvm-edge.log" "$HOME/Library/Logs/rawkvm-widget.log"
    echo "Removed the pairing and the logs. The hub lists this Mac's nodes until"
    echo "sudo rawkvm-hub --forget-node there; setup.sh pairs again (with the token)."
else
    echo "Kept the pairing (/Library/Application Support/rawkvm, ~/Library/Application Support/rawkvm)"
    echo "and the logs: --purge removes them."
fi

if [ "$karabiner" = yes ]; then
    if [ -d /Applications/Karabiner-Elements.app ]; then
        echo "Karabiner-Elements is installed and uses the driver: left alone."
    else
        manager=/Applications/.Karabiner-VirtualHIDDevice-Manager.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Manager
        scripts="/Library/Application Support/org.pqrs/Karabiner-DriverKit-VirtualHIDDevice/scripts/uninstall"
        # Its own uninstaller: deactivate as this user, remove files as root.
        [ ! -x "$manager" ] || "$manager" deactivate || true
        if [ -f "$scripts/remove_files.sh" ]; then
            sudo bash "$scripts/remove_files.sh" || true
        fi
        sudo pkgutil --forget org.pqrs.Karabiner-DriverKit-VirtualHIDDevice >/dev/null 2>&1 || true
        echo "Removed Karabiner-DriverKit-VirtualHIDDevice (macOS may ask to restart to unload it)."
    fi
fi
