#!/bin/sh
# After `tauri android init`: apply what the generated Android project doesn't have by itself.
#   - the network security config (cleartext ws:// to the VPN address, see docs/remote-access.md)
#   - the INTERNET permission
# Safe to run again.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
proj="$root/app/src-tauri/gen/android/app/src/main"
manifest="$proj/AndroidManifest.xml"
[ -f "$manifest" ] || { echo "no $manifest: run 'npx tauri android init' in app first" >&2; exit 1; }

mkdir -p "$proj/res/xml"
cp "$root/app/src-tauri/android/network_security_config.xml" "$proj/res/xml/network_security_config.xml"

if ! grep -q 'android:networkSecurityConfig' "$manifest"; then
  sed -i 's|<application|<application android:networkSecurityConfig="@xml/network_security_config"|' "$manifest"
fi
if ! grep -q 'android.permission.INTERNET' "$manifest"; then
  sed -i 's|<application|<uses-permission android:name="android.permission.INTERNET" />\n    <application|' "$manifest"
fi
grep -q 'networkSecurityConfig' "$manifest" && grep -q 'permission.INTERNET' "$manifest"
echo "android project ready"
