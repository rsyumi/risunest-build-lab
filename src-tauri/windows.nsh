!include "${__FILEDIR__}\windows-common.nsh"
!include WordFunc.nsh
LangString AppRemoveData 1033 "Removing RisuNest application data..."
LangString AppRemoveData 1042 "RisuNest 애플리케이션 데이터를 삭제합니다..."
LangString AppRemoveFailed 1033 "RisuNest application data could not be removed. Run the uninstaller again to retry."
LangString AppRemoveFailed 1042 "RisuNest 애플리케이션 데이터를 삭제할 수 없습니다. 제거 프로그램을 다시 실행해주세요."
LangString AppExitRequest 1033 "Closing RisuNest..."
LangString AppExitRequest 1042 "RisuNest를 종료합니다..."

; Asks every running instance of this installation, in any launch profile, to
; close the way its window does, so each one saves and can finish syncing. The
; app holds the mutex until it exits; the process check that follows stops
; whatever is still running after 60 seconds. The names match windows_session.rs.
!macro RequestAppExit
  Push $0
  Push $1
  Push $2
  Push $3
  StrCpy $0 "${BUNDLEID}.exit-request|$INSTDIR\${MAINBINARYNAME}.exe"
  StrCpy $1 "$INSTDIR\${MAINBINARYNAME}.exe"
  System::Call 'user32::CharLowerW(w r1 r1)'
  ${WordReplace} $1 "\" "/" "+" $1
  StrCpy $1 "Local\${BUNDLEID}.exit-request|$1"
  StrLen $2 $0
  StrLen $3 $1
  ${If} $2 <= 255
  ${AndIf} $3 <= 255
    System::Call 'kernel32::OpenMutexW(i 0x00100000, i 0, w r1) p .r2'
    ${If} $2 <> 0
      System::Call 'kernel32::CloseHandle(p r2)'
      System::Call 'user32::RegisterWindowMessageW(w r0) i .r3'
      ${If} $3 <> 0
        DetailPrint "$(AppExitRequest)"
        System::Call 'user32::PostMessageW(p 0xffff, i r3, p 0, p 0)'
        StrCpy $3 0
        ${Do}
          Sleep 250
          System::Call 'kernel32::OpenMutexW(i 0x00100000, i 0, w r1) p .r2'
          ${If} $2 = 0
            ${Break}
          ${EndIf}
          System::Call 'kernel32::CloseHandle(p r2)'
          IntOp $3 $3 + 1
        ${LoopUntil} $3 >= 240
      ${EndIf}
    ${EndIf}
  ${EndIf}
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro CheckInstallDestination
  !insertmacro RequestAppExit
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro RequestAppExit
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ; The template's process check runs after this hook.
    !insertmacro CheckIfAppIsRunning "$INSTDIR\${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
    SetDetailsPrint both
    DetailPrint "$(AppRemoveData)"
    SetDetailsPrint none
    StrCpy $InstallFailureMessage ""
    ClearErrors
    ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --remove-local-data --yes' $R6
    ${If} ${Errors}
      StrCpy $InstallFailureMessage "$(AppRemoveFailed) (app-cleanup-launch-failed)"
    ${ElseIf} $R6 != 0
      StrCpy $InstallFailureMessage "$(AppRemoveFailed) (app-cleanup-exit:$R6)"
    ${EndIf}
    SetDetailsPrint both
    ${If} $InstallFailureMessage != ""
      ; Keep the application and uninstaller available for another attempt.
      SetErrorLevel 1
      DetailPrint "$InstallFailureMessage"
      IfSilent +2
        MessageBox MB_OK|MB_ICONSTOP "$InstallFailureMessage"
      Abort "$InstallFailureMessage"
    ${EndIf}
  ${EndIf}
!macroend
