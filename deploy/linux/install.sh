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
install -D -m 0644 "$here/rawkvm-agent.service" "$HOME/.config/systemd/user/rawkvm-agent.service"
systemctl --user daemon-reload
systemctl --user enable rawkvm-agent.service
echo "Pair once with: ~/.local/bin/rawkvm-agent --hub <pi-ip> --token <hex>  (Ctrl+C after 'connected')"
echo "Then: systemctl --user start rawkvm-agent"

# Flush to disk now: boards fed from a PC lose power abruptly, and the SD
# card may otherwise keep these writes in its cache for minutes.
sync
