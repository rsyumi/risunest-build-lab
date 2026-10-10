!ifndef RISUNEST_SYNC_DEFAULT_INSTALL_DIR
  !define RISUNEST_SYNC_DEFAULT_INSTALL_DIR "$LOCALAPPDATA\RisuNest Sync"
!endif
!ifndef RISUNEST_SYNC_INSTALL_DIR
  !define RISUNEST_SYNC_INSTALL_DIR "$LOCALAPPDATA\RisuNestSync"
!endif

Var SyncFailureMessage

!macro NSIS_HOOK_PREINSTALL
  SetDetailsPrint both
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  StrCmp $INSTDIR "${RISUNEST_SYNC_DEFAULT_INSTALL_DIR}" 0 sync_install_dir_ready
  StrCpy $INSTDIR "${RISUNEST_SYNC_INSTALL_DIR}"
  SetOutPath $INSTDIR
  sync_install_dir_ready:
  StrCpy $R7 ""
  IfFileExists "$INSTDIR\risunest-sync-manager.exe" 0 sync_prepare_done
  System::Call 'kernel32::GetCurrentProcessId()i.r0'
  DetailPrint "Stopping RisuNest Sync and preparing the installation..."
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" installer prepare $0'
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 sync_prepare_failed
  StrCpy $R7 $1 36
  StrLen $2 $R7
  StrCmp $2 "36" sync_prepare_done sync_prepare_failed
  sync_prepare_failed:
  StrCpy $SyncFailureMessage "RisuNest Sync could not be prepared for installation. Check the server status in RisuNest Sync and try again."
  DetailPrint "$SyncFailureMessage"
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
  DetailPrint "Applying scheduled update settings..."
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" update schedule reconcile lock-held'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_register_startup
  StrCpy $SyncFailureMessage "Scheduled update settings could not be applied. RisuNest Sync setup did not finish."
  Goto sync_install_failed
  sync_register_startup:
  DetailPrint "Registering automatic server startup..."
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" autostart install lock-held'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_start
  StrCpy $SyncFailureMessage "Automatic server startup could not be registered. RisuNest Sync setup did not finish."
  Goto sync_install_failed
  sync_start:
  DetailPrint "Starting RisuNest Sync and checking the server response..."
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" installer start-and-verify lock-held'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_release_guard
  StrCpy $SyncFailureMessage "The RisuNest Sync server could not be started or verified. Open RisuNest Sync and check the server status."
  Goto sync_install_failed
  sync_install_failed:
  DetailPrint "$SyncFailureMessage"
  StrCpy $R6 "1"
  SetErrorLevel 1
  IfSilent sync_release_guard 0
  MessageBox MB_OK|MB_ICONEXCLAMATION "$SyncFailureMessage"
  sync_release_guard:
  StrCmp $R7 "" sync_install_done
  StrCmp $R6 "1" sync_cancel_guard
  DetailPrint "Finishing RisuNest Sync installation..."
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" installer finish "$R7"'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_install_done
  StrCpy $SyncFailureMessage "RisuNest Sync installation could not be finalized."
  Goto sync_install_done
  sync_cancel_guard:
  CreateDirectory "$LOCALAPPDATA\RisuNestSyncData\manager-update"
  FileOpen $0 "$LOCALAPPDATA\RisuNestSyncData\manager-update\installer-$R7.cancel" w
  IfErrors sync_cancel_guard_failed
  FileWrite $0 '{"cancel":true}'
  FileClose $0
  Goto sync_install_done
  sync_cancel_guard_failed:
  DetailPrint "Recovery could not be requested. Check the server status in RisuNest Sync."
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
    StrCpy $SyncFailureMessage "RisuNest Sync could not be prepared for the update. Program files were preserved."
    DetailPrint "Stopping RisuNest Sync for the update..."
    nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" prepare-update'
    Pop $0
    Pop $1
    StrCmp $0 "0" sync_uninstall_done sync_uninstall_failed
  ${EndIf}
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  InitPluginsDir
  StrCpy $SyncFailureMessage "RisuNest Sync removal could not be prepared. Program files were preserved."
  ClearErrors
  CopyFiles /SILENT "$INSTDIR\risunest-sync-manager.exe" "$PLUGINSDIR\risunest-sync-remove.exe"
  IfErrors sync_uninstall_failed
  StrCpy $R9 "$LOCALAPPDATA\RisuNestSyncData"
  ${un.GetParameters} $0
  ReadEnvStr $1 RISUNEST_SYNC_UNINSTALL_DATA_DIR
  ${If} $1 != ""
    StrCpy $R9 $1
  ${EndIf}
  ClearErrors
  ${un.GetOptions} $0 "/DELETEAPPDATA" $1
  ${IfNot} ${Errors}
    StrCpy $DeleteAppDataCheckboxState 1
  ${EndIf}
  StrCpy $SyncFailureMessage "RisuNest Sync removal checks did not pass. Program files were preserved."
  DetailPrint "Checking whether RisuNest Sync can be removed..."
  ${If} $DeleteAppDataCheckboxState = 1
    nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" uninstall --dry-run --delete-data'
  ${Else}
    nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" uninstall --dry-run'
  ${EndIf}
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 sync_uninstall_failed
  StrCpy $R7 ""
  IfFileExists "$R9\risunest-sync-instance.json" 0 sync_uninstall_cleanup
  System::Call 'kernel32::GetCurrentProcessId()i.r0'
  StrCpy $SyncFailureMessage "RisuNest Sync could not be prepared for removal. Program files were preserved."
  DetailPrint "Stopping RisuNest Sync and preparing removal..."
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" installer prepare $0'
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 sync_uninstall_failed
  StrCpy $R7 $1 36
  StrLen $2 $R7
  StrCmp $2 "36" 0 sync_uninstall_failed
  sync_uninstall_cleanup:
  StrCpy $SyncFailureMessage "RisuNest Sync could not stop the server or remove automatic startup and scheduled updates. Program files were preserved."
  DetailPrint "Stopping the server and removing startup and update tasks..."
  nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" uninstall lock-held'
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 sync_uninstall_failed
  ${If} $DeleteAppDataCheckboxState = 1
    StrCmp $R7 "" sync_uninstall_delete_data
    StrCpy $SyncFailureMessage "RisuNest Sync could not finish preparing to remove server data. Program files were preserved."
    DetailPrint "Preparing to remove RisuNest Sync server data..."
    nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" installer finish "$R7"'
    Pop $0
    Pop $1
    StrCmp $0 "0" 0 sync_uninstall_failed
    StrCpy $R7 ""
    sync_uninstall_delete_data:
    StrCpy $SyncFailureMessage "RisuNest Sync server data could not be completely removed. Program files were preserved; retry removal to finish."
    DetailPrint "Removing RisuNest Sync server data..."
    nsExec::ExecToStack '"$INSTDIR\risunest-sync-manager.exe" --data-dir "$R9" installer delete-data'
    Pop $0
    Pop $1
    StrCmp $0 "0" 0 sync_uninstall_failed
  ${EndIf}
  Goto sync_uninstall_done
  sync_uninstall_failed:
  StrCmp $R7 "" sync_uninstall_cancel_done
  CreateDirectory "$R9\manager-update"
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
  CreateDirectory "$R9\manager-update"
  FileOpen $0 "$R9\manager-update\installer-$R7.release" w
  IfErrors sync_uninstall_guard_failed
  FileWrite $0 '{"release":true}'
  FileClose $0
  DetailPrint "Finishing RisuNest Sync removal..."
  StrCpy $R6 300
  sync_uninstall_wait_guard:
  IfFileExists "$R9\manager-update\installer-$R7.ready" 0 sync_uninstall_guard_done
  Sleep 100
  IntOp $R6 $R6 - 1
  IntCmp $R6 0 sync_uninstall_guard_failed sync_uninstall_guard_failed sync_uninstall_wait_guard
  sync_uninstall_guard_failed:
  StrCpy $SyncFailureMessage "RisuNest Sync program files were removed, but removal could not be finalized."
  SetErrorLevel 1
  Abort "$SyncFailureMessage"
  sync_uninstall_guard_done:
  DetailPrint "Removing RisuNest Sync installation registration..."
  nsExec::ExecToStack '"$PLUGINSDIR\risunest-sync-remove.exe" --data-dir "$R9" --server "$INSTDIR\risunest-sync-server.exe" installer forget-removal'
  Pop $0
  Pop $1
  StrCmp $0 "0" sync_uninstall_finished
  StrCpy $SyncFailureMessage "RisuNest Sync program files were removed, but installation registration could not be removed."
  SetErrorLevel 1
  Abort "$SyncFailureMessage"
  sync_uninstall_finished:
!macroend
