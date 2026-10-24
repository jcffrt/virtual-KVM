#!/bin/sh
# Install the RawKVM hub on a Linux board with the Logitech receiver
# plugged in: Raspberry Pi OS (Pi 3 B, Pi 4...) or Armbian / Orange Pi OS.
# Run as root from the directory holding the rawkvm-hub binary:
#   sudo sh hub-linux/install.sh ./rawkvm-hub
# The .deb (rawkvm-hub_<version>_arm64.deb) does the same and is the usual
# way; this script stays for boards without dpkg.
# Use the aarch64 binary on 64-bit systems (armv7 needs its own build).
set -eu
[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
here=$(dirname "$0")
model=$(tr -d '\0' 2>/dev/null < /proc/device-tree/model || echo unknown)
echo "board: $model"

install -m 0755 "${1:-rawkvm-hub}" /usr/bin/rawkvm-hub
rm -f /usr/local/bin/rawkvm-hub # before 0.5.0
install -m 0644 "$here/modprobe-rawkvm.conf" /etc/modprobe.d/rawkvm.conf
install -m 0644 "$here/rawkvm-hub.service" /etc/systemd/system/rawkvm-hub.service

# The receiver is probed inside the initramfs on Armbian and on Raspberry
# Pi OS with an initramfs; it carries its own copy of modprobe.d, so the
# blacklist only holds after a reboot once the initramfs is regenerated.
if command -v update-initramfs >/dev/null 2>&1; then
    echo "regenerating the initramfs so the blacklist applies at boot"
    update-initramfs -u
fi

# Unload the DJ drivers now so hid-generic takes the receiver without a
# reboot, then re-plug every Logitech USB device (VID 046D) so it is
# probed again. The hub itself switches the receiver out of DJ mode.
rmmod hid_logitech_hidpp 2>/dev/null || true
rmmod hid_logitech_dj 2>/dev/null || true
found=0
for d in /sys/bus/usb/devices/*; do
    [ -f "$d/idVendor" ] || continue
    [ "$(cat "$d/idVendor")" = 046d ] || continue
    echo "re-plugging $(basename "$d") 046d:$(cat "$d/idProduct") $(cat "$d/product" 2>/dev/null || true)"
    echo 0 > "$d/authorized"; sleep 1; echo 1 > "$d/authorized"
    found=1
done
[ "$found" -eq 1 ] || echo "WARNING: no Logitech USB device found; plug the receiver in (host port)." >&2

# Persistent journal, so that a crash of the board leaves its log behind.
install -d -m 0755 /etc/systemd/journald.conf.d
install -m 0644 "$here/journald-rawkvm.conf" /etc/systemd/journald.conf.d/rawkvm.conf
ramlog=
for svc in armbian-ramlog orangepi-ramlog; do
    if systemctl cat "$svc.service" >/dev/null 2>&1; then ramlog=$svc; break; fi
done
if [ -n "$ramlog" ] && [ -d /var/log.hdd ]; then
    echo "/var/log is in RAM ($ramlog): binding /var/log/journal to /var/log.hdd/journal"
    install -d -m 2755 -g systemd-journal /var/log.hdd/journal
    sed "s/@RAMLOG@/$ramlog/g" "$here/var-log-journal.mount.in" > /etc/systemd/system/var-log-journal.mount
    systemctl daemon-reload
    systemctl enable var-log-journal.mount
    if ! mountpoint -q /var/log/journal; then
        install -d /var/log/journal
        systemctl start var-log-journal.mount
    fi
else
    install -d -m 2755 -g systemd-journal /var/log/journal
fi
systemd-tmpfiles --create --prefix /var/log/journal 2>/dev/null || true
systemctl restart systemd-journald
journalctl --flush

systemctl daemon-reload
systemctl enable --now rawkvm-hub.service
sleep 2
echo "Pairing token:"
/usr/bin/rawkvm-hub --config /etc/rawkvm/hub.toml --show-token

# Flush to disk now: boards fed from a PC lose power abruptly, and the SD
# card may otherwise keep these writes in its cache for minutes.
sync
