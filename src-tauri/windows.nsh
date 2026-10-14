!include "${__FILEDIR__}\windows-common.nsh"
LangString AppRemoveData 1033 "Removing RisuNest application data..."
LangString AppRemoveData 1042 "RisuNest 애플리케이션 데이터를 삭제합니다..."
LangString AppRemoveFailed 1033 "RisuNest application data could not be removed. Run the uninstaller again to retry."
LangString AppRemoveFailed 1042 "RisuNest 애플리케이션 데이터를 삭제할 수 없습니다. 제거 프로그램을 다시 실행해주세요."

!macro NSIS_HOOK_PREINSTALL
  !insertmacro CheckInstallDestination
!macroend

!macro NSIS_HOOK_PREUNINSTALL
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
