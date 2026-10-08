import { portableBackupSuggestedName } from './portableBackupName'
import { invoke } from '@tauri-apps/api/core'
import { open, save } from '@tauri-apps/plugin-dialog'
import { language } from 'src/lang'
import { alertCheckboxConfirm, alertConfirm, alertNormal } from '../alert'
import { pickIOSBackupSource, materializeIOSBackupSource, discardIOSFile } from './iosFiles'
import { isTauriIOS, isTauri, isTauriAndroid } from '../platform'
import { loadPluginsAfterAuthoritativeRestore } from '../plugins/plugins.svelte'
import {
    discardAndroidSafSource,
    pickAndroidPortableBackupSource,
    materializeAndroidBackupSource,
} from './androidSafBridge'
import {
    selectPortableBackupExport,
    selectPortableBackupRestore,
} from './deviceBackup/selectionDialog'
import { selectPluginValueAssignment } from './pluginValueAssignDialog'
import { runSharedNativeFileOperation } from './nativeFileJobManager'
import {
    NativeFileJobError,
    runNativeArchiveExport,
    runNativeArchiveRestore,
    runNativeBlockRisuSaveRestore,
    runNativeLegacyLocalBackupRestore,
    syntheticNativeFileJobStatus,
    type NativeFileJobOptions,
    type NativeFileRestoreJobOptions,
    type NativeFileJobSource,
    type NativeFileJobStatus,
    type NativeFileJobResult,
} from './nativeFileJobs'
import { describeDesktopSource } from './nativeFileSourceInfo'
import { formatPreservationReport } from './preservationReportMessage'
import { getPersistentDataRuntime } from './persistentDataRuntime.svelte'
import {
    holdServerSyncAfterRestore,
} from './sync/serverSyncProduction'

type SourceInfo = { name: string; bytes?: number }
export interface BackupPickerContext {
    signal: AbortSignal
    onStatus(status: NativeFileJobStatus): void
    onSource(source: SourceInfo): void
}
export type BackupSourceFactory = (
    context: BackupPickerContext,
) => Promise<NativeFileJobSource | null>
export type BackupRestoreResult = NativeFileJobResult | { warningCodes: string[] }
export interface BackupRestoreOptions extends NativeFileRestoreJobOptions {
    onSource?(source: SourceInfo): void
    /**
     * The device holds nothing worth keeping yet, as on the first-run
     * screen: skip the replacement confirmation and describe the section
     * choice as a first import rather than an overwrite.
     */
    firstRun?: boolean
}

function combineSignals(...signals: (AbortSignal | undefined)[]) {
    const controller = new AbortController()
    const cancel = () => controller.abort()
    for (const signal of signals) {
        signal?.addEventListener('abort', cancel, { once: true })
        if (signal?.aborted) cancel()
    }
    return {
        signal: controller.signal,
        dispose() {
            for (const signal of signals)
                signal?.removeEventListener('abort', cancel)
        },
    }
}
function checkSignal(signal: AbortSignal) {
    if (signal.aborted) throw new DOMException('Backup cancelled', 'AbortError')
}

export async function exportPortableBackupFromSystemPicker(
    options: NativeFileJobOptions = {},
): Promise<NativeFileJobResult | null> {
    if (!isTauri)
        throw new NativeFileJobError(
            'native-required',
            'RisuNest backup requires the native app',
        )
    return runSharedNativeFileOperation(
        'export',
        'portable-backup-export',
        async ({ signal, onStatus }) => {
            const joined = combineSignals(signal, options.signal)
            try {
                checkSignal(joined.signal)
                const selection = await selectPortableBackupExport()
                if (!selection) return null
                checkSignal(joined.signal)
                const suggestedName = portableBackupSuggestedName()
                const path =
                    isTauriAndroid || isTauriIOS
                        ? null
                        : await save({
                              defaultPath: suggestedName,
                              filters: [
                                  {
                                      name: 'RisuNest Backup',
                                      extensions: ['risunest'],
                                  },
                              ],
                          })
                if (!isTauriAndroid && !isTauriIOS && !path) return null
                checkSignal(joined.signal)
                return await runNativeArchiveExport(
                    getPersistentDataRuntime(),
                    isTauriIOS
                        ? { type: 'iosFiles', suggestedName }
                        : isTauriAndroid
                          ? { type: 'androidSaf', suggestedName }
                          : { type: 'desktopPath', path: path! },
                    selection,
                    {
                        ...options,
                        signal: joined.signal,
                        confirmSourcePreservation: () => alertConfirm(language.risuNest.backup.sourcePreservationConfirm),
                        onStatus(status) {
                            onStatus(status)
                            options.onStatus?.(status)
                        },
                    },
                )
            } finally {
                joined.dispose()
            }
        },
        { presentation: 'dialog', format: 'library-backup' },
    )
}

export function restoreBackupFromSystemPicker(
    options: BackupRestoreOptions = {},
) {
    return restoreBackupFromNativeSource(async (context) => {
        if (isTauriIOS) {
            const picked = await pickIOSBackupSource(context.signal)
            if (!picked) return null
            context.onSource({ name: picked.name, bytes: picked.bytes })
            return { type: 'iosScoped', token: picked.token }
        }
        if (isTauriAndroid)
            return pickAndroidPortableBackupSource({
                signal: context.signal,
                onSource: (source) =>
                    context.onSource({
                        name: source.displayName,
                        bytes: source.bytes,
                    }),
                onProgress: (progress) =>
                    context.onStatus(
                        syntheticNativeFileJobStatus(
                            {
                                jobId: progress.requestId,
                                kind: 'restore-portable-backup',
                            },
                            'copying-source',
                            {
                                stageUnit: 'bytes',
                                stageCompleted: progress.copiedBytes,
                                ...(progress.totalBytes === null
                                    ? {}
                                    : { stageTotal: progress.totalBytes }),
                            },
                        ),
                    ),
            })
        const path = await open({
            multiple: false,
            directory: false,
            filters: [
                { name: 'Backup', extensions: ['risunest', 'bin', 'risudat'] },
            ],
        })
        if (typeof path !== 'string') return null
        context.onSource(await describeDesktopSource(path))
        return { type: 'desktopPath', path }
    }, options)
}

export async function restoreBackupFromNativeSource(
    source: NativeFileJobSource | BackupSourceFactory,
    options: BackupRestoreOptions = {},
): Promise<BackupRestoreResult | null> {
    if (!isTauri)
        throw new NativeFileJobError(
            'native-required',
            'Native backup restore requires the app',
        )
    return runSharedNativeFileOperation(
        'import',
        'backup-import',
        async (context) => {
            const joined = combineSignals(context.signal, options.signal)
            let input: NativeFileJobSource | null = null
            let selectedCustody: NativeFileJobSource | undefined
            let materializedIOSPath: string | undefined
            let report: NativeFileJobStatus['preservationReport']
            const onStatus = (status: NativeFileJobStatus) => {
                context.onStatus(status)
                options.onStatus?.(status)
                if (status.preservationReport)
                    report = status.preservationReport
            }
            const onSource = (value: SourceInfo) => {
                context.setSource(value)
                options.onSource?.(value)
            }
            try {
                checkSignal(joined.signal)
                input =
                    typeof source === 'function'
                        ? await source({
                              signal: joined.signal,
                              onStatus,
                              onSource,
                          })
                        : source
                if (!input) return null
                if (input.type === 'androidSeekable' || input.type === 'iosScoped') selectedCustody = input
                if (input.type === 'desktopPath')
                    onSource(await describeDesktopSource(input.path))
                checkSignal(joined.signal)
                const format = await invoke<
                    | 'portable'
                    | 'block-risu-save'
                    | 'local-backup'
                >('native_backup_source_format', { source: input })
                checkSignal(joined.signal)
                if (format !== 'portable') {
                    if (input.type === 'androidSeekable') input = await materializeAndroidBackupSource(input.token)
                    else if (input.type === 'iosScoped') {
                        const materialized = await materializeIOSBackupSource(input.token)
                        materializedIOSPath = materialized.path
                        input = { type: 'desktopPath', path: materialized.path }
                    }
                    checkSignal(joined.signal)
                }
                const runtime = getPersistentDataRuntime()
                let replacesLibrary = format !== 'portable'
                const restoreOptions: NativeFileRestoreJobOptions = {
                    ...options,
                    signal: joined.signal,
                    onStatus,
                    assignPluginValues: selectPluginValueAssignment,
                    onBlockingChange(blocking) {
                        context.setBlocking(blocking)
                        options.onBlockingChange?.(blocking)
                    },
                    async afterPortableAdoption(status) {
                        await context.releaseLibraryAfterPortableAdoption(status)
                        await options.afterPortableAdoption?.(status)
                    },
                    beforeActivation: options.beforeActivation,
                    onNativeStatus: options.onNativeStatus,
                    afterRefresh: async (alreadyRestarted) => {
                        await loadPluginsAfterAuthoritativeRestore(alreadyRestarted)
                        await options.afterRefresh?.(alreadyRestarted)
                    },
                }
                const selectedInput = input
                    const result = await (async () => {
                        if (
                            format === 'portable'
                        )
                            return runNativeArchiveRestore(
                                runtime,
                                selectedInput,
                                {
                                    ...restoreOptions,
                                    choosePortableSections: async (preview) => {
                                        const selection =
                                            await selectPortableBackupRestore(
                                                preview,
                                                {
                                                    firstRun:
                                                        options.firstRun ??
                                                        false,
                                                },
                                            )
                                        replacesLibrary =
                                            selection?.library ?? false
                                        return selection
                                    },
                                },
                            )
                        if (format === 'block-risu-save')
                            return runNativeBlockRisuSaveRestore(
                                runtime,
                                selectedInput,
                                restoreOptions,
                            )
                        if (format === 'local-backup') {
                            try {
                                return await runNativeLegacyLocalBackupRestore(runtime, selectedInput, restoreOptions)
                            } catch (error) {
                                if (!(error instanceof NativeFileJobError) || error.code !== 'compatibility-import-required') throw error
                                checkSignal(joined.signal)
                                onStatus(syntheticNativeFileJobStatus({ kind: 'restore-legacy-local-backup' }, 'awaiting-reselect'))
                                const { importLegacyBackupWithWebView } = await import('../drive/backuplocal')
                                return importLegacyBackupWithWebView({
                                    signal: joined.signal,
                                    onStatus,
                                    setSource: onSource,
                                    setPartialWritesPossible: context.setPartialWritesPossible,
                                }, {
                                    beforeActivation: async () => {
                                        await restoreOptions.beforeActivation?.()
                                    },
                                    onCommitted: holdServerSyncAfterRestore,
                                })
                            }
                        }
                        throw new NativeFileJobError(
                            'unsupported-format',
                            'Unsupported backup format',
                        )
                    })()
                    if (report) alertNormal(formatPreservationReport(report))
                    return result
            } finally {
                joined.dispose()
                // A claimed token has already moved to native ownership, so this only removes an
                // unclaimed picker result when confirmation, format probing, or admission failed.
                if (input?.type === 'androidSpool')
                    await discardAndroidSafSource(input.token)
                if (selectedCustody) await invoke('native_portable_source_discard',{source:selectedCustody})
                if (materializedIOSPath) await discardIOSFile(materializedIOSPath)
            }
        },
        { presentation: 'dialog', format: 'library-backup' },
    )
}
