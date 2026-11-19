#!/bin/sh
# After `tauri android init`: apply what the generated Android project doesn't have by itself.
#   - the network security config (cleartext ws:// to the VPN address, see docs/remote-access.md)
#   - the INTERNET permission
#   - the launcher icon: the desktop icon, not Tauri's default
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
# Launcher icon: render the desktop icon into Android's sizes. The adaptive icon is masked to its
# inner two thirds, so the tile is inset and its own color fills behind it.
res="$proj/res"
tmp="$(mktemp -d)"
(cd "$root/app" && npx tauri icon src-tauri/icons/icon.png -o "$tmp" >/dev/null)
cp -r "$tmp/android/." "$res/"
rm -rf "$tmp"
cat > "$res/mipmap-anydpi-v26/ic_launcher.xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
  <foreground>
    <inset android:drawable="@mipmap/ic_launcher_foreground" android:inset="17%" />
  </foreground>
  <background android:drawable="@color/ic_launcher_background" />
</adaptive-icon>
XML
cat > "$res/values/ic_launcher_background.xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<resources>
  <color name="ic_launcher_background">#18211C</color>
</resources>
XML

grep -q 'networkSecurityConfig' "$manifest" && grep -q 'permission.INTERNET' "$manifest"
echo "android project ready"
