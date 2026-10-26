; Extra steps for the Colony Command installer (Tauri NSIS hooks).
;
; Rules: never stop a process by image name (other Colony daemons and the sessions
; in colony-ptyd belong to whoever is running them), and never stop colony-ptyd at
; all. A running exe can't be overwritten or deleted, but it can be renamed, so every
; sidecar exe that may be running (colonyd, colony-ptyd, colony-hook, colony-setup) is
; moved aside to <name>.<tick>.old before the new files are copied. colony-ptyd exits by
; itself when its sessions end; the .old copies are deleted later (best effort, here and
; by the app at startup). Moving colonyd aside too matters: the running app can restart
; it between "stop" and "copy", and a locked colonyd.exe would be left at the old version.
;
; Every path needs a backslash after the variable: "$INSTDIR" + name without one is a
; different, unknown variable. scripts/check-nsis.sh checks for that.

; Stops this install's colonyd, matched by its full path: the one in INSTDIR, or a copy
; of it renamed aside there (colonyd.<tick>.old). Other colonyd processes (another
; install, a dev build, a test daemon) are not touched.
!macro ColonyStopInstalledDaemon
  ; The path goes in an environment variable, so a quote in it can't break the command.
  ; $$ is NSIS's escape for a literal $ (PowerShell's $_ and $env: are not NSIS variables).
  Push $0
  System::Call 'kernel32::SetEnvironmentVariable(t "COLONY_INSTDIR", t "$INSTDIR")'
  ; The installer is a 32-bit program, and a 32-bit PowerShell can't read the path of a
  ; 64-bit process (Path comes back empty, so nothing would match): use the 64-bit one.
  StrCpy $0 "powershell"
  ${If} ${FileExists} "$WINDIR\sysnative\WindowsPowerShell\v1.0\powershell.exe"
    StrCpy $0 "$WINDIR\sysnative\WindowsPowerShell\v1.0\powershell.exe"
  ${EndIf}
  nsExec::Exec `"$0" -NoProfile -NonInteractive -Command "Get-Process colonyd* -ErrorAction SilentlyContinue | Where-Object { $$_.Path -and ((Split-Path -Parent $$_.Path) -eq $$env:COLONY_INSTDIR) -and ((Split-Path -Leaf $$_.Path) -match '^colonyd(\.[0-9]+\.old|\.exe)$$') } | Stop-Process -Force"`
  Pop $0
  Pop $0
!macroend

; Moves $INSTDIR\<name>.exe to a name nothing else uses, so the new file can take its
; place while the old one keeps running (and a previous renamed-aside copy that is
; still running doesn't get in the way).
!macro ColonyMoveAside name
  Push $0
  ${If} ${FileExists} "$INSTDIR\${name}.exe"
    System::Call 'kernel32::GetTickCount()i.r0'
    Rename "$INSTDIR\${name}.exe" "$INSTDIR\${name}.$0.old"
  ${EndIf}
  Pop $0
!macroend

!macro ColonyMoveAsideAll
  !insertmacro ColonyMoveAside "colonyd"
  !insertmacro ColonyMoveAside "colony-ptyd"
  !insertmacro ColonyMoveAside "colony-hook"
  !insertmacro ColonyMoveAside "colony-setup"
!macroend

; Best effort: older renamed-aside copies of <name> that have exited by now. One that is
; still running can't be deleted now; /REBOOTOK leaves it for the next restart.
!macro ColonyCleanOld name
  Push $1
  Push $2
  FindFirst $1 $2 "$INSTDIR\${name}.*.old"
  ${While} $2 != ""
    Delete /REBOOTOK "$INSTDIR\$2"
    FindNext $1 $2
  ${EndWhile}
  FindClose $1
  Pop $2
  Pop $1
!macroend

!macro ColonyCleanOldAll
  !insertmacro ColonyCleanOld "colonyd"
  !insertmacro ColonyCleanOld "colony-ptyd"
  !insertmacro ColonyCleanOld "colony-hook"
  !insertmacro ColonyCleanOld "colony-setup"
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro ColonyStopInstalledDaemon
  ; Even if the stop failed: a running exe can still be renamed.
  !insertmacro ColonyMoveAsideAll
  !insertmacro ColonyCleanOldAll
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro ColonyCleanOldAll
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Not when an update is replacing this install: PREINSTALL has already handled it.
  ${If} $UpdateMode <> 1
    !insertmacro ColonyStopInstalledDaemon
    Push $0
    Push $3
    StrCpy $3 "no"
    ${If} ${FileExists} "$INSTDIR\colony-setup.exe"
      ; Silent uninstalls leave the hooks alone: the answer defaults to No.
      MessageBox MB_YESNO|MB_ICONQUESTION "Also remove Colony's hooks from Claude Code's settings (on Windows and in running WSL distros)?$\r$\n$\r$\nYour other settings are kept, and a backup of each settings file is made first.$\r$\n$\r$\nIf you answer No, the hooks stay registered but the hook program (%USERPROFILE%\.colony\bin\colony-hook.exe, including a copy you built yourself) is deleted, so they do nothing." /SD IDNO IDNO colony_keep_hooks
      nsExec::ExecToLog '"$INSTDIR\colony-setup.exe" uninstall --target all --yes'
      Pop $0
      StrCpy $3 "yes"
      colony_keep_hooks:
    ${EndIf}
    ${If} $3 == "no"
      ; Make the leftover entries inert: their commands run nothing unless this file exists.
      Delete "$PROFILE\.colony\bin\colony-hook.exe"
    ${EndIf}
    Pop $3
    Pop $0
    ; After colony-setup has run: it is one of the files moved aside.
    !insertmacro ColonyMoveAsideAll
    !insertmacro ColonyCleanOldAll
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    ; The copy used for the uninstall can't delete itself while it runs; it has exited now.
    Delete "$PROFILE\.colony\bin\colony-setup.exe"
    ; Whatever could not be deleted before (a sidecar that was still running).
    !insertmacro ColonyCleanOldAll
  ${EndIf}
!macroend
