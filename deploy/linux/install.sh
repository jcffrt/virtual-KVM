#!/bin/sh
# Install the Linux agent by hand (same paths as the rawkvm-agent .deb,
# which is the usual way): sudo for the system files, then
# `rawkvm-agent setup` as the user.
# Usage: sh install.sh [path/to/rawkvm-agent]
set -eu
here=$(dirname "$0")
bin=${1:-rawkvm-agent}
ext=rawkvm@olitec.cc
for d in "$here/$ext" "$here/../gnome-extension/$ext" "$here/../../gnome-extension/$ext"; do
    if [ -f "$d/metadata.json" ]; then ext_src=$d; break; fi
done
sudo install -m 0755 "$bin" /usr/bin/rawkvm-agent
sudo install -m 0644 "$here/70-rawkvm-uhid.rules" /usr/lib/udev/rules.d/70-rawkvm-uhid.rules
sudo install -m 0644 "$here/rawkvm-uhid.conf" /usr/lib/modules-load.d/rawkvm-uhid.conf
sudo install -m 0644 "$here/rawkvm-agent.service" /usr/lib/systemd/user/rawkvm-agent.service
if [ -n "${ext_src:-}" ]; then
    sudo install -d /usr/share/gnome-shell/extensions/$ext
    sudo install -m 0644 "$ext_src/metadata.json" "$ext_src/extension.js" /usr/share/gnome-shell/extensions/$ext/
fi
# Before 0.5.0: rule and module list in /etc.
sudo rm -f /etc/udev/rules.d/99-rawkvm-uhid.rules /etc/modules-load.d/rawkvm-uhid.conf
sudo modprobe uhid
sudo udevadm control --reload
sudo udevadm trigger --name-match=uhid
/usr/bin/rawkvm-agent setup
