#!/bin/sh
# Create, once, the local code-signing identity "RawKVM Local" in your login
# keychain. Run it as yourself (not with sudo); it asks for sudo only to trust
# the certificate for code signing:
#   deploy/macos/make-signing-identity.sh
# The install scripts sign with it when it exists. Binaries signed with the
# same certificate keep the same designated requirement from one version to
# the next, so macOS keeps the Local Network and Input Monitoring grants
# (ADR 0016). Safe to run again.
set -eu
[ "$(id -u)" -ne 0 ] || { echo "run as yourself, without sudo" >&2; exit 1; }
name="RawKVM Local"
keychain="$HOME/Library/Keychains/login.keychain-db"
# LibreSSL from the system: its PKCS#12 output imports into the keychain
# (OpenSSL 3 would need -legacy).
openssl=/usr/bin/openssl

if security find-certificate -c "$name" "$keychain" >/dev/null 2>&1; then
    echo "\"$name\" already in $keychain"
else
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    cat > "$tmp/cert.cnf" <<'EOF'
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = RawKVM Local
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF
    "$openssl" req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$tmp/cert.cnf" \
        -keyout "$tmp/key.pem" -out "$tmp/cert.pem" 2>/dev/null
    # security import rejects a PKCS#12 with an empty password: use a
    # throwaway one that never leaves this script.
    pass=$("$openssl" rand -hex 16)
    "$openssl" pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" -name "$name" \
        -passout "pass:$pass" -out "$tmp/id.p12"
    security import "$tmp/id.p12" -k "$keychain" -P "$pass" -T /usr/bin/codesign >/dev/null
    echo "created \"$name\" in $keychain"
fi

if security find-identity -v -p codesigning "$keychain" | grep -q "\"$name\""; then
    echo "\"$name\" is trusted for code signing"
else
    cert=$(mktemp)
    security find-certificate -c "$name" -p "$keychain" > "$cert"
    echo "trusting \"$name\" for code signing (sudo)"
    sudo security add-trusted-cert -d -r trustRoot -p codeSign -k "$keychain" "$cert"
    rm -f "$cert"
fi
echo "done. The first signature asks for access to the key: choose \"Always Allow\"."
