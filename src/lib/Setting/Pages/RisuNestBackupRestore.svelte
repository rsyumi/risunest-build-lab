<script lang="ts">
    import { language } from 'src/lang'
    import { alertConfirm, alertNormal } from 'src/ts/alert'
    import { isTauri } from 'src/ts/platform'
    import { LoadLocalBackup } from 'src/ts/drive/backuplocal'
    import { openSyncConflictBackups } from 'src/ts/storage/sync/syncConflictRestore'
    import SettingGroup from '../RisuNest/SettingGroup.svelte'
    import SettingRow from '../RisuNest/SettingRow.svelte'
    import SettingButton from '../RisuNest/SettingButton.svelte'
    import SettingProgress from '../RisuNest/SettingProgress.svelte'
    import { publishNativeOfficialAccountBackup, restoreNativeOfficialAccountBackup } from 'src/ts/storage/sync/nativeOfficialAccountOperations'
    import { presentFileOperationError } from 'src/ts/storage/fileOperationErrorPresentation'
    import { DBState } from 'src/ts/stores.svelte'
    import {
        nativeFileOperation,
        importRisuSaveFromSystemPicker,
        exportRisuSaveFromSystemPicker,
    } from 'src/ts/storage/risuSaveFileRouteProduction.svelte'
    import {
        formatRisuSaveExportResult,
    } from 'src/ts/storage/exportExcludedReport'
    import { restoreBackupFromSystemPicker } from 'src/ts/storage/portableBackupFileRouteProduction.svelte'
    import {
        cancelActiveNativeFileOperation,
        dismissNativeFileOperationOutcome,
    } from 'src/ts/storage/nativeFileJobManager'
    import {
        nativeFileJobProgressText,
        nativeFileJobTitle,
    } from 'src/ts/gui/nativeFileJobProgress'

    let nativeAccountBusy = $state(false)
    let risuSaveOperation = $derived($nativeFileOperation?.kind ?? null)
    let risuSaveStatus = $derived($nativeFileOperation?.status)
    // Imports open the shared progress dialog; only inline operations (exports) use this row.
    let inlineOperation = $derived(
        $nativeFileOperation?.presentation === 'inline',
    )

    async function runRisuSaveOperation(
        kind: 'import' | 'export',
    ): Promise<void> {
        if (risuSaveOperation) return
        const startedAt = Date.now()
        if (
            kind === 'import' &&
            !isTauri &&
            (!(await alertConfirm(language.risuSaveImportConfirm)) ||
                !(await alertConfirm(language.backupLoadConfirm2)))
        )
            return
        if (kind === 'import') {
            // The shared progress dialog reports progress, cancellation, and the outcome.
            try {
                if (isTauri) await restoreBackupFromSystemPicker()
                else await importRisuSaveFromSystemPicker()
            } catch (error) {
                presentFileOperationError(kind, error, startedAt)
            }
            return
        }
        // The export runs behind the shared progress dialog, which reports how it ended.
        dismissNativeFileOperationOutcome()
        try {
            const result = await exportRisuSaveFromSystemPicker()
            if (!result) return
            // The exclusion report replaces the dialog's plain completion message.
            dismissNativeFileOperationOutcome()
            alertNormal(formatRisuSaveExportResult(result))
        } catch (error) {
            presentFileOperationError(kind, error, startedAt)
        }
    }

    async function runNativeAccountOperation<T>(
        operation: () => Promise<T>,
    ): Promise<T | undefined> {
        if (nativeAccountBusy) return undefined
        nativeAccountBusy = true
        try {
            return await operation()
        } finally {
            nativeAccountBusy = false
        }
    }

    async function loadPocketRisuBackup(): Promise<void> {
        if (isTauri) return runRisuSaveOperation('import')
        if (
            (await alertConfirm(language.pocketRisuImportConfirm)) &&
            (await alertConfirm(language.backupLoadConfirm2))
        )
            LoadLocalBackup()
    }

    function restoreOfficialBackup(): Promise<void | undefined> {
        return runNativeAccountOperation(async () => {
            if (
                !(await alertConfirm(
                    language.risuNest.backup.officialRestoreConfirm,
                ))
            )
                return
            if (
                !(await alertConfirm(
                    language.risuNest.backup.officialRestoreInlayWarning,
                ))
            )
                return
            const startedAt = Date.now()
            try {
                const result = await restoreNativeOfficialAccountBackup()
                if (result?.kind === 'missing')
                    alertNormal(language.risuNest.backup.officialMissing)
                else if (result?.kind !== 'activated')
                    alertNormal(language.risuNest.backup.accountRestoreFailed)
            } catch (error) {
                presentFileOperationError('import', error, startedAt, {
                    fallbackMessage: language.risuNest.backup.accountRestoreFailed,
                    committedMessage: language.risuNest.backup.accountRestoreCommittedRestartFailed,
                })
            }
        })
    }

    function publishOfficialBackup(): Promise<void | undefined> {
        return runNativeAccountOperation(async () => {
            if (
                !(await alertConfirm(
                    language.risuNest.backup.officialPublishConfirm,
                ))
            )
                return
            const startedAt = Date.now()
            try {
                await publishNativeOfficialAccountBackup()
            } catch (error) {
                presentFileOperationError('export', error, startedAt)
            }
        })
    }

</script>

<SettingGroup id="risunest-backup" title={language.risuNest.backup.title}>
    <SettingRow
        data-backup-group="files"
        label={language.risuNest.backup.groupFiles}
        help={language.risuNest.backup.filesHelp}
    >
        {#snippet below()}
            {#if risuSaveOperation && inlineOperation}
                <div class="mt-2">
                    <SettingProgress
                        label={nativeFileJobTitle(risuSaveOperation, risuSaveStatus)}
                        detail={nativeFileJobProgressText(risuSaveStatus)}
                    >
                        {#snippet actions()}
                            <SettingButton
                                variant="secondary"
                                onclick={cancelActiveNativeFileOperation}
                                >{language.cancelRisuSaveOperation}</SettingButton
                            >
                        {/snippet}
                    </SettingProgress>
                </div>
            {/if}
        {/snippet}
        <SettingButton
            busy={risuSaveOperation === 'import'}
            disabled={risuSaveOperation !== null}
            onclick={() => runRisuSaveOperation('import')}
            >{language.risuNest.backup.importFile}</SettingButton
        >
        <SettingButton
            variant="secondary"
            busy={risuSaveOperation === 'export'}
            disabled={risuSaveOperation !== null}
            onclick={() => runRisuSaveOperation('export')}
            >{language.risuNest.backup.exportFile}</SettingButton
        >
    </SettingRow>
    <SettingRow
        data-backup-group="restore"
        label={language.risuNest.backup.groupRestore}
        help={language.risuNest.backup.restoreHelp}
    >
        {#if !isTauri}
        <SettingButton
            disabled={risuSaveOperation !== null}
            onclick={loadPocketRisuBackup}
            >{language.loadPocketRisuBackup}</SettingButton
        >
        {/if}
        <SettingButton variant="secondary" onclick={() => openSyncConflictBackups()}
            >{language.syncConflictBackups}</SettingButton
        >
    </SettingRow>
    {#if isTauri && DBState.db.account}
        <SettingRow
            data-backup-group="account"
            label={language.risuNest.backup.groupAccount}
            help={language.risuNest.backup.accountHelp}
        >
            <SettingButton
                busy={risuSaveOperation === 'export'}
                disabled={nativeAccountBusy || risuSaveOperation !== null}
                onclick={publishOfficialBackup}
                >{language.risuNest.backup.officialPublish}</SettingButton
            >
            <SettingButton
                variant="secondary"
                busy={risuSaveOperation === 'import'}
                disabled={nativeAccountBusy || risuSaveOperation !== null}
                onclick={restoreOfficialBackup}
                >{language.risuNest.backup.officialRestore}</SettingButton
            >
        </SettingRow>
    {/if}
</SettingGroup>
