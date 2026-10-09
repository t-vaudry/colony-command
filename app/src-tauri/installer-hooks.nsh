; Extra steps for the Colony Command installer (Tauri NSIS hooks).

!macro NSIS_HOOK_PREINSTALL
  ; Installing over a running Colony (an update): stop the daemon so its file can be
  ; replaced. colony-ptyd is not stopped, because the sessions live in it. A running
  ; exe can't be overwritten but can be renamed, so move it aside; the new one is used
  ; the next time it starts.
  nsExec::Exec 'taskkill /F /IM colonyd.exe'
  Delete "$INSTDIR\colony-ptyd.exe.old"
  Rename "$INSTDIR\colony-ptyd.exe" "$INSTDIR\colony-ptyd.exe.old"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Delete /REBOOTOK "$INSTDIR\colony-ptyd.exe.old"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Not when an update is replacing this install.
  ${If} $UpdateMode <> 1
    ${If} ${FileExists} "$INSTDIR\colony-setup.exe"
      ; Silent uninstalls leave the hooks alone: the answer defaults to No.
      MessageBox MB_YESNO|MB_ICONQUESTION "Also remove Colony's hooks from Claude Code's settings (on Windows and in WSL)?$\r$\n$\r$\nYour other settings are kept, and a backup of each settings file is made first. Without this, the hooks stay in place but do nothing." /SD IDNO IDNO +2
      nsExec::ExecToLog '"$INSTDIR\colony-setup.exe" uninstall --target all --yes'
    ${EndIf}
  ${EndIf}
  nsExec::Exec 'taskkill /F /IM colonyd.exe'
  nsExec::Exec 'taskkill /F /IM colony-ptyd.exe'
!macroend
