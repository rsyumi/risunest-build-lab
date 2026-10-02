import { describe, expect, it } from 'vitest'
import { parse } from 'svelte/compiler'

import { readFileSync } from 'node:fs'
import { language } from 'src/lang'
import { buildNativeFileJobDialogModel } from 'src/ts/gui/nativeFileJobDialogModel'
const source = readFileSync('src/lib/Setting/Pages/UserSettings.svelte', 'utf8')
const backupSource = readFileSync('src/lib/Setting/Pages/RisuNestBackupRestore.svelte', 'utf8')
const storageSource = readFileSync('src/lib/Setting/Pages/RisuNestStorageDashboard.svelte', 'utf8')
const accountOperationsSource = readFileSync('src/ts/storage/sync/nativeOfficialAccountOperations.ts', 'utf8')
const errorPresentationSource = readFileSync('src/ts/storage/fileOperationErrorPresentation.ts', 'utf8')
const fileJobManagerSource = readFileSync('src/ts/storage/nativeFileJobManager.ts', 'utf8')
const fileJobsSource = readFileSync('src/ts/storage/nativeFileJobs.ts', 'utf8')

describe('UserSettings local backup route', () => {
    it('makes the existing backup import action reachable on Android and uses the common native picker', () => {
        const backup = backupSource
        const ast = parse(backup)
        const guards: string[][] = []
        function visit(node: any, parents: string[] = []) {
            if (!node || typeof node !== 'object') return
            const conditions =
                node.type === 'IfBlock'
                    ? [
                          ...parents,
                          backup.slice(
                              node.expression.start,
                              node.expression.end,
                          ),
                      ]
                    : parents
            if (
                node.type === 'InlineComponent' &&
                node.name === 'SettingButton' &&
                /runRisuSaveOperation\(['"]import['"]\)/.test(
                    backup.slice(node.start, node.end),
                )
            ) {
                guards.push(conditions)
            }
            for (const value of Object.values(node)) {
                if (Array.isArray(value))
                    for (const child of value) visit(child, conditions)
                else if (value && typeof value === 'object')
                    visit(value, conditions)
            }
        }
        visit(ast.html)
        expect(guards).toHaveLength(1)
        for (const expression of guards[0]) {
            // Evaluate the actual Svelte template's platform guard for an Android build.
            expect(
                new Function(
                    'isTauri',
                    'isTauriDesktop',
                    'isTauriAndroid',
                    `return (${expression})`,
                )(true, false, true),
            ).toBe(true)
        }
        expect(backup).toContain(
            'if (isTauri) await restoreBackupFromSystemPicker()',
        )
        expect(backup).toContain(
            "if (isTauri) return runRisuSaveOperation('import')",
        )
    })
    it('keeps local backup and official account controls but removes legacy Drive controls', () => {
        expect(source).toContain('SavePartialLocalBackup()')
        expect(source).toContain('loadRisuAccountBackup')
        expect(source).not.toContain('checkDriver')
        expect(source).not.toContain('googleDriveConnection')
    })

    it('moves RisuNest backup and sync controls to the dedicated page', () => {
        expect(storageSource).toContain('restoreNativePersistentSnapshot')
        expect(source).not.toContain('restoreNativePersistentSnapshot')
        for (const control of [
            'runRisuSaveOperation',
            'openSyncConflictBackups()',
            'publishNativeOfficialAccountBackup()',
            'restoreNativeOfficialAccountBackup()',
            'onclick={cancelActiveNativeFileOperation}',
        ]) {
            expect(backupSource).toContain(control)
            expect(source).not.toContain(control)
        }
        expect(accountOperationsSource).toContain('getNativeOfficialAccountFlow().publish')
        expect(accountOperationsSource).toContain('getNativeOfficialAccountFlow().restore')
        expect(source).not.toContain('getNativeOfficialAccountFlow().publish')
        expect(source).not.toContain('getNativeOfficialAccountFlow().restore')
        expect(fileJobManagerSource).toContain('activeController.abort()')
        expect(accountOperationsSource).toContain("runSharedNativeFileOperation('export', 'official-account-publish'")
        expect(accountOperationsSource).toContain("runSharedNativeFileOperation('import', 'official-account-restore'")
        expect(accountOperationsSource).toContain("{ presentation: 'dialog', format: 'library-backup' }")
    })

    it('keeps official account actions behind the existing account gate', () => {
        const restoreGroup = backupSource.indexOf(
            '{language.risuNest.backup.groupRestore}',
        )
        const accountGate = backupSource.indexOf(
            '{#if isTauri && DBState.db.account}',
        )
        const officialRestore = backupSource.indexOf(
            '{language.risuNest.backup.officialRestore}',
        )
        expect(restoreGroup).toBeGreaterThan(-1)
        expect(accountGate).toBeGreaterThan(restoreGroup)
        expect(officialRestore).toBeGreaterThan(accountGate)
    })

    it('uses localized safe copy for official backup actions and native failures', () => {

        for (const key of [
            'officialMissing',
            'officialPublishConfirm',
        ])
            expect(backupSource).toContain(`language.risuNest.backup.${key}`)
        expect(fileJobsSource).toContain('language.risuNest.backup.officialRestoreInlayWarning')
        expect(backupSource.slice(backupSource.indexOf('async function loadPocketRisuBackup'), backupSource.indexOf('function restoreOfficialBackup'))).not.toContain('alertCheckboxConfirm')
        expect(backupSource.slice(backupSource.indexOf('function restoreOfficialBackup'), backupSource.indexOf('async function publishOfficialBackup'))).not.toContain('alertCheckboxConfirm')

        expect(backupSource).toContain("presentFileOperationError('export', error, startedAt)")
        expect(errorPresentationSource).toContain('const text = language.risuNest.backup')
        expect(errorPresentationSource).toContain('options.fallbackMessage ?? text.actionFailed')
        const completion = buildNativeFileJobDialogModel(null, {
            kind: 'export', format: 'library-backup', state: 'succeeded',
            startedAt: 0, finishedAt: 1, observedStages: [], warningCodes: [], partialWritesPossible: false,
            status: {
                jobId: 'synthetic-publication', kind: 'official-publication-upload', state: 'succeeded',
                phase: 'complete', progress: { completedBytes: 0, completedItems: 0 },
            },
        }, 1)
        expect(completion.open).toBe(true)
        expect(completion.title).toBe(language.risuNest.backup.officialPublish)
        expect(completion.terminal?.summary).toBe(language.risuNest.importDialog.resultExportSucceeded)

        expect(backupSource).not.toContain('status.phase}')
        expect(backupSource).not.toContain('${status.phase}')
        expect(backupSource).not.toContain(
            'alertError(error instanceof Error ? error : String(error))',
        )
    })
})
