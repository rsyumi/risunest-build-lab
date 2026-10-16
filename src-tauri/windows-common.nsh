!ifndef RISUNEST_WINDOWS_COMMON
!define RISUNEST_WINDOWS_COMMON
!include FileFunc.nsh
!pragma warning error 7025
!pragma warning error 6040

Var InstallFailureMessage
Var ExistingNsis
Var DetectedArchitecture
Var PackageArchitecture
Var DestinationProbe
Var DestinationOperation
Var DestinationVolume

LangString InstallArchitecture 1033 "This package is for $PackageArchitecture. Windows uses $DetectedArchitecture. Download the $DetectedArchitecture package."
LangString InstallArchitecture 1042 "이 설치 파일은 $PackageArchitecture용이며, Windows는 $DetectedArchitecture를 사용합니다. $DetectedArchitecture용 설치 파일을 다운로드하세요."
LangString InstallUnsupportedWindows 1033 "This package requires 64-bit Windows. Detected Windows: $DetectedArchitecture. Package: $PackageArchitecture."
LangString InstallUnsupportedWindows 1042 "64비트 Windows가 필요합니다. Windows: $DetectedArchitecture. 설치 파일: $PackageArchitecture."
LangString InstallPermission 1033 "The installation destination could not be written: $DestinationOperation ($INSTDIR)."
LangString InstallPermission 1042 "설치 경로에 쓸 수 없습니다: $DestinationOperation ($INSTDIR)."
LangString InstallCreateDirectory 1033 "create directory"
LangString InstallCreateDirectory 1042 "폴더 생성"
LangString InstallCreateFile 1033 "create file"
LangString InstallCreateFile 1042 "파일 생성"
LangString InstallRenameFile 1033 "rename file"
LangString InstallRenameFile 1042 "파일 이름 변경"
LangString InstallDeleteFile 1033 "delete file"
LangString InstallDeleteFile 1042 "파일 삭제"
LangString InstallRelocation 1033 "An installation already exists at $INSTDIR. Uninstall it before choosing a different location."
LangString InstallRelocation 1042 "$INSTDIR에 이미 설치되어 있습니다. 다른 경로에 설치하려면 먼저 제거하세요."
LangString InstallVolumeMissing 1033 "The installation location is unavailable: $INSTDIR. Connect the drive or run the installer interactively to choose another folder."
LangString InstallVolumeMissing 1042 "설치 경로를 사용할 수 없습니다: $INSTDIR. 드라이브를 연결하거나 설치 파일을 직접 실행하여 다른 폴더를 선택하세요."
LangString InstallLocationMissing 1033 "The existing installation location could not be read."
LangString InstallLocationMissing 1042 "기존 설치 경로를 읽을 수 없습니다."
LangString InstallDowngrade 1033 "A newer version is installed. Install this older version in $INSTDIR?"
LangString InstallDowngrade 1042 "더 최신 버전이 설치되어 있습니다. $INSTDIR에 이전 버전을 설치하시겠습니까?"
LangString InstallUnknownVersion 1033 "The installed version is unknown. Replace it with this version in $INSTDIR?"
LangString InstallUnknownVersion 1042 "설치된 버전을 확인할 수 없습니다. $INSTDIR에 이 버전을 설치하시겠습니까?"
LangString InstallVersionRefused 1033 "A downgrade or an unknown installed version requires interactive confirmation. Run the installer without /P or /S."
LangString InstallVersionRefused 1042 "이전 버전으로 설치하거나 설치된 버전을 확인할 수 없는 경우 확인이 필요합니다. /P 또는 /S 없이 설치 파일을 실행하세요."
LangString InstallUpdate 1033 "Update"
LangString InstallUpdate 1042 "업데이트"
LangString InstallReinstall 1033 "Reinstall"
LangString InstallReinstall 1042 "다시 설치"
LangString InstallInPlace 1033 "The application will be installed in $INSTDIR."
LangString InstallInPlace 1042 "$INSTDIR에 설치합니다."

!macro CheckInstallVolume
  ${GetRoot} "$INSTDIR" $DestinationVolume
  System::Call 'kernel32::GetFileAttributesW(w "$DestinationVolume\") i .r1'
!macroend

!macro CheckInstallDestination
  !insertmacro CheckInstallVolume
  ${If} $1 = -1
    StrCpy $InstallFailureMessage "$(InstallVolumeMissing)"
    Goto destination_report_failure
  ${EndIf}
  StrCpy $DestinationOperation "$(InstallCreateDirectory)"
  ClearErrors
  CreateDirectory "$INSTDIR"
  IfErrors destination_failed
  StrCpy $DestinationOperation "$(InstallCreateFile)"
  ClearErrors
  GetTempFileName $DestinationProbe "$INSTDIR"
  IfErrors destination_failed
  StrCpy $DestinationOperation "$(InstallRenameFile)"
  ClearErrors
  Rename "$DestinationProbe" "$DestinationProbe.renamed"
  IfErrors destination_failed
  StrCpy $DestinationOperation "$(InstallDeleteFile)"
  ClearErrors
  Delete "$DestinationProbe.renamed"
  IfErrors destination_failed
  Goto destination_ready
  destination_failed:
  StrCpy $InstallFailureMessage "$(InstallPermission)"
  destination_report_failure:
  DetailPrint "$InstallFailureMessage"
  IfSilent +2
    MessageBox MB_OK|MB_ICONSTOP "$InstallFailureMessage"
  SetErrorLevel 1
  Abort "$InstallFailureMessage"
  destination_ready:
!macroend
!endif
