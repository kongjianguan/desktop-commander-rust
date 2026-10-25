Unicode True
Name "Desktop Commander"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\DesktopCommander"
RequestExecutionLevel user
Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles
Section
  SetOutPath "$INSTDIR"
  File /r "${BUNDLE}\*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\Desktop Commander"
  CreateShortcut "$SMPROGRAMS\Desktop Commander\Desktop Commander.lnk" "$INSTDIR\desktop-commander.exe" "remote"
  CreateShortcut "$SMPROGRAMS\Desktop Commander\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\DesktopCommanderRust" "DisplayName" "Desktop Commander"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\DesktopCommanderRust" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\DesktopCommanderRust" "UninstallString" '"$INSTDIR\Uninstall.exe"'
SectionEnd
Section "Uninstall"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\DesktopCommanderRust"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "DesktopCommanderRust"
  RMDir /r "$SMPROGRAMS\Desktop Commander"
  RMDir /r "$INSTDIR"
SectionEnd
