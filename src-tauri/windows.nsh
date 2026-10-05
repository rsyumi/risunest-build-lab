!macro NSIS_HOOK_PREUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ; The template's process check runs after this hook.
    !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
    SetDetailsPrint both
    DetailPrint "Removing RisuNest application data..."
    SetDetailsPrint none
    ClearErrors
    ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --remove-local-data --yes' $R6
    SetDetailsPrint both
    ${If} ${Errors}
    ${OrIf} $R6 != 0
      ; Keep the application and uninstaller available for another attempt.
      SetErrorLevel 1
      Abort "RisuNest application data could not be removed. Nothing was uninstalled; run the uninstaller again to retry."
    ${EndIf}
  ${EndIf}
!macroend
