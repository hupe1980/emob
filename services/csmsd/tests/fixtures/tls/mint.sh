#!/usr/bin/env bash
# Mint a throwaway PKI for csmsd's TLS tests and the local demo.
#
#   mint.sh [OUT_DIR]   (default: this directory)
#
# A CA, a loopback server certificate (127.0.0.1, ::1, localhost), client
# certificates for CP-1 and CP-2 under that CA (security profile 3), and a
# rogue CA with its own "CP-1" — the certificate a forger would present.
# EC P-256, ten years, no passphrase. Nothing here is a secret: these keys
# protect nothing but a test.
set -euo pipefail
out="${1:-$(cd "$(dirname "$0")" && pwd)}"
mkdir -p "$out"
cd "$out"
days=3650
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

key() { openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$1" 2>/dev/null; }

ca() { # name subject
    key "$1.key"
    openssl req -x509 -new -key "$1.key" -subj "$2" -days "$days" -sha256 \
        -addext "basicConstraints=critical,CA:TRUE" \
        -addext "keyUsage=critical,keyCertSign,cRLSign" -out "$1.pem"
}

leaf() { # name subject ca extfile-lines
    key "$1.key"
    openssl req -new -key "$1.key" -subj "$2" -out "$tmp/$1.csr"
    printf '%b' "$4" > "$tmp/$1.ext"
    openssl x509 -req -in "$tmp/$1.csr" -CA "$3.pem" -CAkey "$3.key" -CAcreateserial \
        -days "$days" -sha256 -extfile "$tmp/$1.ext" -out "$1.pem" 2>/dev/null
}

client='basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=clientAuth\n'
ca ca "/O=emob test/CN=emob test CA"
leaf server "/O=emob test/CN=localhost" ca \
    'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1,IP:::1,DNS:localhost\n'
leaf cp-1 "/O=emob test/CN=CP-1" ca "$client"
leaf cp-2 "/O=emob test/CN=CP-2" ca "$client"
ca rogue-ca "/O=somebody else/CN=rogue CA"
leaf rogue-cp-1 "/O=somebody else/CN=CP-1" rogue-ca "$client"
rm -f ./*.srl rogue-ca.key ca.key
echo "minted into $out"
