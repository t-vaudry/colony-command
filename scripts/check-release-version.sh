#!/usr/bin/env bash
# Checks that every place a version lives agrees, and (given a tag) that the tag matches.
#
#   scripts/check-release-version.sh            # the files agree with each other
#   scripts/check-release-version.sh v1.0.0     # ...and with the tag, and CHANGELOG.md has a section for it
#
# CI runs the first form on every push and the second on a `v*` tag, before building.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo=$(awk '/^\[workspace.package\]/{f=1} f && /^version/{gsub(/^[^"]*"|".*$/,""); print; exit}' Cargo.toml)
tauri=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' app/src-tauri/tauri.conf.json | head -1)
npm=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' app/package.json | head -1)
# Cargo.lock must already carry the version, or `cargo build --locked` fails in CI.
lock=$(awk '/^name = "colony-setup"\r?$/{getline; gsub(/^[^"]*"|".*$/,""); print; exit}' Cargo.lock)

fail=0
for pair in "tauri.conf.json=$tauri" "app/package.json=$npm" "Cargo.lock=$lock"; do
  if [ "${pair#*=}" != "$cargo" ]; then
    echo "version mismatch: Cargo.toml has $cargo but ${pair%%=*} has ${pair#*=}"
    fail=1
  fi
done

if [ $# -ge 1 ]; then
  tag=$1
  [ "${tag#v}" = "$cargo" ] || { echo "tag $tag doesn't match the app version $cargo"; fail=1; }
  grep -q "^## \[${tag#v}\]" CHANGELOG.md || { echo "CHANGELOG.md has no '## [${tag#v}]' section"; fail=1; }
fi
[ "$fail" = 0 ] && echo "versions agree: $cargo"
exit "$fail"
