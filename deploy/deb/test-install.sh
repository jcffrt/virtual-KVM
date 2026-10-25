#!/bin/sh
# Install, migrate, upgrade, remove and purge the .debs in a Debian
# container without systemd running (CI, debs.yml):
#   test-install.sh DEB_DIR REPO_DIR
# Starts from a copy of the manual install of the Pi before 0.5.0.
set -eu
debs=$1
repo=$2
hub=$(ls "$debs"/rawkvm-hub_*.deb)
gadget=$(ls "$debs"/rawkvm-agent-gadget_*.deb)
token=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo "== $*"; }

apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq systemd >/dev/null

step "the manual install of the Pi"
install -d /usr/local/bin /etc/systemd/system/multi-user.target.wants /etc/rawkvm /var/lib/rawkvm
echo 'old binary' > /usr/local/bin/rawkvm-hub
echo 'old binary' > /usr/local/bin/rawkvm-agent-gadget
sed 's|/usr/bin/rawkvm-hub|/usr/local/bin/rawkvm-hub|' "$repo/deploy/hub-linux/rawkvm-hub.service" \
    > /etc/systemd/system/rawkvm-hub.service
ln -s /etc/systemd/system/rawkvm-hub.service /etc/systemd/system/multi-user.target.wants/
cat > /etc/systemd/system/rawkvm-agent-gadget.service <<UNIT
[Service]
ExecStart=/usr/local/bin/rawkvm-agent-gadget --config /etc/rawkvm/agent.toml --udc-wait 60 --host-os linux --no-hidpp
UNIT
install -d /etc/systemd/system/rawkvm-hub.service.d
printf '[Service]\nEnvironment=RUST_LOG=debug\n' > /etc/systemd/system/rawkvm-hub.service.d/debug.conf
install -d /etc/modprobe.d /etc/systemd/journald.conf.d
cp "$repo/deploy/hub-linux/modprobe-rawkvm.conf" /etc/modprobe.d/rawkvm.conf
cp "$repo/deploy/hub-linux/journald-rawkvm.conf" /etc/systemd/journald.conf.d/rawkvm.conf
cat > /etc/rawkvm/hub.toml <<TOML
# RawKVM hub configuration.
name = "orangepi3"
hub_id = "6a1f3c52-1d6e-4b8a-9c4e-2f0d5b7a8e91"
token = "$token"
listen = "0.0.0.0:4433"

[[node_options]]
index = 1
swap_alt_gui = true
TOML
echo 'old v3 config' > /etc/rawkvm/hub.toml.bak-v3
cat > /etc/rawkvm/agent.toml <<TOML
node_id = "2b0e8a51-7f3c-4d2e-8a6b-1c9d0e4f5a73"
node_name = "orangepi3-gadget"
hub = "127.0.0.1"
token = "$token"
TOML
echo 'layout' > /var/lib/rawkvm/layout.toml
echo 'cert' > /var/lib/rawkvm/cert.der
# Orange Pi OS keeps /var/log in RAM (orangepi-ramlog): one array set,
# one missing.
install -d /etc/default
printf 'ENABLED=true\nSIZE=50M\nUSE_RSYNC=true\nXTRA_RSYNC_TO=(--exclude foo)\n' > /etc/default/orangepi-ramlog
cp /etc/default/orangepi-ramlog /tmp/ramlog.before
cp /etc/rawkvm/hub.toml /tmp/hub.toml.before
cp /etc/rawkvm/agent.toml /tmp/agent.toml.before

step "install: the move from the manual install"
# </dev/null: a conffile question would fail instead of waiting.
dpkg -i "$hub" "$gadget" </dev/null
[ ! -e /usr/local/bin/rawkvm-hub ] || fail "old hub binary left"
[ ! -e /usr/local/bin/rawkvm-agent-gadget ] || fail "old gadget binary left"
[ ! -e /etc/systemd/system/rawkvm-hub.service ] || fail "old hub unit left"
[ ! -e /etc/systemd/system/multi-user.target.wants/rawkvm-hub.service ] || fail "old enable link left"
[ ! -e /etc/systemd/system/rawkvm-agent-gadget.service ] || fail "old gadget unit left"
[ -f /etc/systemd/system/rawkvm-hub.service.d/debug.conf ] || fail "drop-in removed"
cmp /etc/rawkvm/hub.toml /tmp/hub.toml.before || fail "hub.toml changed"
cmp /etc/rawkvm/agent.toml /tmp/agent.toml.before || fail "agent.toml changed"
[ -f /etc/rawkvm/hub.toml.bak-v3 ] || fail "backup removed"
[ -f /var/lib/rawkvm/layout.toml ] || fail "state removed"
[ -f /var/lib/rawkvm/cert.der ] || fail "state removed"
grep -qx 'RAWKVM_GADGET_ARGS="--host-os linux --no-hidpp"' /etc/default/rawkvm-agent-gadget ||
    fail "gadget options not carried over: $(cat /etc/default/rawkvm-agent-gadget)"
grep -q '^ExecStart=/usr/bin/rawkvm-hub ' /lib/systemd/system/rawkvm-hub.service || fail "hub unit path"
netwatch_link=/etc/systemd/system/multi-user.target.wants/rawkvm-netwatch.service
[ -L "$netwatch_link" ] || fail "Wi-Fi watchdog not enabled"
[ -f /etc/default/rawkvm-netwatch ] || fail "watchdog options missing"
/usr/bin/rawkvm-netwatch --version
# No such interface here: nothing to ping, and it changes nothing.
/usr/bin/rawkvm-netwatch --check --iface rawkvm-none || fail "rawkvm-netwatch --check"
grep -qx 'XTRA_RSYNC_TO=(--exclude foo --exclude journal) # rawkvm-hub' /etc/default/orangepi-ramlog ||
    fail "ramlog TO: $(cat /etc/default/orangepi-ramlog)"
grep -qx 'XTRA_RSYNC_FROM=(--exclude journal) # rawkvm-hub' /etc/default/orangepi-ramlog ||
    fail "ramlog FROM: $(cat /etc/default/orangepi-ramlog)"
grep -qx 'SIZE=50M' /etc/default/orangepi-ramlog || fail "ramlog settings lost"

step "the new hub reads the existing hub.toml"
[ "$(/usr/bin/rawkvm-hub --show-token)" = "$token" ] || fail "--show-token"
/usr/bin/rawkvm-agent-gadget --version
cmp /etc/rawkvm/hub.toml /tmp/hub.toml.before || fail "hub.toml rewritten by --show-token"

step "upgrade (same version again) keeps a disabled watchdog disabled"
systemctl disable rawkvm-netwatch.service
dpkg -i "$hub" "$gadget" </dev/null
[ ! -e "$netwatch_link" ] || fail "upgrade enabled the watchdog again"
[ "$(grep -c '^RAWKVM_GADGET_ARGS=' /etc/default/rawkvm-agent-gadget)" = 1 ] || fail "options added twice"
[ "$(grep -c 'exclude journal' /etc/default/orangepi-ramlog)" = 2 ] || fail "ramlog excludes added twice"

step "remove keeps the config, the token and the certificate"
dpkg -r rawkvm-hub rawkvm-agent-gadget
[ ! -e /usr/bin/rawkvm-hub ] || fail "binary left after remove"
cmp /etc/rawkvm/hub.toml /tmp/hub.toml.before || fail "hub.toml removed"
[ -f /etc/rawkvm/agent.toml ] || fail "pairing removed"
[ -f /var/lib/rawkvm/cert.der ] || fail "pairing removed"

step "purge deletes only what belongs to each package"
dpkg -P rawkvm-hub
[ ! -e /etc/rawkvm/hub.toml ] || fail "hub purge left its files"
[ ! -e /var/lib/rawkvm ] || fail "hub purge left its files"
[ -f /etc/rawkvm/agent.toml ] || fail "hub purge removed the gadget's agent.toml"
[ -f /etc/rawkvm/hub.toml.bak-v3 ] || fail "hub purge removed the backup"
cmp /etc/default/orangepi-ramlog /tmp/ramlog.before || fail "purge did not restore orangepi-ramlog: $(cat /etc/default/orangepi-ramlog)"
[ ! -e /etc/default/rawkvm-netwatch ] || fail "purge left the watchdog options"
[ ! -e /var/lib/rawkvm-netwatch ] || fail "purge left the watchdog state"
dpkg -P rawkvm-agent-gadget
[ ! -e /etc/rawkvm/agent.toml ] || fail "gadget purge left agent.toml"
[ ! -e /etc/modprobe.d/rawkvm.conf ] || fail "purge left the blacklist"

step "fresh install on a clean system"
rm -rf /etc/rawkvm
dpkg -i "$hub" "$gadget" </dev/null | tee /tmp/fresh.log
grep -q -- '--pair --hub' /tmp/fresh.log || fail "no pairing hint for the gadget"
[ ! -e /etc/rawkvm/hub.toml ] || fail "the package must not ship hub.toml"
/usr/bin/rawkvm-hub --show-token | grep -Eq '^[0-9a-f]{64}$' || fail "first start makes no token"
[ -L "$netwatch_link" ] || fail "fresh install did not enable the Wi-Fi watchdog"
echo "all package checks passed"
