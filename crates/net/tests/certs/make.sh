#!/bin/sh
# Makes the test certificates for the HTTPS tests (spec 0004, AC-3).
# Needs OpenSSL 3.4 or later (for -not_before and -not_after).
#
# ca.der           the test root, trusted only through NetConfig.extra_roots
# localhost.*      a leaf for 127.0.0.1 and localhost, signed by the root
# wrong-host.*     a leaf for another host name, signed by the root
# expired.*        a leaf for 127.0.0.1 whose validity ended in 2020
# self-signed.*    a leaf for 127.0.0.1 that signs itself
#
# Leaves last 825 days, the most macOS accepts for a root you add, so
# run this again when the tests report an expired localhost certificate.
set -eu
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

key() { openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$1"; }

key "$tmp/ca.key"
openssl req -x509 -new -key "$tmp/ca.key" -sha256 -days 3650 \
  -subj "/CN=net test root" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -out "$tmp/ca.pem"
openssl x509 -in "$tmp/ca.pem" -outform DER -out ca.der

# leaf <name> <subject alt names> [extra x509 flags...]
leaf() {
  name=$1; san=$2; shift 2
  key "$tmp/$name.key"
  openssl pkcs8 -topk8 -nocrypt -in "$tmp/$name.key" -out "$name.key"
  openssl req -new -key "$tmp/$name.key" -subj "/CN=$name" -out "$tmp/$name.csr"
  printf '%s\n' \
    "basicConstraints=critical,CA:FALSE" \
    "keyUsage=critical,digitalSignature" \
    "extendedKeyUsage=serverAuth" \
    "subjectAltName=$san" > "$tmp/$name.ext"
  openssl x509 -req -in "$tmp/$name.csr" -CA "$tmp/ca.pem" -CAkey "$tmp/ca.key" \
    -sha256 -extfile "$tmp/$name.ext" "$@" -out "$name.pem"
}

leaf localhost "IP:127.0.0.1,DNS:localhost" -days 825
leaf wrong-host "DNS:not-this-host.test" -days 825
leaf expired "IP:127.0.0.1,DNS:localhost" -not_before 20190101000000Z -not_after 20200101000000Z

key "$tmp/self-signed.key"
openssl pkcs8 -topk8 -nocrypt -in "$tmp/self-signed.key" -out self-signed.key
openssl req -x509 -new -key "$tmp/self-signed.key" -sha256 -days 825 \
  -subj "/CN=self-signed" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "extendedKeyUsage=serverAuth" \
  -addext "subjectAltName=IP:127.0.0.1,DNS:localhost" \
  -out self-signed.pem
