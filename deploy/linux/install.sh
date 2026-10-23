#!/bin/sh
# Install the Linux agent for the current user. Uses sudo for the udev rule.
# Usage: install.sh [path/to/rawkvm-agent]
# Then pair once:  ~/.local/bin/rawkvm-agent --hub <pi-ip> --token <hex>
set -eu
here=$(dirname "$0")
bin=${1:-rawkvm-agent}
sudo install -m 0644 "$here/99-rawkvm-uhid.rules" /etc/udev/rules.d/99-rawkvm-uhid.rules
sudo install -m 0644 "$here/rawkvm-uhid.conf" /etc/modules-load.d/rawkvm-uhid.conf
sudo modprobe uhid
sudo udevadm control --reload
sudo udevadm trigger --name-match=uhid
if ! id -nG | grep -qw input; then
    sudo usermod -aG input "$USER"
    echo "Added $USER to group input: log out and back in before starting the agent."
fi
install -D -m 0755 "$bin" "$HOME/.local/bin/rawkvm-agent"

# GNOME extension for screen-edge switching (docs/gnome-plan.md). Looked up
# next to this script, in a sibling gnome-extension/ (copied with scp) or in
# the repository's gnome-extension/ directory.
ext=rawkvm@olitec.cc
for d in "$here/$ext" "$here/../gnome-extension/$ext" "$here/../../gnome-extension/$ext"; do
    [ -f "$d/metadata.json" ] && ext_src=$d && break
done
if [ -n "${ext_src:-}" ] && command -v gnome-shell >/dev/null 2>&1; then
    # GNOME refuses an extension whose metadata lacks its major version
    # ("OUT OF DATE"): say so instead of installing it silently.
    shell_major=$(gnome-shell --version | sed -n 's/^GNOME Shell \([0-9]*\).*/\1/p')
    if ! grep -q "\"$shell_major\"" "$ext_src/metadata.json"; then
        echo "WARNING: GNOME Shell $shell_major is not in shell-version of $ext_src/metadata.json:" >&2
        echo "         the extension would show as OUT OF DATE and stay off (docs/gnome-plan.md)." >&2
    fi
    dest="$HOME/.local/share/gnome-shell/extensions/$ext"
    mkdir -p "$dest"
    cp "$ext_src/metadata.json" "$ext_src/extension.js" "$dest/"
    # Add it to the enabled list; gnome-shell loads new extensions only at
    # the next login under Wayland.
    python3 - "$ext" <<'PY'
import ast, subprocess, sys
ext = sys.argv[1]
cur = subprocess.run(["gsettings", "get", "org.gnome.shell", "enabled-extensions"],
                     capture_output=True, text=True).stdout.strip()
items = ast.literal_eval(cur.replace("@as ", "")) if cur else []
if ext not in items:
    items.append(ext)
    subprocess.run(["gsettings", "set", "org.gnome.shell", "enabled-extensions", str(items)], check=True)
PY
    gnome-extensions enable "$ext" 2>/dev/null || true
    echo "GNOME extension $ext installed: log out and back in once to load it."
else
    echo "No GNOME Shell or extension sources found: edge switching disabled on this machine."
fi
install -D -m 0644 "$here/rawkvm-agent.service" "$HOME/.config/systemd/user/rawkvm-agent.service"
systemctl --user daemon-reload
systemctl --user enable rawkvm-agent.service
echo "Pair once with: ~/.local/bin/rawkvm-agent --hub <pi-ip> --token <hex>  (Ctrl+C after 'connected')"
echo "Then: systemctl --user start rawkvm-agent"

# Flush to disk now: boards fed from a PC lose power abruptly, and the SD
# card may otherwise keep these writes in its cache for minutes.
sync
