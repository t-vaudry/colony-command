; Extra steps for the Colony Command installer (Tauri NSIS hooks).
;
; Rules: never stop a process by image name (other Colony daemons and the sessions
; in colony-ptyd belong to whoever is running them), and never stop colony-ptyd at
; all. A running exe can't be overwritten or deleted, but it can be renamed, so
; colony-ptyd is moved aside and exits by itself when its sessions end.

; Stops this install's colonyd, matched by its full path. Other colonyd processes
; (another install, a dev build, a test daemon) are not touched.
!macro ColonyStopInstalledDaemon
  ; The path goes in an environment variable, so a quote in it can't break the command.
  Push $0
  System::Call 'kernel32::SetEnvironmentVariable(t "COLONY_INSTDIR", t "$INSTDIR")'
  nsExec::Exec `powershell -NoProfile -NonInteractive -Command "Get-Process colonyd -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $env:COLONY_INSTDIR 'colonyd.exe') } | Stop-Process -Force"`
  Pop $0
  Pop $0
!macroend

; Moves colony-ptyd.exe to a name nothing else uses, so the new file can take its
; place while the old one keeps running (and a previous renamed-aside copy that is
; still running doesn't get in the way).
!macro ColonyMoveAsidePtyd
  Push $0
  ${If} ${FileExists} "$INSTDIRcolony-ptyd.exe"
    System::Call 'kernel32::GetTickCount()i.r0'
    Rename "$INSTDIRcolony-ptyd.exe" "$INSTDIRcolony-ptyd.$0.old"
  ${EndIf}
  Pop $0
!macroend

; Best effort: older renamed-aside copies that have exited by now.
!macro ColonyCleanOldPtyd
  Push $1
  Push $2
  FindFirst $1 $2 "$INSTDIRcolony-ptyd.*.old"
  ${While} $2 != ""
    Delete /REBOOTOK "$INSTDIR$2"
    FindNext $1 $2
  ${EndWhile}
  FindClose $1
  Pop $2
  Pop $1
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro ColonyStopInstalledDaemon
  !insertmacro ColonyCleanOldPtyd
  !insertmacro ColonyMoveAsidePtyd
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro ColonyCleanOldPtyd
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Not when an update is replacing this install: PREINSTALL has already handled it.
  ${If} $UpdateMode <> 1
    !insertmacro ColonyStopInstalledDaemon
    !insertmacro ColonyMoveAsidePtyd
    Push $0
    Push $3
    StrCpy $3 "no"
    ${If} ${FileExists} "$INSTDIR\colony-setup.exe"
      ; Silent uninstalls leave the hooks alone: the answer defaults to No.
      MessageBox MB_YESNO|MB_ICONQUESTION "Also remove Colony's hooks from Claude Code's settings (on Windows and in running WSL distros)?$\r$\n$\r$\nYour other settings are kept, and a backup of each settings file is made first.$\r$\n$\r$\nIf you answer No, the hooks stay registered but the hook program (%USERPROFILE%.colonyincolony-hook.exe, including a copy you built yourself) is deleted, so they do nothing." /SD IDNO IDNO colony_keep_hooks
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
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    ; The copy used for the uninstall can't delete itself while it runs; it has exited now.
    Delete "$PROFILE\.colony\bin\colony-setup.exe"
  ${EndIf}
!macroend
