#!/bin/sh
# Install the RawKVM agent on Ubuntu from a release, then pair it:
#   curl -fsSL https://raw.githubusercontent.com/jcffrt/virtual-KVM/main/deploy/linux/setup-linux.sh | sh
#   sh setup-linux.sh [--version vX.Y.Z] [--from DIR] [-- options of rawkvm-agent setup]
# Run as your own user: it asks for sudo only for `apt install`.
# Download (ADR 0021): --from DIR, else curl of the GitHub release
# (GH_TOKEN if set), else `gh release download`. Checked against SHA256SUMS.
set -eu
repo=jcffrt/virtual-KVM
version=
from=
die() { echo "setup-linux: $*" >&2; exit 1; }
while [ $# -gt 0 ]; do
    case $1 in
    --version) version=${2:?--version needs vX.Y.Z}; shift 2 ;;
    --from) from=${2:?--from needs a directory}; shift 2 ;;
    --) shift; break ;;
    *) break ;;
    esac
done
[ "$(id -u)" -ne 0 ] || die "run as your own user, not root (it uses sudo for apt)"
command -v dpkg >/dev/null 2>&1 || die "this installer is for Debian / Ubuntu"
arch=$(dpkg --print-architecture)
[ "$arch" = amd64 ] || die "no package for $arch (amd64 only)"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# apt reads the file as user _apt.
chmod 755 "$work"

# The last rawkvm-agent package in directory $1, by name.
find_deb() {
    found=
    for f in "$1"/rawkvm-agent_*_"$arch".deb; do
        [ -e "$f" ] && found=$(basename "$f")
    done
    [ -n "$found" ] && echo "$found"
}

fetch_release() {
    api=https://api.github.com/repos/$repo/releases/latest
    [ -n "$version" ] && api=https://api.github.com/repos/$repo/releases/tags/$version
    if [ -n "${GH_TOKEN:-}" ]; then
        json=$(curl -fsSL -H "Authorization: Bearer $GH_TOKEN" "$api") || return 1
    else
        json=$(curl -fsSL "$api") || return 1
    fi
    tag=$(printf '%s\n' "$json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$tag" ] || return 1
    deb=rawkvm-agent_${tag#v}_$arch.deb
    base=https://github.com/$repo/releases/download/$tag
    curl -fL -o "$work/$deb" "$base/$deb" && curl -fsSL -o "$work/SHA256SUMS" "$base/SHA256SUMS"
}

if [ -n "$from" ]; then
    deb=$(find_deb "$from") || die "no rawkvm-agent_*_$arch.deb in $from"
    cp "$from/$deb" "$from/SHA256SUMS" "$work/"
elif ! fetch_release; then
    if ! command -v gh >/dev/null 2>&1 || ! gh auth status >/dev/null 2>&1; then
        die "could not download the release (no network, or no release yet)"
    fi
    echo "setup-linux: downloading with gh"
    gh release download ${version:+"$version"} --repo "$repo" -D "$work" \
        -p "rawkvm-agent_*_$arch.deb" -p SHA256SUMS
    deb=$(find_deb "$work") || die "gh downloaded no package"
fi

# --ignore-missing would accept a file that is not listed: check its line.
(cd "$work" && grep " $deb\$" SHA256SUMS | sha256sum -c -) || die "$deb does not match SHA256SUMS"
sudo apt-get install -y "$work/$deb"
# Interactive (hub choice, hidden token) even when piped from curl.
exec /usr/bin/rawkvm-agent setup "$@" </dev/tty
