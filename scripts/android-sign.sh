#!/usr/bin/env bash
# Aligns and signs a release APK.
#
#   scripts/android-sign.sh <unsigned.apk> <signed.apk>
#
# Reads the signing key from the environment, so nothing secret is on the command line:
#   ANDROID_KEYSTORE            path to the .jks / .keystore file
#   ANDROID_KEYSTORE_PASSWORD   the keystore's password
#   ANDROID_KEY_ALIAS           the key's alias
#   ANDROID_KEY_PASSWORD        the key's password (defaults to the keystore's)
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 <unsigned.apk> <signed.apk>" >&2; exit 2; }
in=$1
out=$2
: "${ANDROID_KEYSTORE:?set ANDROID_KEYSTORE}" "${ANDROID_KEYSTORE_PASSWORD:?set ANDROID_KEYSTORE_PASSWORD}" "${ANDROID_KEY_ALIAS:?set ANDROID_KEY_ALIAS}"
export ANDROID_KEY_PASSWORD=${ANDROID_KEY_PASSWORD:-$ANDROID_KEYSTORE_PASSWORD}

tools=$(ls -d "${ANDROID_HOME:?set ANDROID_HOME}"/build-tools/* | sort -V | tail -1)
aligned=$(mktemp --suffix=.apk)
trap 'rm -f "$aligned"' EXIT

# Align first: signing a v2 APK must come after zipalign.
"$tools/zipalign" -f -p 4 "$in" "$aligned"
"$tools/apksigner" sign \
  --ks "$ANDROID_KEYSTORE" --ks-key-alias "$ANDROID_KEY_ALIAS" \
  --ks-pass env:ANDROID_KEYSTORE_PASSWORD --key-pass env:ANDROID_KEY_PASSWORD \
  --out "$out" "$aligned"
"$tools/apksigner" verify --verbose "$out"
