#!/bin/sh
# Install the RawKVM USB gadget agent on a Raspberry Pi (Raspberry Pi OS)
# or an Armbian board. Idempotent. Run as root:
#   sudo sh install.sh ./rawkvm-agent-gadget [--hub IP --token HEX]
set -eu
[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
here=$(dirname "$0")
bin=${1:?usage: install.sh path/to/rawkvm-agent-gadget [--hub IP --token HEX]}
shift
reboot_needed=0

model=$(tr -d '\0' < /proc/device-tree/model 2>/dev/null || echo unknown)
echo "board: $model"

# 1. Raspberry Pi: put the OTG controller (dwc2) in device mode.
case "$model" in
*"Raspberry Pi 3 Model B"*)
    echo "WARNING: on the Pi 3 B/B+ the OTG port sits behind the USB hub/Ethernet chip; gadget mode cannot work." >&2 ;;
esac
case "$model" in
*"Raspberry Pi"*)
    cfg=/boot/firmware/config.txt
    [ -f "$cfg" ] || cfg=/boot/config.txt
    if ! grep -q '^dtoverlay=dwc2' "$cfg"; then
        printf '\n[all]\ndtoverlay=dwc2,dr_mode=peripheral\n' >> "$cfg"
        echo "added dtoverlay=dwc2,dr_mode=peripheral to $cfg"
        reboot_needed=1
    fi
    modules="dwc2 libcomposite" ;;
*)
    # Armbian / others: the UDC driver is usually built in; the OTG port
    # must be in peripheral mode in the device tree (see docs).
    modules="libcomposite" ;;
esac

# 2. Modules at boot, and now.
printf '%s\n' $modules > /etc/modules-load.d/rawkvm-gadget.conf
for m in $modules; do modprobe "$m" 2>/dev/null || reboot_needed=1; done
mountpoint -q /sys/kernel/config || mount -t configfs none /sys/kernel/config

# 3. Binary, config, service.
install -m 0755 "$bin" /usr/local/bin/rawkvm-agent-gadget
install -d -m 0700 /etc/rawkvm
/usr/local/bin/rawkvm-agent-gadget --config /etc/rawkvm/agent.toml --configure-only "$@"
install -m 0644 "$here/rawkvm-agent-gadget.service" /etc/systemd/system/rawkvm-agent-gadget.service
systemctl daemon-reload
systemctl enable rawkvm-agent-gadget.service

if [ "$reboot_needed" -eq 1 ]; then
    echo "Reboot needed (dwc2 overlay / modules). The service starts after the reboot."
else
    systemctl restart rawkvm-agent-gadget.service
    echo "started; log: journalctl -u rawkvm-agent-gadget -f"
fi
ls /sys/class/udc 2>/dev/null | sed 's/^/UDC: /' || echo "no UDC yet"
