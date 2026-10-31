; Slidr interactive installer (NSIS / Modern UI 2).
; Build:  makensis -DVERSION=0.1.0 slidr.nsi   (run from this directory)
; Produces an interactive wizard: welcome → install dir → components
; (optional desktop shortcut) → install → finish, plus an uninstaller.

Unicode True
!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif

Name "Slidr"
OutFile "Slidr-windows-setup.exe"
InstallDir "$PROGRAMFILES64\Slidr"
InstallDirRegKey HKLM "Software\Slidr" "InstallDir"
RequestExecutionLevel admin
BrandingText "Slidr ${VERSION}"

!define MUI_ICON "logo.ico"
!define MUI_UNICON "logo.ico"
!define MUI_ABORTWARNING

; ── Installer pages ──
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\slidr.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch Slidr"
; Slidr's mixer needs a virtual audio cable for the Slidr Mic. VB-CABLE's
; licence doesn't allow building it into another installer without VB-Audio's
; agreement, so the finish page only points to it (unchecked, and not at all
; when it is installed already).
!define MUI_FINISHPAGE_TEXT "Slidr has been installed.$\r$\n$\r$\nFor the mixer's Slidr Mic you also need a virtual audio cable: VB-CABLE by VB-Audio (www.vb-cable.com). VB-CABLE is donationware, all participations are welcome."
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Open the VB-CABLE download page"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION OpenCablePage
!define MUI_PAGE_CUSTOMFUNCTION_SHOW FinishShow
!insertmacro MUI_PAGE_FINISH

; ── Uninstaller pages ──
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "Slidr (required)" SecCore
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "slidr.exe"
  File "logo.ico"

  WriteRegStr HKLM "Software\Slidr" "InstallDir" "$INSTDIR"

  CreateDirectory "$SMPROGRAMS\Slidr"
  CreateShortcut "$SMPROGRAMS\Slidr\Slidr.lnk" "$INSTDIR\slidr.exe" "" "$INSTDIR\logo.ico"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  !define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Slidr"
  WriteRegStr   HKLM "${UNINST_KEY}" "DisplayName"     "Slidr"
  WriteRegStr   HKLM "${UNINST_KEY}" "DisplayIcon"     "$INSTDIR\logo.ico"
  WriteRegStr   HKLM "${UNINST_KEY}" "DisplayVersion"  "${VERSION}"
  WriteRegStr   HKLM "${UNINST_KEY}" "Publisher"       "Slidr"
  WriteRegStr   HKLM "${UNINST_KEY}" "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegStr   HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
SectionEnd

Section "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\Slidr.lnk" "$INSTDIR\slidr.exe" "" "$INSTDIR\logo.ico"
SectionEnd

LangString DESC_SecCore    ${LANG_ENGLISH} "The Slidr application and Start-menu shortcut."
LangString DESC_SecDesktop ${LANG_ENGLISH} "Place a shortcut on the Desktop."
!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore}    $(DESC_SecCore)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} $(DESC_SecDesktop)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; ── VB-CABLE hint on the finish page ──
!define CABLE_PAGE "https://vb-audio.com/Cable/"

; $0 = 1 when VB-CABLE's driver is installed (its service is registered).
Function CableInstalled
  StrCpy $0 0
  ClearErrors
  EnumRegKey $1 HKLM "SYSTEM\CurrentControlSet\Services\VBAudioVACMME" 0
  IfErrors 0 found
  ClearErrors
  EnumRegKey $1 HKLM "SYSTEM\CurrentControlSet\Services\VBAudioVACWDM" 0
  IfErrors done
  found:
    StrCpy $0 1
  done:
FunctionEnd

Function FinishShow
  Call CableInstalled
  StrCmp $0 1 0 keep
    ; Already there: plain "installed" text, no checkbox.
    ShowWindow $mui.FinishPage.ShowReadme ${SW_HIDE}
    SendMessage $mui.FinishPage.Text ${WM_SETTEXT} 0 "STR:Slidr has been installed."
  keep:
FunctionEnd

Function OpenCablePage
  ExecShell "open" "${CABLE_PAGE}"
FunctionEnd

Section "Uninstall"
  Delete "$INSTDIR\slidr.exe"
  Delete "$INSTDIR\logo.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir  "$INSTDIR"
  Delete "$SMPROGRAMS\Slidr\Slidr.lnk"
  RMDir  "$SMPROGRAMS\Slidr"
  Delete "$DESKTOP\Slidr.lnk"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Slidr"
  DeleteRegKey HKLM "Software\Slidr"
SectionEnd
