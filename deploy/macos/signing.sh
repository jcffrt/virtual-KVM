# shellcheck shell=sh
# Sourced by the install scripts. Signs with the local identity
# "RawKVM Local" (make-signing-identity.sh) when the invoking user has it,
# ad hoc otherwise (ADR 0016).

sign_identity="RawKVM Local"

# sign_user: the user whose login keychain holds the identity ($SUDO_USER
# under sudo, else the current user).
sign_user() {
    if [ "$(id -u)" -eq 0 ]; then echo "${SUDO_USER:-root}"; else id -un; fi
}

# sign_keychain USER: that user's login keychain.
sign_keychain() {
    home=$(dscl . -read "/Users/$1" NFSHomeDirectory | awk '{print $2}')
    echo "$home/Library/Keychains/login.keychain-db"
}

# as_user USER CMD...: run CMD as USER (no-op wrapper if we already are).
as_user() {
    u=$1
    shift
    if [ "$(id -un)" = "$u" ]; then "$@"; else sudo -u "$u" -H "$@"; fi
}

# sign PATH [IDENTIFIER]: sign PATH (a binary or an .app) in place. Bare
# binaries need IDENTIFIER (else codesign uses the file name); an .app takes
# its bundle identifier from Info.plist. PATH must be writable by the
# signing user, so callers sign a copy they own before installing it.
sign() {
    path=$1
    ident=${2:-}
    user=$(sign_user)
    kc=$(sign_keychain "$user")
    if [ "$user" != root ] && as_user "$user" security find-identity -p codesigning "$kc" 2>/dev/null |
        grep -q "\"$sign_identity\""; then
        as_user "$user" codesign --force --deep --sign "$sign_identity" --keychain "$kc" \
            ${ident:+--identifier "$ident"} "$path"
        echo "signed $path with \"$sign_identity\""
    else
        codesign --force --deep --sign - ${ident:+--identifier "$ident"} "$path"
        echo "warning: signed $path ad hoc; macOS forgets its permissions on every update." >&2
        echo "         Run deploy/macos/make-signing-identity.sh once to avoid it." >&2
    fi
}

# signed_adhoc PATH: true if PATH exists and carries an ad hoc signature.
signed_adhoc() {
    [ -e "$1" ] && codesign -dv "$1" 2>&1 | grep -q '^Signature=adhoc'
}

# install_signed SRC DEST IDENTIFIER: copy SRC to a temporary file owned by
# the signing user, sign it there and install it as DEST (root:wheel 0755).
install_signed() {
    # Under sudo, mktemp would use root's private $TMPDIR, which the signing
    # user cannot even traverse: use /tmp.
    tmpdir=$(mktemp -d /tmp/rawkvm-sign.XXXXXX)
    cp "$1" "$tmpdir/bin"
    chown "$(sign_user)" "$tmpdir" "$tmpdir/bin"
    sign "$tmpdir/bin" "$3"
    install -m 0755 -o root -g wheel "$tmpdir/bin" "$2"
    rm -rf "$tmpdir"
}
