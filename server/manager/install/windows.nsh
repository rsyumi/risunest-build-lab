!include "${__FILEDIR__}\..\..\..\src-tauri\windows-common.nsh"

LangString SyncPrepare 1033 "Stopping RisuNest Sync and preparing the installation..."
LangString SyncPrepare 1042 "RisuNest Sync를 중지하고 설치를 준비합니다..."
LangString SyncPrepareFailed 1033 "RisuNest Sync could not be prepared for installation. Check the server status in RisuNest Sync and try again."
LangString SyncPrepareFailed 1042 "RisuNest Sync 설치를 준비할 수 없습니다. 서버 상태를 확인한 후 다시 시도하세요."
LangString SyncSchedule 1033 "Applying scheduled update settings..."
LangString SyncSchedule 1042 "예약 업데이트 설정을 적용합니다..."
LangString SyncScheduleFailed 1033 "Scheduled update settings could not be applied. RisuNest Sync setup did not finish."
LangString SyncScheduleFailed 1042 "예약 업데이트 설정을 적용할 수 없어 설치를 완료하지 못했습니다."
LangString SyncAutostart 1033 "Registering automatic server startup..."
LangString SyncAutostart 1042 "서버 자동 실행을 등록합니다..."
LangString SyncAutostartFailed 1033 "Automatic server startup could not be registered. RisuNest Sync setup did not finish."
LangString SyncAutostartFailed 1042 "서버 자동 실행을 등록할 수 없어 설치를 완료하지 못했습니다."
LangString SyncStart 1033 "Starting RisuNest Sync and checking the server response..."
LangString SyncStart 1042 "RisuNest Sync를 시작하고 서버 응답을 확인합니다..."
LangString SyncStartFailed 1033 "The RisuNest Sync server could not be started or verified. Open RisuNest Sync and check the server status."
LangString SyncStartFailed 1042 "RisuNest Sync 서버를 시작하거나 응답을 확인할 수 없습니다. RisuNest Sync에서 서버 상태를 확인하세요."
LangString SyncFinish 1033 "Finishing RisuNest Sync installation..."
LangString SyncFinish 1042 "RisuNest Sync 설치를 완료합니다..."
LangString SyncFinishFailed 1033 "RisuNest Sync installation could not be finalized."
LangString SyncFinishFailed 1042 "RisuNest Sync 설치를 완료할 수 없습니다."
LangString SyncRecoveryFailed 1033 "Recovery could not be requested. Check the server status in RisuNest Sync."
LangString SyncRecoveryFailed 1042 "복구를 요청할 수 없습니다. RisuNest Sync에서 서버 상태를 확인하세요."
LangString SyncUpdateFailed 1033 "RisuNest Sync could not be prepared for the update. Program files were preserved."
LangString SyncUpdateFailed 1042 "RisuNest Sync 업데이트를 준비할 수 없습니다."
LangString SyncUpdate 1033 "Stopping RisuNest Sync for the update..."
LangString SyncUpdate 1042 "업데이트를 위해 RisuNest Sync를 중지합니다..."
LangString SyncRemovePrepareFailed 1033 "RisuNest Sync removal could not be prepared. Program files were preserved."
LangString SyncRemovePrepareFailed 1042 "RisuNest Sync 제거를 준비할 수 없습니다."
LangString SyncCheckFailed 1033 "RisuNest Sync removal checks did not pass. Program files were preserved."
LangString SyncCheckFailed 1042 "RisuNest Sync 제거 조건을 충족하지 못했습니다."
LangString SyncCheck 1033 "Checking whether RisuNest Sync can be removed..."
LangString SyncCheck 1042 "RisuNest Sync를 제거할 수 있는지 확인합니다..."
LangString SyncRemoveGuardFailed 1033 "RisuNest Sync could not be prepared for removal. Program files were preserved."
LangString SyncRemoveGuardFailed 1042 "RisuNest Sync 제거를 준비할 수 없습니다."
LangString SyncRemovePrepare 1033 "Stopping RisuNest Sync and preparing removal..."
LangString SyncRemovePrepare 1042 "RisuNest Sync를 중지하고 제거를 준비합니다..."
LangString SyncCleanupFailed 1033 "RisuNest Sync could not stop the server or remove automatic startup and scheduled updates. Program files were preserved."
LangString SyncCleanupFailed 1042 "서버 중지 또는 자동 실행 및 예약 업데이트 제거를 완료할 수 없습니다."
LangString SyncCleanup 1033 "Stopping the server and removing startup and update tasks..."
LangString SyncCleanup 1042 "서버를 중지하고 자동 실행 및 예약 업데이트를 제거합니다..."
LangString SyncDataPrepareFailed 1033 "RisuNest Sync could not finish preparing to remove server data. Program files were preserved."
LangString SyncDataPrepareFailed 1042 "RisuNest Sync 서버 데이터 삭제를 준비할 수 없습니다."
LangString SyncDataPrepare 1033 "Preparing to remove RisuNest Sync server data..."
LangString SyncDataPrepare 1042 "RisuNest Sync 서버 데이터 삭제를 준비합니다..."
LangString SyncDataFailed 1033 "RisuNest Sync server data could not be completely removed. Program files were preserved; retry removal to finish."
LangString SyncDataFailed 1042 "RisuNest Sync 서버 데이터를 모두 삭제하지 못했습니다. 다시 시도해주세요."
LangString SyncData 1033 "Removing RisuNest Sync server data..."
LangString SyncData 1042 "RisuNest Sync 서버 데이터를 삭제합니다..."
LangString SyncRemoveFinish 1033 "Finishing RisuNest Sync removal..."
LangString SyncRemoveFinish 1042 "RisuNest Sync 제거를 완료합니다..."
LangString SyncRemoveFinishFailed 1033 "RisuNest Sync program files were removed, but removal could not be finalized."
LangString SyncRemoveFinishFailed 1042 "RisuNest Sync 제거를 완료할 수 없습니다."
LangString SyncForget 1033 "Removing RisuNest Sync installation registration..."
LangString SyncForget 1042 "RisuNest Sync 설치 등록을 제거합니다..."
LangString SyncForgetFailed 1033 "RisuNest Sync program files were removed, but installation registration could not be removed."
LangString SyncForgetFailed 1042 "RisuNest Sync 설치 등록을 제거할 수 없습니다."
LangString SyncRegister 1033 "Registering this RisuNest Sync installation..."
LangString SyncRegister 1042 "RisuNest Sync 설치를 등록합니다..."
LangString SyncRegisterFailed 1033 "The RisuNest Sync installation could not be registered."
LangString SyncRegisterFailed 1042 "RisuNest Sync 설치를 등록할 수 없습니다."
LangString SyncObsolete 1033 "Removing obsolete RisuNest Sync program files..."
LangString SyncObsolete 1042 "사용하지 않는 이전 RisuNest Sync 프로그램 파일을 제거합니다..."
LangString SyncObsoleteSkipped 1033 "The previous bundle marker could not be read. Obsolete-file cleanup was skipped."
LangString SyncObsoleteSkipped 1042 "이전 설치의 파일 목록을 읽을 수 없어 이전 파일 제거를 건너뛰었습니다."
LangString SyncObsoleteFailed 1033 "Obsolete RisuNest Sync program files could not be removed."
LangString SyncObsoleteFailed 1042 "이전 RisuNest Sync 프로그램 파일을 제거할 수 없습니다."
LangString SyncDataKept 1033 "Server data was kept because the installation registration could not be confirmed."
LangString SyncDataKept 1042 "설치 등록을 확인할 수 없어 서버 데이터를 유지했습니다."
LangString SyncFiles 1033 "Removing RisuNest Sync program files..."
LangString SyncFiles 1042 "RisuNest Sync 프로그램 파일을 제거합니다..."
LangString SyncFilesFailed 1033 "RisuNest Sync removal is incomplete. Program files could not all be removed."
LangString SyncFilesFailed 1042 "RisuNest Sync 프로그램 파일을 모두 제거하지 못했습니다. 다시 시도해주세요."

!ifndef RISUNEST_SYNC_DEFAULT_INSTALL_DIR
  !define RISUNEST_SYNC_DEFAULT_INSTALL_DIR "$LOCALAPPDATA\RisuNest Sync"
!endif
!ifndef RISUNEST_SYNC_INSTALL_DIR
  !define RISUNEST_SYNC_INSTALL_DIR "$LOCALAPPDATA\RisuNestSync"
!endif
; installer delete-data removes the GUI profile unless another registered instance shares it.
!define RISUNEST_HOOK_OWNS_APP_DATA

Var SyncFailureMessage
Var SyncForce
Var SyncForceOption
Var SyncDeleteMode
Var SyncDiagnostic
Var SyncPreviousInstall

!macro SyncSkipMarkerBlanks
  ${Do}
    StrCpy $3 $1 1 $2
    ${If} $3 != " "
    ${AndIf} $3 != "$\t"
      ${Break}
    ${EndIf}
    IntOp $2 $2 + 1
  ${Loop}
!macroend

; Archive updates replace the program files without the uninstall entry, so the bundle marker
; carries the installed version. The generated marker writes the bundle's "version" key before
; "vendor", the only key holding nested versions. Leaves $0 unchanged when the marker cannot be
; read. Uses $1-$6.
!macro NSIS_HOOK_INSTALLED_VERSION
  ClearErrors
  FileOpen $5 "$INSTDIR\risunest-sync-bundle.json" r
  ${IfNot} ${Errors}
    StrCpy $6 0
    ${Do}
      ClearErrors
      FileRead $5 $1
      ${If} ${Errors}
        ${Break}
      ${EndIf}
      StrCpy $2 0
      ${Do}
        StrCpy $3 $1 1 $2
        ${If} $3 == ""
          ${Break}
        ${EndIf}
        StrCpy $4 $1 9 $2
        StrCpy $3 $1 8 $2
        IntOp $2 $2 + 1
        ${If} $4 S== '"version"'
          IntOp $2 $2 + 8
        ${ElseIf} $3 S== '"vendor"'
          IntOp $2 $2 + 7
        ${Else}
          ${Continue}
        ${EndIf}
        !insertmacro SyncSkipMarkerBlanks
        ${If} $3 == ":"
          StrCpy $6 1
          ${If} $4 S== '"version"'
            IntOp $2 $2 + 1
            !insertmacro SyncSkipMarkerBlanks
            ${If} $3 == '"'
              StrCpy $4 ""
              ${Do}
                IntOp $2 $2 + 1
                StrCpy $3 $1 1 $2
                ${If} $3 == '"'
                  ${If} $4 != ""
                    StrCpy $0 $4
                  ${EndIf}
                  ${Break}
                ${ElseIf} $3 == ""
                  ${Break}
                ${EndIf}
                StrCpy $4 "$4$3"
              ${Loop}
            ${EndIf}
          ${EndIf}
          ${Break}
        ${EndIf}
      ${Loop}
    ${LoopUntil} $6 = 1
    FileClose $5
  ${EndIf}
  ClearErrors
!macroend

!macro NSIS_HOOK_PREINSTALL
  SetDetailsPrint both
  StrCpy $SyncPreviousInstall $ExistingNsis
  ${IfThen} $ExistingNsis != "" ${|} Goto sync_install_dir_ready ${|}
  StrCmp $INSTDIR "${RISUNEST_SYNC_DEFAULT_INSTALL_DIR}" 0 sync_install_dir_ready
  StrCpy $INSTDIR "${RISUNEST_SYNC_INSTALL_DIR}"
  SetOutPath $INSTDIR
  sync_install_dir_ready:
  !insertmacro CheckInstallDestination
  InitPluginsDir
  ; An unfinished install keeps its baseline so a retry still removes the files it replaced.
  IfFileExists "$INSTDIR\risunest-sync-bundle.previous.json" sync_baseline_ready
  ClearErrors
  CopyFiles /SILENT "$INSTDIR\risunest-sync-bundle.json" "$INSTDIR\risunest-sync-bundle.previous.json"
  sync_baseline_ready:
  ClearErrors
  !insertmacro CheckIfAppIsRunning "$INSTDIR\${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  StrCpy $R7 ""
  IfFileExists "$INSTDIR\risunest-sync-bundle.json" sync_prepare_existing
  IfFileExists "$INSTDIR\risunest-sync-manager.exe" 0 sync_prepare_done
  sync_prepare_existing:
  StrCpy $SyncPreviousInstall "1"
  System::Call 'kernel32::GetCurrentProcessId()i.r0'
  DetailPrint "$(SyncPrepare)"
  !ifdef SYNC_INSTALLER_MANAGER
  File /oname=$PLUGINSDIR\risunest-sync-manager.exe "${SYNC_INSTALLER_MANAGER}"
  File /oname=$PLUGINSDIR\risunest-sync-manager-background.exe "${SYNC_INSTALLER_BACKGROUND}"
  nsExec::ExecToStack '"$PLUGINSDIR\risunest-sync-manager.exe" --server "$INSTDIR\risunest-sync-server.exe" installer prepare $0'
  !else
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" installer prepare $0'
  !endif
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 sync_prepare_failed
  StrCpy $R7 $1 36
  StrLen $2 $R7
  StrCmp $2 "36" sync_prepare_done sync_prepare_failed
  sync_prepare_failed:
  StrCpy $SyncFailureMessage "$(SyncPrepareFailed)"
  Call SyncCommandError
  IfSilent sync_prepare_quiet sync_prepare_interactive
  sync_prepare_interactive:
  MessageBox MB_OK|MB_ICONSTOP "$SyncFailureMessage"
  sync_prepare_quiet:
  SetErrorLevel 1
  Abort "$SyncFailureMessage"
  sync_prepare_done:
!macroend

!macro NSIS_HOOK_POSTINSTALL
  SetDetailsPrint both
  StrCpy $SyncFailureMessage ""
  StrCpy $R6 "0"
  DetailPrint "$(SyncRegister)"
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" installer register'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_obsolete
  StrCpy $SyncFailureMessage "$(SyncRegisterFailed)"
  Goto sync_install_failed
  sync_obsolete:
  StrCmp $SyncPreviousInstall "" sync_schedule
  DetailPrint "$(SyncObsolete)"
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" installer obsolete-files "$INSTDIR\risunest-sync-bundle.previous.json"'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_obsolete_checked
  StrCpy $SyncFailureMessage "$(SyncObsoleteFailed)"
  Goto sync_install_failed
  sync_obsolete_checked:
  Delete "$INSTDIR\risunest-sync-bundle.previous.json"
  ClearErrors
  StrCpy $2 $1 7
  ${If} $2 == "skipped"
    DetailPrint "$(SyncObsoleteSkipped)"
  ${EndIf}
  sync_schedule:
  DetailPrint "$(SyncSchedule)"
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" update schedule reconcile lock-held'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_register_startup
  StrCpy $SyncFailureMessage "$(SyncScheduleFailed)"
  Goto sync_install_failed
  sync_register_startup:
  DetailPrint "$(SyncAutostart)"
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" autostart install lock-held'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_start
  StrCpy $SyncFailureMessage "$(SyncAutostartFailed)"
  Goto sync_install_failed
  sync_start:
  DetailPrint "$(SyncStart)"
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" installer start-and-verify lock-held'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_release_guard
  StrCpy $SyncFailureMessage "$(SyncStartFailed)"
  Goto sync_install_failed
  sync_install_failed:
  Call SyncCommandError
  StrCpy $R6 "1"
  SetErrorLevel 1
  IfSilent sync_release_guard 0
  MessageBox MB_OK|MB_ICONEXCLAMATION "$SyncFailureMessage"
  sync_release_guard:
  StrCmp $R7 "" sync_install_done
  StrCmp $R6 "1" sync_cancel_guard
  DetailPrint "$(SyncFinish)"
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" installer finish "$R7"'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_install_done
  StrCpy $SyncFailureMessage "$(SyncFinishFailed)"
  Call SyncCommandError
  IfSilent +2
    MessageBox MB_OK|MB_ICONSTOP "$SyncFailureMessage"
  Goto sync_install_done
  sync_cancel_guard:
  CreateDirectory "$LOCALAPPDATA\RisuNestSyncData\manager-update"
  ClearErrors
  FileOpen $0 "$LOCALAPPDATA\RisuNestSyncData\manager-update\installer-$R7.cancel" w
  IfErrors sync_cancel_guard_failed
  FileWrite $0 '{"cancel":true}'
  FileClose $0
  Goto sync_install_done
  sync_cancel_guard_failed:
  DetailPrint "$(SyncRecoveryFailed)"
  sync_install_done:
  StrCmp $SyncFailureMessage "" sync_install_success
  SetErrorLevel 1
  Abort "$SyncFailureMessage"
  sync_install_success:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  SetDetailsPrint both
  StrCpy $R7 ""
  ${If} $UpdateMode = 1
    StrCpy $SyncFailureMessage "$(SyncUpdateFailed)"
    DetailPrint "$(SyncUpdate)"
    nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" prepare-update'
    Pop $0
    Pop $1
    StrCmp $0 "0" sync_uninstall_done sync_uninstall_failed
  ${EndIf}
  !insertmacro CheckIfAppIsRunning "$INSTDIR\${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  InitPluginsDir
  StrCpy $SyncFailureMessage "$(SyncRemovePrepareFailed)"
  ClearErrors
  CopyFiles /SILENT "$INSTDIR\risunest-sync-manager.exe" "$PLUGINSDIR\risunest-sync-remove.exe"
  IfErrors 0 sync_uninstall_copy_ready
  StrCpy $0 "copy-failed"
  StrCpy $1 "removal-helper-copy-failed: $INSTDIR\risunest-sync-manager.exe -> $PLUGINSDIR\risunest-sync-remove.exe"
  Goto sync_uninstall_failed
  sync_uninstall_copy_ready:
  StrCpy $R9 "$LOCALAPPDATA\RisuNestSyncData"
  ${un.GetParameters} $0
  ReadEnvStr $1 RISUNEST_SYNC_UNINSTALL_DATA_DIR
  ${If} $1 != ""
    StrCpy $R9 $1
  ${EndIf}
  StrCpy $SyncFailureMessage "$(SyncCheckFailed)"
  DetailPrint "$(SyncCheck)"
  StrCpy $SyncForce "normal"
  StrCpy $SyncForceOption ""
  ClearErrors
  ${un.GetOptions} $0 "/FORCE" $1
  ${IfNot} ${Errors}
    StrCpy $SyncForce "force"
    StrCpy $SyncForceOption "--force"
  ${EndIf}
  StrCpy $SyncDeleteMode "preserve"
  ${If} $DeleteAppDataCheckboxState = 1
    StrCpy $SyncDeleteMode "delete"
  ${EndIf}
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" installer removal-check $SyncForce $SyncDeleteMode'
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 sync_uninstall_failed
  StrCpy $2 $1 9
  ${If} $2 == "keep-data"
  ${AndIf} $DeleteAppDataCheckboxState = 1
    StrCpy $DeleteAppDataCheckboxState 0
    DetailPrint "$(SyncDataKept)"
  ${EndIf}
  StrCpy $R7 ""
  IfFileExists "$R9\." 0 sync_uninstall_cleanup
  System::Call 'kernel32::GetCurrentProcessId()i.r0'
  StrCpy $SyncFailureMessage "$(SyncRemoveGuardFailed)"
  DetailPrint "$(SyncRemovePrepare)"
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" installer prepare $0'
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 sync_uninstall_failed
  StrCpy $R7 $1 36
  StrLen $2 $R7
  StrCmp $2 "36" 0 sync_uninstall_failed
  sync_uninstall_cleanup:
  StrCpy $SyncFailureMessage "$(SyncCleanupFailed)"
  DetailPrint "$(SyncCleanup)"
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" uninstall lock-held $SyncForceOption'
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 sync_uninstall_failed
  ${If} $DeleteAppDataCheckboxState = 1
    StrCmp $R7 "" sync_uninstall_delete_data
    StrCpy $SyncFailureMessage "$(SyncDataPrepareFailed)"
    DetailPrint "$(SyncDataPrepare)"
    nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" installer finish "$R7"'
    Pop $0
    Pop $1
    StrCmp $0 "0" 0 sync_uninstall_failed
    StrCpy $R7 ""
    sync_uninstall_delete_data:
    StrCpy $SyncFailureMessage "$(SyncDataFailed)"
    DetailPrint "$(SyncData)"
    nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" installer delete-data'
    Pop $0
    Pop $1
    StrCmp $0 "0" 0 sync_uninstall_failed
  ${EndIf}
  StrCpy $SyncFailureMessage "$(SyncFilesFailed)"
  DetailPrint "$(SyncFiles)"
  nsExec::ExecToStack '"$PLUGINSDIR\risunest-sync-remove.exe" --data-dir "$R9" --server "$INSTDIR\risunest-sync-server.exe" installer remove-files $SyncForce'
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 sync_uninstall_failed
  Goto sync_uninstall_done
  sync_uninstall_failed:
  Call un.SyncCommandError
  StrCmp $R7 "" sync_uninstall_cancel_done
  CreateDirectory "$R9\manager-update"
  ClearErrors
  FileOpen $0 "$R9\manager-update\installer-$R7.cancel" w
  IfErrors sync_uninstall_cancel_done
  FileWrite $0 '{"cancel":true}'
  FileClose $0
  sync_uninstall_cancel_done:
  DetailPrint "$SyncFailureMessage"
  IfSilent sync_uninstall_quiet sync_uninstall_interactive
  sync_uninstall_interactive:
  MessageBox MB_OK|MB_ICONSTOP "$SyncFailureMessage"
  sync_uninstall_quiet:
  SetErrorLevel 1
  Abort "$SyncFailureMessage"
  sync_uninstall_done:
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  SetDetailsPrint both
  ${If} $UpdateMode = 1
    Goto sync_uninstall_finished
  ${EndIf}
  StrCmp $R7 "" sync_uninstall_guard_done
  DetailPrint "$(SyncRemoveFinish)"
  nsExec::ExecToStack '"$PLUGINSDIR\risunest-sync-remove.exe" --data-dir "$R9" --server "$INSTDIR\risunest-sync-server.exe" installer finish "$R7"'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_uninstall_guard_done
  StrCpy $SyncFailureMessage "$(SyncRemoveFinishFailed)"
  Call un.SyncCommandError
  DetailPrint "$SyncFailureMessage"
  IfSilent +2
    MessageBox MB_OK|MB_ICONSTOP "$SyncFailureMessage"
  SetErrorLevel 1
  Abort "$SyncFailureMessage"
  sync_uninstall_guard_done:
  DetailPrint "$(SyncForget)"
  nsExec::ExecToStack '"$PLUGINSDIR\risunest-sync-remove.exe" --data-dir "$R9" --server "$INSTDIR\risunest-sync-server.exe" installer forget-removal'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_uninstall_finished
  StrCpy $SyncFailureMessage "$(SyncForgetFailed)"
  Call un.SyncCommandError
  IfSilent +2
    MessageBox MB_OK|MB_ICONSTOP "$SyncFailureMessage"
  SetErrorLevel 1
  Abort "$SyncFailureMessage"
  sync_uninstall_finished:
!macroend

!macro SyncErrorFunction prefix
Function ${prefix}SyncCommandError
  StrCpy $SyncDiagnostic "manager-exit:$0"
  ${If} $0 == "error"
    StrCpy $SyncDiagnostic "manager-launch-failed"
  ${ElseIf} $0 == "timeout"
    StrCpy $SyncDiagnostic "manager-timeout"
  ${ElseIf} $1 != ""
    StrCpy $SyncDiagnostic ""
    StrCpy $2 0
    sync_error_line:
    StrCpy $3 $1 1 $2
    StrCmp $3 "" sync_error_ready
    StrCmp $3 "$\r" sync_error_ready
    StrCmp $3 "$\n" sync_error_ready
    StrCpy $SyncDiagnostic "$SyncDiagnostic$3"
    IntOp $2 $2 + 1
    Goto sync_error_line
  ${EndIf}
  sync_error_ready:
  StrCpy $SyncFailureMessage "$SyncFailureMessage$\r$\n$SyncDiagnostic"
  DetailPrint "$SyncFailureMessage"
FunctionEnd
!macroend
!insertmacro SyncErrorFunction ""
!insertmacro SyncErrorFunction "un."
