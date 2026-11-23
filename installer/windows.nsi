; Per-user installer for Codex RPC. scripts/build.js runs it as:
;   makensis /DVERSION=x.y.z /DSOURCE=<built exe> /DOUTFILE=<setup exe> installer/windows.nsi
; Install folder, Start menu shortcut and uninstall key match the ones in
; Tauri 2's NSIS template (currentUser mode), so it upgrades those installs in place.
Unicode true
ManifestDPIAware true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!include "MUI2.nsh"

!define PRODUCT "Codex RPC"
!define EXE "codex-rich-presence.exe"
!define APP_ID "io.github.inerthel-agi.codex-rich-presence"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}"
!define NOTIFY_KEY "Software\Classes\AppUserModelId\${APP_ID}"

Name "${PRODUCT}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\${PRODUCT}"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "FileDescription" "${PRODUCT} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Inerthel, MIT license"

!define MUI_ICON "..\assets\app.ico"
!define MUI_UNICON "..\assets\app.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_PARAMETERS "--settings"

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; The running app keeps its executable locked.
!macro StopApp
  nsExec::Exec 'taskkill /F /IM "${EXE}"'
  Pop $0
  Sleep 500
!macroend

Section "Install"
  !insertmacro StopApp
  ; Web view caches of the Tauri builds (0.5.1 and older); nothing reads them any more.
  RMDir /r "$LOCALAPPDATA\${APP_ID}\EBWebView"
  RMDir "$LOCALAPPDATA\${APP_ID}"
  RMDir /r "$LOCALAPPDATA\eu.stealthylabs.codex-rich-presence\EBWebView"
  RMDir "$LOCALAPPDATA\eu.stealthylabs.codex-rich-presence"
  SetOutPath "$INSTDIR"
  File "/oname=${EXE}" "${SOURCE}"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\${PRODUCT}.lnk" "$INSTDIR\${EXE}" "--settings"
  ; Lets Windows attribute the app's notifications to it.
  WriteRegStr HKCU "${NOTIFY_KEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Inerthel"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

; Settings and usage history in %LOCALAPPDATA%\codex-rich-presence are kept.
Section "Uninstall"
  !insertmacro StopApp
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${PRODUCT}.lnk"
  ; The Tauri installers could also leave a desktop shortcut.
  Delete "$DESKTOP\${PRODUCT}.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "CodexRichPresence"
  DeleteRegKey HKCU "${NOTIFY_KEY}"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
