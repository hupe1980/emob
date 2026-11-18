#!/usr/bin/env bash
# Mint the test PKI `emob-pnc`'s fixtures carry, with OpenSSL.
#
# Every certificate here is encoded by **OpenSSL** rather than by anything in
# this workspace, for the same reason `emob-eichrecht` runs records from the
# S.A.F.E. corpus: a path validation checked only against DER this repository
# produced is checked against its own opinions.
#
# What it is *not* is a chain from a published V2G pool. Hubject's own is in
# `concepts/reference/roaming/openplugncharge-master.zip` and the test that runs it says so;
# what is minted here is what CI can carry, and the accurate claim for it is
# "the Annex F profiles are enforced against DER a third implementation wrote".
#
# The validity windows are absolute dates, so nothing here rots.
#
#   just pnc-fixtures          (both files)
#   scripts/make-test-pki.sh iso20   (only src/fixtures/iso20.rs)
set -euo pipefail

SET="${1:-all}"
case "$SET" in all|iso2|iso20) ;; *) echo "usage: $0 [all|iso2|iso20]" >&2; exit 2 ;; esac

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
cd "$out"
FROM="20200101000000Z"; TO_ROOT="20600101000000Z"; TO="20500101000000Z"

if [ "$SET" != iso20 ]; then
# ISO 15118 Annex F is secp256r1 for -2. Deterministic content everywhere except
# the keys, which OpenSSL generates: the fixtures are checked in, so they are
# minted once and reviewed as bytes.
mk_key() { openssl ecparam -name prime256v1 -genkey -noout -out "$1.key" 2>/dev/null; }

ext_ca() {
  cat > "$1.ext" <<EOF
basicConstraints=critical,CA:TRUE,pathlen:$2
keyUsage=critical,keyCertSign,cRLSign
EOF
}

ext_leaf() {
  cat > "$1.ext" <<EOF
basicConstraints=critical,CA:FALSE
keyUsage=critical,$2
EOF
}

# `-not_before`/`-not_after` are absolute so the fixtures never rot; `openssl
# x509` takes them as `YYYYMMDDHHMMSSZ`.
FROM="20200101000000Z"
TO_ROOT="20600101000000Z"
TO="20500101000000Z"

sign() { # subject key, subject name, issuer stem, ext stem, notAfter
  openssl req -new -key "$1.key" -subj "$2" -out "$1.csr" 2>/dev/null
  openssl x509 -req -in "$1.csr" -CA "$3.crt" -CAkey "$3.key" -CAcreateserial \
    -not_before "$FROM" -not_after "$5" -extfile "$4.ext" -out "$1.crt" \
    -sha256 2>/dev/null
}

# ── The V2G root of the pool this platform is connected to ──────────────────
mk_key root
ext_ca root 2
openssl req -new -x509 -key root.key -subj "/O=emob test/DC=V2G/CN=V2G Root CA" \
  -not_before "$FROM" -not_after "$TO_ROOT" -extensions v3 \
  -config <(printf '[req]\ndistinguished_name=dn\n[dn]\n[v3]\nbasicConstraints=critical,CA:TRUE,pathlen:2\nkeyUsage=critical,keyCertSign,cRLSign\n') \
  -out root.crt -sha256 2>/dev/null

# ── …and a second ecosystem's, which is the whole of R10 ────────────────────
mk_key other-root
openssl req -new -x509 -key other-root.key -subj "/O=another scheme/DC=V2G/CN=Other V2G Root CA" \
  -not_before "$FROM" -not_after "$TO_ROOT" -extensions v3 \
  -config <(printf '[req]\ndistinguished_name=dn\n[dn]\n[v3]\nbasicConstraints=critical,CA:TRUE,pathlen:2\nkeyUsage=critical,keyCertSign,cRLSign\n') \
  -out other-root.crt -sha256 2>/dev/null

# ── The Mobility Operator's chain, and a contract under it ──────────────────
mk_key mo1; ext_ca mo1 1; sign mo1 "/O=emob test/CN=MO Sub CA 1" root mo1 "$TO"
mk_key mo2; ext_ca mo2 0; sign mo2 "/O=emob test/CN=MO Sub CA 2" mo1 mo2 "$TO"
mk_key contract
ext_leaf contract "digitalSignature,nonRepudiation"
sign contract "/O=emob test/CN=DE8AACA2B3C4D5L" mo2 contract "$TO"

# A second contract, from the *other* ecosystem, whose chain is impeccable and
# whose root this platform's first pool does not publish.
mk_key other-mo1; ext_ca other-mo1 1
sign other-mo1 "/O=another scheme/CN=Other MO Sub CA 1" other-root other-mo1 "$TO"
mk_key foreign
ext_leaf foreign "digitalSignature,nonRepudiation"
sign foreign "/O=another scheme/CN=DE8BBCA2B3C4D5M" other-mo1 foreign "$TO"

# ── The OEM's chain, and a vehicle under it ─────────────────────────────────
mk_key oem1; ext_ca oem1 1; sign oem1 "/O=emob test/CN=OEM Sub CA 1" root oem1 "$TO"
mk_key oem2; ext_ca oem2 0; sign oem2 "/O=emob test/CN=OEM Sub CA 2" oem1 oem2 "$TO"
mk_key vehicle
ext_leaf vehicle "digitalSignature"
sign vehicle "/O=emob test/CN=WVW00000000000001" oem2 vehicle "$TO"

emit() { # stem, constant name, what it is
  printf '\n/// %s\npub const %s: &str = concat!(\n' "$3" "$2"
  openssl x509 -in "$1.crt" -outform DER 2>/dev/null | base64 | fold -w 68 |
    sed 's/.*/    "&",/'
  printf ');\n'
}

{
  cat <<'HEADER'
//! A V2G test PKI, minted by OpenSSL, embedded as the bytes it produced.
//!
//! # Why these and not a published pool's
//!
//! `emob-pnc` validates certificate paths, and a path validation checked only
//! against DER this repository produced is checked against its own opinions —
//! the same argument `emob-eichrecht` makes about running S.A.F.E.'s records
//! rather than its own. So every certificate here is encoded by **OpenSSL**,
//! to the same ASN.1, with the Annex F fields where Annex F says they go.
//!
//! What that is *not* is interoperability with a live ecosystem. Hubject's own
//! open test PKI is in `concepts/reference/roaming/openplugncharge-master.zip` — a real V2G
//! Root, real OEM sub-CAs and real provisioning certificates — and
//! `tests/a_contract_is_installed.rs` runs the provisioning chain against it
//! where the working copy has that archive, saying by name when it does not.
//! The archive is third-party and unlicensed for redistribution, which is why
//! it is indexed rather than embedded.
//!
//! # Re-minting
//!
//! `just pnc-fixtures`, which runs `scripts/make-test-pki.sh` and prints this
//! file. The validity windows are absolute — 2020 to 2050, and 2060 for the
//! roots — so nothing here rots and every test names the instant it judges at,
//! which is the property the whole workspace is built on.

/// Decode one of this module's certificates to DER.
///
/// # Panics
///
/// Never: every constant here is base64 this module emitted.
#[must_use]
pub fn der(pem_body: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(pem_body)
        .expect("a constant this module emitted")
}
HEADER
  emit root ROOT "The V2G root of the ecosystem this platform is connected to."
  emit mo1 MO_SUB_CA_1 "The Mobility Operator's first sub-CA, under [\`ROOT\`]."
  emit mo2 MO_SUB_CA_2 "Its second, which signs contracts."
  emit contract CONTRACT "A contract certificate — \`CN=DE8AACA2B3C4D5L\`, which \`[V2G2-108]\` makes the EMAID."
  emit oem1 OEM_SUB_CA_1 "The manufacturer's first sub-CA, under [\`ROOT\`]."
  emit oem2 OEM_SUB_CA_2 "Its second, which signs vehicles."
  emit vehicle PROVISIONING "An OEM provisioning certificate — \`CN=WVW00000000000001\`, which \`[V2G2-933]\` makes the \`CertID\`."
  emit other-root OTHER_ROOT "**Another ecosystem's** V2G root. The whole of R10 is that this one is not [\`ROOT\`]."
  emit other-mo1 OTHER_MO_SUB_CA_1 "That ecosystem's Mobility Operator sub-CA."
  emit foreign FOREIGN_CONTRACT "A contract minted in the other ecosystem. Impeccable, and not this platform's."
  printf '\n/// The ISO 15118-20 PKI: three ecosystems and an Ed448 one.\npub mod iso20;\n'
} > "$OLDPWD/src/fixtures.rs"
echo "🔑 crates/emob-pnc/src/fixtures.rs re-minted"
fi


# ── ISO 15118-20 ─────────────────────────────────────────────────────────────
# Annex B: secp521r1 signed with SHA-512 [V2G20-2322], or Ed448; Annex B's
# extensions only [V2G20-2323]; every certificate inside its issuer's validity
# [V2G20-3000]. One root per European ecosystem (research R3 names them; none
# publishes a -20 root, so these are emob's), and one Ed448 ecosystem whose only
# purpose is to be refused by name (D304).
if [ "$SET" != iso2 ]; then
mkdir -p "$OLDPWD/src/fixtures"
p5() { openssl ecparam -name secp521r1 -genkey -noout -out "$1.key" 2>/dev/null; }
e4() { openssl genpkey -algorithm ED448 -out "$1.key" 2>/dev/null; }
cnf20() { # file O CN DC|- basicConstraints keyUsage
  {
    printf '%s\n' "[req]" "distinguished_name = dn" "prompt = no" "[dn]" "O = $2" "CN = $3"
    [ "$4" = - ] || printf 'DC = %s\n' "$4"
    printf '%s\n' "[v3]" "basicConstraints = critical,$5" "keyUsage = critical,$6" "subjectKeyIdentifier = hash"
  } > "$1"
}
root20() { # stem digest-flag O CN
  openssl req -new -x509 -key "$1.key" -config "$1.cnf" -extensions v3 $2 \
    -not_before "$FROM" -not_after "$TO_ROOT" -out "$1.crt" 2>/dev/null
}
issue20() { # stem issuer digest-flag
  openssl req -new -key "$1.key" -config "$1.cnf" -out "$1.csr" 2>/dev/null
  openssl x509 -req -in "$1.csr" -CA "$2.crt" -CAkey "$2.key" -CAcreateserial \
    -extfile "$1.cnf" -extensions v3 $3 -not_before "$FROM" -not_after "$TO" -out "$1.crt" 2>/dev/null
}
for eco in hubject gireve charin; do
  p5 "$eco-root"; cnf20 "$eco-root.cnf" "$eco test" "$eco V2G Root CA 20" V2G "CA:TRUE,pathlen:1" "keyCertSign,cRLSign"
  root20 "$eco-root" -sha512
  p5 "$eco-mo"; cnf20 "$eco-mo.cnf" "$eco test" "$eco MO Sub CA 20" - "CA:TRUE,pathlen:0" "keyCertSign,cRLSign"
  issue20 "$eco-mo" "$eco-root" -sha512
  p5 "$eco-contract"; cnf20 "$eco-contract.cnf" "$eco test" "DE8AACA2B3C4D5L" - "CA:FALSE" "digitalSignature,nonRepudiation"
  issue20 "$eco-contract" "$eco-mo" -sha512
done
e4 ed-root; cnf20 ed-root.cnf "ed448 test" "Ed448 V2G Root CA" V2G "CA:TRUE,pathlen:1" "keyCertSign,cRLSign"; root20 ed-root ""
e4 ed-mo; cnf20 ed-mo.cnf "ed448 test" "Ed448 MO Sub CA" - "CA:TRUE,pathlen:0" "keyCertSign,cRLSign"; issue20 ed-mo ed-root ""
e4 ed-contract; cnf20 ed-contract.cnf "ed448 test" "DE8AACA2B3C4D5L" - "CA:FALSE" "digitalSignature,nonRepudiation"; issue20 ed-contract ed-mo ""

emit20() { # stem, constant name, what it is
  printf '\n/// %s\npub const %s: &str = concat!(\n' "$3" "$2"
  openssl x509 -in "$1.crt" -outform DER 2>/dev/null | base64 | tr -d '\n' | fold -w 68 |
    sed 's/.*/    "&",/'
  printf ');\n'
}
{
  cat <<'HEADER'
//! An ISO 15118-20 test PKI, minted by OpenSSL by `scripts/make-test-pki.sh`.
//!
//! One root per European Plug & Charge ecosystem — Hubject, GIREVE with
//! Thales, CharIN with Irdeto — on secp521r1 with `ecdsa-with-SHA512`, each
//! with a Mobility Operator sub-CA and a contract (`CN=DE8AACA2B3C4D5L` in all
//! three, so a test can show the **root**, not the identifier, deciding). None
//! of the three publishes a -20 root (research R3), so these are this
//! workspace's own and say so in their `O`.
//!
//! And one Ed448 ecosystem, whose only use is to be refused by name: this
//! build has no Ed448 verifier on its `RustCrypto` generation (D304).
HEADER
  for eco in hubject gireve charin; do
    up=$(echo "$eco" | tr '[:lower:]' '[:upper:]')
    emit20 "$eco-root" "${up}_ROOT" "The \`$eco-test\` ecosystem's -20 V2G root (secp521r1)."
    emit20 "$eco-mo" "${up}_MO_SUB_CA" "Its Mobility Operator sub-CA."
    emit20 "$eco-contract" "${up}_CONTRACT" "A contract under it — \`CN=DE8AACA2B3C4D5L\`."
  done
  emit20 ed-root ED448_ROOT "An Ed448 -20 V2G root."
  emit20 ed-mo ED448_MO_SUB_CA "Its Mobility Operator sub-CA (Ed448)."
  emit20 ed-contract ED448_CONTRACT "A contract under it (Ed448), refused by name."
} > "$OLDPWD/src/fixtures/iso20.rs"
echo "🔑 crates/emob-pnc/src/fixtures/iso20.rs minted"
fi
