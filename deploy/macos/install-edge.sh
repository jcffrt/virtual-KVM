#!/bin/sh
# Install the RawKVM edge helper for the user who runs it with sudo:
#   sudo deploy/macos/install-edge.sh [path/to/rawkvm-edge-macos]
# Needs the agent installed and paired first (deploy/macos/install.sh): the
# helper takes hub, token, pinned certificate and the agent's node_id from
# /Library/Application Support/rawkvm/agent.toml.
set -eu
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
user=${SUDO_USER:-}
[ -n "$user" ] && [ "$user" != root ] || { echo "run with sudo from your own account" >&2; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
bin=${1:-target/release/rawkvm-edge-macos}
label=cc.olitec.rawkvm.edge
agent_cfg="/Library/Application Support/rawkvm/agent.toml"

[ -f "$bin" ] || { echo "binary not found: $bin (cargo build --release -p rawkvm-edge-macos)" >&2; exit 1; }
[ -f "$agent_cfg" ] || { echo "$agent_cfg not found: install and pair the agent first" >&2; exit 1; }
# Check the folder before stopping anything (see install.sh).
[ -f "$here/signing.sh" ] || { echo "missing $here/signing.sh: copy the whole deploy/macos folder" >&2; exit 1; }
[ -f "$here/$label.plist" ] || { echo "missing $here/$label.plist: copy the whole deploy/macos folder" >&2; exit 1; }
plutil -lint -s "$here/$label.plist" || { echo "invalid plist: $here/$label.plist" >&2; exit 1; }
. "$here/signing.sh"
uid=$(id -u "$user")
home=$(dscl . -read "/Users/$user" NFSHomeDirectory | awk '{print $2}')

install -d -m 0755 /usr/local/bin
install_signed "$bin" /usr/local/bin/rawkvm-edge "$label"

# Helper config, owned by the user (the agent's is root-only).
cfg_dir="$home/Library/Application Support/rawkvm"
cfg="$cfg_dir/edge.toml"
sudo -u "$user" mkdir -p "$cfg_dir" "$home/Library/Logs" "$home/Library/LaunchAgents"
/usr/local/bin/rawkvm-edge --config "$cfg" --from-agent "$agent_cfg"
chown "$user" "$cfg"
chmod 0600 "$cfg"

plist="$home/Library/LaunchAgents/$label.plist"
launchctl bootout "gui/$uid/$label" 2>/dev/null || true
sed "s|__HOME__|$home|g" "$here/$label.plist" > "$plist"
chown "$user" "$plist"
chmod 0644 "$plist"
launchctl bootstrap "gui/$uid" "$plist"

echo "installed; log: tail -f ~/Library/Logs/rawkvm-edge.log"
echo "diagnostics: rawkvm-edge --show"

# Flush to disk now: boards fed from a PC lose power abruptly, and the SD
# card may otherwise keep these writes in its cache for minutes.
sync
