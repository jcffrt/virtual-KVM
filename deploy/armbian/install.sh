#!/bin/sh
# Install the hub on the Orange Pi. Run as root from the directory holding
# the rawkvm-hub binary and this deploy/armbian folder.
set -eu
here=$(dirname "$0")
install -m 0755 "${1:-rawkvm-hub}" /usr/local/bin/rawkvm-hub
install -m 0644 "$here/modprobe-rawkvm.conf" /etc/modprobe.d/rawkvm.conf
install -m 0644 "$here/rawkvm-hub.service" /etc/systemd/system/rawkvm-hub.service
# Unload the DJ drivers now so hid-generic takes the receiver without a reboot.
rmmod hid_logitech_hidpp 2>/dev/null || true
rmmod hid_logitech_dj 2>/dev/null || true
for d in /sys/bus/usb/devices/*; do
    if [ -f "$d/idVendor" ] && [ "$(cat "$d/idVendor")" = 046d ] && grep -qiE 'c52b|c532|c534|c539|c53f|c547|c548' "$d/idProduct"; then
        echo 0 > "$d/authorized"; sleep 1; echo 1 > "$d/authorized"
    fi
done
systemctl daemon-reload
systemctl enable --now rawkvm-hub.service
sleep 2
echo "Pairing token:"
/usr/local/bin/rawkvm-hub --config /etc/rawkvm/hub.toml --show-token
