#!/bin/sh
# Install, check, remove and purge the Linux agent's .deb in an Ubuntu
# container (CI, debs.yml): test-install-agent.sh DEB_DIR
# Starts from the /etc files of the manual install before 0.5.0.
set -eu
deb=$(echo "$1"/rawkvm-agent_*_amd64.deb)

fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo "== $*"; }

apt-get update -qq

step "the manual install's files in /etc"
install -d /etc/udev/rules.d /etc/modules-load.d
echo "# Let members of group input create virtual HID devices (RawKVM agent)." > /etc/udev/rules.d/99-rawkvm-uhid.rules
echo '# Load uhid at boot so /dev/uhid exists for the RawKVM agent.' > /etc/modules-load.d/rawkvm-uhid.conf

step "install (dependencies from apt)"
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$deb" </dev/null | tee /tmp/install.log
grep -q 'rawkvm-agent setup' /tmp/install.log || fail "no hint to run setup"
[ ! -e /etc/udev/rules.d/99-rawkvm-uhid.rules ] || fail "old udev rule left"
[ ! -e /etc/modules-load.d/rawkvm-uhid.conf ] || fail "old module list left"
for f in /usr/bin/rawkvm-agent /usr/lib/udev/rules.d/70-rawkvm-uhid.rules \
    /usr/lib/modules-load.d/rawkvm-uhid.conf /usr/lib/systemd/user/rawkvm-agent.service \
    /usr/share/gnome-shell/extensions/rawkvm@olitec.cc/metadata.json \
    /usr/share/gnome-shell/extensions/rawkvm@olitec.cc/extension.js; do
    [ -f "$f" ] || fail "missing $f"
done
grep -q '^ExecStart=/usr/bin/rawkvm-agent$' /usr/lib/systemd/user/rawkvm-agent.service || fail "unit path"

step "the binary runs on this glibc"
/usr/bin/rawkvm-agent --version

step "--check as a user, before setup"
useradd -m tester
if su tester -c 'rawkvm-agent --check' > /tmp/check.log 2>&1; then
    fail "--check passed without a config"
fi
cat /tmp/check.log
grep -q '^FAIL  config' /tmp/check.log || fail "--check does not report the missing config"
grep -q '^info  version' /tmp/check.log || fail "--check prints no version"

step "setup refuses root"
if rawkvm-agent setup </dev/null > /tmp/root.log 2>&1; then fail "setup ran as root"; fi
grep -q 'not with sudo' /tmp/root.log || fail "no message for root: $(cat /tmp/root.log)"

step "remove and purge"
dpkg -r rawkvm-agent
[ ! -e /usr/bin/rawkvm-agent ] || fail "binary left"
dpkg -P rawkvm-agent
[ ! -e /usr/share/gnome-shell/extensions/rawkvm@olitec.cc ] || fail "extension left"
echo "all agent package checks passed"
