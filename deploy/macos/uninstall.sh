#!/bin/sh
# Remove RawKVM from this Mac (ADR 0026). Run it as yourself; it asks for
# your password (in this terminal) for the system parts.
#
#   sh uninstall.sh                agent, edge helper, widget, start at login
#   sh uninstall.sh --purge        also the pairing and the logs; keeps the
#                                  node identity (node_id, node_name), so
#                                  pairing again is the same node for the hub
#   sh uninstall.sh --purge --new-node   also the node identity
#   sh uninstall.sh --karabiner    also Karabiner-DriverKit-VirtualHIDDevice
#
# Keeps the signing identity "RawKVM Local" (Keychain Access removes it).
# Never removes Karabiner-Elements.
set -eu
purge=no
new_node=no
karabiner=no
for a in "$@"; do
    case "$a" in
    --purge) purge=yes ;;
    --new-node) new_node=yes ;;
    --karabiner) karabiner=yes ;;
    -h | --help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "unknown option: $a" >&2; exit 1 ;;
    esac
done
[ "$new_node" = no ] || [ "$purge" = yes ] || { echo "--new-node goes with --purge" >&2; exit 1; }
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

# keep_identity FILE [sudo]: leave only node_id and node_name in FILE (ADR
# 0028): the hub keeps the node's index, its hotkey and its place in the
# layout, which all follow the node_id.
keep_identity() {
    f=$1
    shift
    "$@" test -f "$f" || return 0
    # shellcheck disable=SC2016
    "$@" sh -c 'umask 077 && grep -E "^(node_id|node_name) = " "$1" > "$1.new"; mv "$1.new" "$1"' sh "$f"
}

sys_dir="/Library/Application Support/rawkvm"
user_dir="$HOME/Library/Application Support/rawkvm"
if [ "$purge" = yes ] && [ "$new_node" = yes ]; then
    sudo rm -rf "$sys_dir"
    rm -f "$user_dir/agent.toml" "$user_dir/edge.toml" "$user_dir/widget.toml"
    rmdir "$user_dir" 2>/dev/null || true
elif [ "$purge" = yes ]; then
    keep_identity "$sys_dir/agent.toml" sudo
    keep_identity "$user_dir/agent.toml"
    keep_identity "$user_dir/edge.toml"
    keep_identity "$user_dir/widget.toml"
fi
if [ "$purge" = yes ]; then
    sudo rm -f /var/log/rawkvm-agent.log
    rm -f "$HOME/Library/Logs/rawkvm-edge.log" "$HOME/Library/Logs/rawkvm-widget.log"
    if [ "$new_node" = yes ]; then
        echo "Removed the pairing, the node identity and the logs. Pairing again makes"
        echo "new nodes: sudo rawkvm-hub --forget-node <old id> on the hub releases the"
        echo "old index, and the layout has to be set again in the widget."
    else
        echo "Removed the pairing and the logs. Kept node_id and node_name in"
        echo "$sys_dir and $user_dir: setup.sh pairs again (with the token) as the same nodes."
    fi
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
