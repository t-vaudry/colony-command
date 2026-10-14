#!/usr/bin/env bash
# Prepares the next patch release in the working tree: bumps X.Y.Z to X.Y.(Z+1)
# everywhere a version lives, and turns CHANGELOG.md's `## [Unreleased]` section
# into `## [X.Y.Z+1] - <date>` (leaving a fresh, empty Unreleased above it). With
# nothing under Unreleased, the notes are the commits on main since the last tag
# (merged pull requests by their titles). Prints the new version. Commits nothing.
#
#   scripts/bump-patch.sh            # run from anywhere in the repo, on a clean tree
#
# The release workflow (.github/workflows/release.yml) runs this, then commits and tags.
set -euo pipefail
cd "$(dirname "$0")/.."

old=$(awk '/^\[workspace.package\]/{f=1} f && /^version/{gsub(/^[^"]*"|".*$/,""); print; exit}' Cargo.toml)
IFS=. read -r major minor patch <<<"$old"
new="$major.$minor.$((patch + 1))"

# Cargo.toml: only the [workspace.package] version.
awk -v new="$new" '/^\[/{f=($0=="[workspace.package]")} f && /^version *=/ && !done {sub(/"[^"]*"/, "\"" new "\""); done=1} {print}' Cargo.toml >Cargo.toml.tmp
mv Cargo.toml.tmp Cargo.toml
# tauri.conf.json: its first "version" is the app's.
sed -i "0,/\"version\": *\"[^\"]*\"/s//\"version\": \"$new\"/" app/src-tauri/tauri.conf.json
# package.json and both versions in package-lock.json.
(cd app && npm version "$new" --no-git-tag-version --allow-same-version >/dev/null)
# Cargo.lock follows the workspace version.
cargo metadata --format-version 1 >/dev/null

# Release notes for the changelog.
section=$(awk '/^## \[Unreleased\]/{f=1; next} /^## /{f=0} f' CHANGELOG.md)
if [ -z "$(printf '%s' "$section" | tr -d '[:space:]')" ]; then
  last=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)
  range=${last:+$last..}HEAD
  # Merge commits read "Merge pull request #N from ..." with the PR title as the body.
  section=$(git log --first-parent --format='%s%x1f%b%x1e' "$range" | awk -v RS=$'\x1e' -F$'\x1f' '
    NF < 1 || $1 ~ /^[[:space:]]*$/ { next }
    {
      sub(/^\n+/, "", $1)
      if (match($1, /^Merge pull request #[0-9]+/)) {
        pr = substr($1, 20, RLENGTH - 19)
        split($2, body, "\n")
        printf "- %s (%s)\n", (body[1] != "" ? body[1] : $1), pr
      } else {
        printf "- %s\n", $1
      }
    }')
  section=$'\n'"$section"$'\n'
  [ -n "$(printf '%s' "$section" | tr -d '[:space:]')" ] || section=$'\n- Maintenance release.\n'
fi

today=$(date -u +%Y-%m-%d)
notes=$(mktemp)
printf '%s\n' "$section" >"$notes"
awk -v new="$new" -v today="$today" -v notes="$notes" '
  /^## \[Unreleased\]/ && !done {
    print "## [Unreleased]"; print ""
    print "## [" new "] - " today
    while ((getline line < notes) > 0) print line
    print ""
    skip = 1; done = 1; next
  }
  skip && /^## / { skip = 0 }
  !skip { print }
  END {
    if (!done) { print "missing ## [Unreleased] in CHANGELOG.md" > "/dev/stderr"; exit 1 }
  }' CHANGELOG.md >CHANGELOG.md.tmp
rm -f "$notes"
mv CHANGELOG.md.tmp CHANGELOG.md
# One blank line between sections, however the notes ended.
cat -s CHANGELOG.md >CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md

scripts/check-release-version.sh "v$new" >&2
echo "$new"
