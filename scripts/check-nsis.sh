#!/usr/bin/env bash
# Checks app/src-tauri/installer-hooks.nsh for the mistakes that once silently broke updates
# (backslashes lost, so "$INSTDIR\colonyd.exe" became "$INSTDIRcolonyd.exe", an unknown
# variable): a $INSTDIR directly followed by a letter or $, and %USERPROFILE% not followed by
# a backslash. Then, when makensis is installed, compiles scripts/nsis-hooks-test.nsi, which
# includes the hooks with stub macros, treating warnings (unknown variables) as errors.
set -euo pipefail
cd "$(dirname "$0")/.."
hooks=app/src-tauri/installer-hooks.nsh
status=0

if grep -nE '\$INSTDIR[A-Za-z$]' "$hooks"; then
  echo "check-nsis: \$INSTDIR must be followed by a backslash (or nothing/quote), not a letter or \$" >&2
  status=1
fi
if grep -nE '%USERPROFILE%([^\\]|$)' "$hooks"; then
  echo "check-nsis: %USERPROFILE% must be followed by a backslash" >&2
  status=1
fi
# A path separator eaten between a variable and a name: $PROFILE.colony, $PROFILEcolony, ...
if grep -nE '\$(PROFILE|APPDATA|LOCALAPPDATA|TEMP)[A-Za-z.]' "$hooks"; then
  echo "check-nsis: a path variable is directly followed by a name; a backslash is missing" >&2
  status=1
fi

if command -v makensis >/dev/null 2>&1; then
  # From scripts/, where the test script's include path is relative to.
  (cd scripts && makensis -V2 -WX nsis-hooks-test.nsi) || status=1
  rm -f scripts/nsis-hooks-test.exe
else
  echo "check-nsis: makensis not installed, skipping the compile check"
fi
exit $status
