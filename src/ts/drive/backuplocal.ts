import { runWithMobileBackgroundTask } from '../mobileBackgroundTask'
import { BaseDirectory, open, writeFile } from "@tauri-apps/plugin-fs";
import localforage from "localforage";
import { alertError, alertNormal, alertWait, alertMd, alertCheckboxConfirm } from "../alert";
import { LocalWriter, forageStorage } from "../globalApi.svelte";
import { resolveBlobStore } from "../storage/platformBlobStore";
import type { BlobStore } from "../storage/blobStore";
import {
    collectBackupAssetKeys,
    collectReferencedBackupInlays,
    decodeBackupInlayEntry,
    encodeBackupInlayEntry,
    getBackupInlayName,
    isBackupInlayEntryName,
    isLegacyBackupAssetKey,
    readBackupAsset,
    readLocalBackupAsset,
    scanPinnedBackupRecords,
    writeBackupAsset,
} from "./backupAssets";
import { classifyPocketRisuEntry, PocketRisuInlayImporter } from "./pocketRisuBackup";
import { createLegacyBackupAttachments } from './legacyBackupAttachments';
import { isTauri, isTauriDesktop } from "src/ts/platform"
import { decodeRisuSave } from "../storage/risuSave";
import { relaunch } from "../desktopRelaunch";
import { decryptBuffer, encryptBuffer, sleep } from "../util";
import { hubURL } from "../characterCards";
import { language } from "src/lang";
import { getColdStorageBackupKey, isColdStorageBackupData, listColdDataKeys } from "../process/coldstorage.svelte";
import { getColdStorageAffectedCharacters } from "../process/coldstorageData";
import { expandColdPayloads } from "../process/coldPayloadExpansion";
import { getPersistentDataRuntime, publishCurrentOfficialRevision, replacePersistentDatabase } from "../storage/persistentDataRuntime.svelte";
import { installLocalBackup } from "../storage/databaseRestore";
import { type PinnedRisuSaveExport, withFlushedRisuSaveExport } from "../storage/risuSaveStoreAdapter";
import { emptyNativeImportCounts, NativeFileJobActivationCommittedError, NativeFileJobError, syntheticNativeFileJobStatus, type NativeFileJobStage } from "../storage/nativeFileJobs";
import { recordNativeLogError } from "../nativeLog";
import { NativeFileOperationBusyError, runSharedNativeFileOperation } from "../storage/nativeFileJobManager";
import type { SharedNativeFileOperationContext } from "../storage/nativeFileJobManager";

type LegacyLocalBackupFallbackContext = Pick<SharedNativeFileOperationContext,
    'signal' | 'onStatus' | 'setSource' | 'setPartialWritesPossible'>;

const NATIVE_BACKUP_READ_BYTES = 1024 * 1024

export async function* streamNativeBackupFile(
    path: string,
    byteLength: number,
): AsyncGenerator<Uint8Array> {
    let file: Awaited<ReturnType<typeof open>> | undefined
    let primaryError: unknown
    try {
        file = await open(path, { read: true })
        const buffer = new Uint8Array(NATIVE_BACKUP_READ_BYTES)
        let readBytes = 0
        while (true) {
            const length = await file.read(buffer)
            if (length === null) break
            if (length <= 0 || length > buffer.byteLength) {
                throw new Error('Native backup source returned an invalid read length')
            }
            if (readBytes + length > byteLength) {
                throw new Error('Native backup source exceeded its declared length')
            }
            readBytes += length
            yield buffer.slice(0, length)
        }
        if (readBytes !== byteLength) {
            throw new Error('Native backup source ended before its declared length')
        }
    } catch (error) {
        primaryError = error
        throw error
    } finally {
        if (file) {
            try {
                await file.close()
            } catch (error) {
                if (primaryError === undefined) throw error
            }
        }
    }
}

type LocalBackupDatabaseWriter = Pick<LocalWriter, 'writeBackup' | 'writeBackupStream'>

export function writePinnedLocalBackupDatabase(
    writer: LocalBackupDatabaseWriter,
    pinned: PinnedRisuSaveExport,
): Promise<void> {
    if (pinned.withNativeFile) {
        return pinned.withNativeFile(
            { omitAccount: true },
            (file) => writer.writeBackupStream(
                'database.risudat',
                file.bytes,
                streamNativeBackupFile(file.path, file.bytes),
            ),
        )
    }
    return pinned.collectBytes({ omitAccount: true }).then((bytes) => (
        writer.writeBackup('database.risudat', bytes)
    ))
}

function getBasename(data:string){
    const baseNameRegex = /\\/g
    const splited = data.replace(baseNameRegex, '/').split('/')
    const lasts = splited[splited.length-1]
    return lasts
}

export async function SaveLocalBackup(){
    return saveLocalBackupWithWebView()
}


async function saveLocalBackupWithWebView(){
    if (!isTauri) await forageStorage.Init()
    const blobStore = await resolveBlobStore()
    alertWait("Saving local backup...")
    return runWithMobileBackgroundTask('backup', () => withFlushedRisuSaveExport(
        getPersistentDataRuntime(),
        'local-backup',
        (pinned) => saveLocalBackupSnapshot(blobStore, pinned),
    ), undefined, true)
}

async function saveLocalBackupSnapshot(blobStore: BlobStore, pinned: PinnedRisuSaveExport) {
    const { root, accumulator } = await scanPinnedBackupRecords(pinned.reader, 'full')
    const references = accumulator.finish()

    const writer = new LocalWriter()
    const r = await writer.init('RisuNest Backup', ['bin'], 'risu-backup.bin')
    if(!r){
        alertError('Failed')
        return
    }

    const assetMap = references.assetLabels
    const missingAssets: string[] = []

    const backupAssetKeys = await collectBackupAssetKeys(
        blobStore,
        references.assetKeys,
    )
    // Archive entry names are basename-flattened by writeBackup, so distinct
    // keys (e.g. a flat plugin asset and a nested legacy asset) can collide.
    // Skip duplicates so restore cannot silently overwrite one with the other.
    const writtenAssetNames = new Set<string>()
    for(let i=0;i<backupAssetKeys.length;i++){
        const key = backupAssetKeys[i]
        let message = `Saving local Backup... (${i + 1} / ${backupAssetKeys.length})`
        if (missingAssets.length > 0) {
            const skippedItems = missingAssets.map(key => {
                const assetInfo = assetMap.get(key);
                return assetInfo ? `'${assetInfo.assetName}' from ${assetInfo.charName}` : `'${key}'`;
            }).join(', ');
            message += `\n(Skipping... ${skippedItems})`;
        }
        alertWait(message)

        let data = await blobStore.read(key)
        let readRemotely = false
        if (data === null && forageStorage.isAccount) {
            if (root.skipSavingAssetsOnWebSync) {
                continue
            }
            data = await readBackupAsset(blobStore, key, true)
            readRemotely = true
        }
        if (data) {
            const entryBasename = getBasename(key)
            if (!writtenAssetNames.has(entryBasename)) {
                writtenAssetNames.add(entryBasename)
                await writer.writeBackup(isTauri ? key.slice('assets/'.length) : key, data)
            } else {
                console.warn(`Skipping backup asset ${key}: entry name ${entryBasename} already written`)
            }
        } else {
            missingAssets.push(key)
        }
        if (readRemotely) {
            await sleep(1000)
        }
    }

    const inlays = await collectReferencedBackupInlays(blobStore, references.inlayKeys)
    for(let i=0;i<inlays.length;i++){
        const metadata = inlays[i]
        alertWait(`Saving local Backup inlays... (${i + 1} / ${inlays.length})`)
        const data = await blobStore.read(metadata.key)
        if (data === null) {
            missingAssets.push(metadata.key)
            continue
        }
        await writer.writeBackup(
            getBackupInlayName(metadata.key),
            encodeBackupInlayEntry(metadata, data),
        )
    }

    alertWait(`Saving local Backup... (Saving database)`)

    if(forageStorage.isAccount && location.origin.endsWith('risuai.xyz')){
        const dbData = await pinned.collectBytes({ omitAccount: true })
        const time = Date.now()
        const key = (await (await fetch(`https://sv.risuai.xyz/cryptokey?key=${time}`)).json()).key
        const encrypted = await encryptBuffer(dbData, key)
        await writer.writeBackup('encryption.risudat', new TextEncoder().encode(JSON.stringify({ time, type: 'account' })))
        await writer.writeBackup('database.risudat', new Uint8Array(encrypted))
    } else {
        await writePinnedLocalBackupDatabase(writer, pinned)
    }

    await writer.close()

    if (missingAssets.length > 0) {
        let message = 'Backup Successful, but the following assets were missing and skipped:\n\n'
        for (const key of missingAssets) {
            const assetInfo = assetMap.get(key)
            if (assetInfo) {
                message += `* **${assetInfo.assetName}** (from *${assetInfo.charName}*)  \n  *File: ${key}*\n`
            } else {
                message += `* **Unknown Asset**  \n  *File: ${key}*\n`
            }
        }
        alertMd(message)
    } else {
        alertNormal('Success')
    }
}

/**
 * Saves a partial local backup with only critical assets.
 * 
 * Differences from SaveLocalBackup:
 * - Only includes profile images for characters/groups (excludes emotion images, additional assets, VITS files, CC assets)
 * - Additionally includes: persona icons, folder images, bot preset images
 * - Processes only assets in assetMap (selective) instead of all .png files in assets folder
 * - Faster and more efficient for quick backups
 * - Ideal for backing up core visual identity without bulk data
 */
export async function SavePartialLocalBackup(){
    if (!isTauri) await forageStorage.Init()
    const blobStore = await resolveBlobStore()
    if (!(await alertCheckboxConfirm({
        title: language.checkboxConfirmation.partialBackupTitle,
        description: language.checkboxConfirmation.partialBackupDescription,
        checkboxLabel: language.checkboxConfirmation.partialBackup,
        actionLabel: language.confirm,
        cancelLabel: language.cancel,
        requireChecked: true,
    })).confirmed) return

    alertWait("Saving partial local backup...")
    return runWithMobileBackgroundTask('backup', () => withFlushedRisuSaveExport(
        getPersistentDataRuntime(),
        'partial-local-backup',
        (pinned) => savePartialLocalBackupSnapshot(blobStore, pinned),
    ), undefined, true)
}

async function savePartialLocalBackupSnapshot(blobStore: BlobStore, pinned: PinnedRisuSaveExport) {
    const { accumulator } = await scanPinnedBackupRecords(pinned.reader, 'partial')
    const references = accumulator.finish()

    const writer = new LocalWriter()
    const r = await writer.init('RisuNest Backup', ['bin'], 'risu-partial-backup.bin')
    if(!r){
        alertError('Failed')
        return
    }

    const assetMap = references.assetLabels
    const missingAssets: string[] = []

    const assetKeys = references.assetKeys
    for(let i=0;i<assetKeys.length;i++){
        const key = assetKeys[i]
        let message = `Saving partial local backup... (${i + 1} / ${assetKeys.length})`
        if (missingAssets.length > 0) {
            const skippedItems = missingAssets.map(key => {
                const assetInfo = assetMap.get(key);
                return assetInfo ? `'${assetInfo.assetName}' from ${assetInfo.charName}` : `'${key}'`;
            }).join(', ');
            message += `\n(Skipping... ${skippedItems})`;
        }
        alertWait(message)

        let data = await readLocalBackupAsset(blobStore, key)
        let readRemotely = false
        if (data === null && forageStorage.isAccount) {
            data = await readBackupAsset(blobStore, key, true)
            readRemotely = true
        }
        if (data) {
            await writer.writeBackup(key, data)
        } else {
            missingAssets.push(key)
        }
        if (readRemotely) {
            await sleep(100)
        }
    }

    alertWait(`Saving partial local backup... (Saving database)`) 
    await writePinnedLocalBackupDatabase(writer, pinned)
    await writer.close()

    if (missingAssets.length > 0) {
        let message = 'Partial backup successful, but the following profile images were missing and skipped:\n\n'
        for (const key of missingAssets) {
            const assetInfo = assetMap.get(key)
            if (assetInfo) {
                message += `* **${assetInfo.assetName}** (from *${assetInfo.charName}*)  \n  *File: ${key}*\n`
            } else {
                message += `* **Unknown Asset**  \n  *File: ${key}*\n`
            }
        }
        alertMd(message)
    } else {
        alertNormal('Success')
    }
}

export function LoadLocalBackup(confirmationTitle = language.backupLoadConfirm): Promise<void> {
    return runSharedNativeFileOperation(
        'import',
        'legacy-local-backup-import',
        (context) => importLegacyBackupWithWebView(context, {}, confirmationTitle),
        { presentation: 'dialog', format: 'local-backup' },
    ).then(() => undefined, handleLegacyImportFailure)
}

/**
 * The progress dialog already shows the outcome to the user; this keeps the
 * diagnostics record and surfaces only the one failure the dialog never sees.
 */
function handleLegacyImportFailure(error: unknown): void {
    if (error instanceof DOMException && error.name === 'AbortError') return
    if (error instanceof NativeFileOperationBusyError) {
        alertError(language.risuNest.backup.actionFailed)
        return
    }
    console.error(error)
    let message: string
    if (error instanceof NativeFileJobActivationCommittedError) {
        const detail = error.cause instanceof Error ? error.cause.message : String(error.cause)
        message = `Backup data was imported, but the app could not refresh. Restart the app before trying another import.\n[${error.code}] ${detail}`
    } else {
        const code = error instanceof NativeFileJobError ? error.code : 'import-error'
        const detail = error instanceof Error ? error.message : String(error)
        message = `Backup import failed [${code}]: ${detail}`
    }
    void recordNativeLogError(message).catch(() => {})
}

const WEB_LEGACY_IMPORT_JOB = {
    jobId: 'web-legacy-local-backup',
    kind: 'restore-legacy-local-backup' as const,
}

function cancelledError(): DOMException {
    return new DOMException('Backup import was cancelled', 'AbortError')
}

function pickWebBackupFile(signal: AbortSignal): Promise<File | null> {
    if (signal.aborted) return Promise.resolve(null)
    return new Promise((resolve) => {
        const input = document.createElement('input')
        input.type = 'file'
        input.accept = '.bin'
        let settled = false
        const finish = (file: File | null) => {
            if (settled) return
            settled = true
            signal.removeEventListener('abort', onAbort)
            input.remove()
            resolve(file)
        }
        const onAbort = () => finish(null)
        input.onchange = async () => {
            finish(input.files?.[0] ?? null)
        }
        input.oncancel = () => finish(null)
        signal.addEventListener('abort', onAbort, { once: true })
        input.click()
    })
}

/**
 * WebView importer for RisuAI and PocketRisu `.bin` backups. Runs inside a
 * dialog-presented operation: it reports every entry it classifies, marks the
 * operation as partially written once anything reaches the blob store, and
 * ends by restarting the app after the database is installed.
 */
export async function importLegacyBackupWithWebView(
    context: LegacyLocalBackupFallbackContext,
    lifecycle: { beforeActivation?(): Promise<void>; onCommitted?(): void } = {},
    confirmationTitle = language.backupLoadConfirm,
): Promise<{ warningCodes: string[], upstreamLosses: { coldMissing: number, invalidInlays: number, pocketInlays: number } } | null> {
    const file = await pickWebBackupFile(context.signal)
    if (!file) return null
    context.setSource({ name: file.name, bytes: file.size })

    const encryptionMeta: {
        type: 'none' | 'account'
        time?: number
    } = {
        type: 'none',
    }
    if (!isTauri) await forageStorage.Init()
    const blobStore = await resolveBlobStore()
    const counts = emptyNativeImportCounts()
    const warningCodes: string[] = []
    const upstreamLosses = { coldMissing: 0, invalidInlays: 0, pocketInlays: 0 }
    let bytesRead = 0
    let currentItem: string | undefined
    const attachments = createLegacyBackupAttachments(blobStore)
    const markAttachmentWritten = () => {
        counts.attachmentsPrepared += 1
    }
    const report = (stage: NativeFileJobStage) => {
        context.onStatus(syntheticNativeFileJobStatus(WEB_LEGACY_IMPORT_JOB, stage, {
            stageUnit: 'bytes',
            stageCompleted: bytesRead,
            stageTotal: file.size,
            progress: {
                completedBytes: bytesRead,
                totalBytes: file.size,
                completedItems: counts.entriesRead,
                ...(counts.entriesTotal === undefined ? {} : { totalItems: counts.entriesTotal }),
            },
            counts: { ...counts },
            ...(currentItem === undefined ? {} : { currentItem }),
        }))
    }
    const checkCancelled = () => {
        if (context.signal.aborted) throw cancelledError()
    }
    let attachmentStagingError: unknown
    const pocketRisuInlays = new PocketRisuInlayImporter(async (id, bytes, metadata) => {
        try {
            await attachments.put(id, bytes, metadata)
        } catch (error) {
            attachmentStagingError = error
            throw error
        }
        markAttachmentWritten()
    })

    report('reading-archive')
    let pendingDatabase: Uint8Array | null = null
    let sawEncryption = false
    const restoredColdStoragePayloads = new Map<string, unknown>()
    const deferredEntries: { name: string, start: number, length: number }[] = []
    const restoreAttachment = async (name: string, data: Uint8Array) => {
        const inlayEntry = decodeBackupInlayEntry(name, data)
        if (inlayEntry) {
            counts.inlays += 1
            await attachments.put(inlayEntry.key, inlayEntry.data, inlayEntry.metadata)
            markAttachmentWritten()
            await sleep(10)
            return
        }
        if (isBackupInlayEntryName(name)) {
            counts.skipped += 1
            upstreamLosses.invalidInlays += 1
            if (!warningCodes.includes('upstream-restore-losses')) {
                warningCodes.push('upstream-restore-losses')
            }
            await sleep(10)
            return
        }
        const pocketRisuEntry = classifyPocketRisuEntry(name)
        if (pocketRisuEntry) {
            if (pocketRisuEntry.kind === 'skip') {
                counts.skipped += 1
            } else {
                if (pocketRisuEntry.kind === 'inlay-data' || pocketRisuEntry.kind === 'inlay-legacy') {
                    counts.pocketMedia += 1
                } else {
                    counts.pocketMetadata += 1
                }
                await pocketRisuInlays.add(pocketRisuEntry, new Uint8Array(data))
                if (attachmentStagingError) throw attachmentStagingError
            }
            await sleep(10)
            return
        }
        const coldStorageKey = getColdStorageBackupKey(name)
        let handledAsColdStorage = false

        if (coldStorageKey) {
            handledAsColdStorage = true
            counts.coldStorage += 1
            try {
                const text = new TextDecoder().decode(data)
                const jsonData = JSON.parse(text)

                if (isColdStorageBackupData(jsonData)) {
                    restoredColdStoragePayloads.set(coldStorageKey, jsonData)
                } else {
                    console.warn(`Skipping invalid cold storage backup item ${name}`)
                    counts.skipped += 1
                }
            } catch (e) {
                console.error(`Failed to parse cold storage item ${coldStorageKey}:`, e)
                counts.skipped += 1
            }
        }

        if (!handledAsColdStorage) {
            const key = `assets/${name}`
            if (isLegacyBackupAssetKey(key)) {
                counts.assets += 1
                await writeBackupAsset({ ...blobStore, put: attachments.put }, key, data)
                markAttachmentWritten()
            } else {
                counts.skipped += 1
            }
        }

    }
    let committedRevision: number | null = null
    try {
        let offset = 0
        while (offset < file.size) {
            checkCancelled()
            if (file.size - offset < 4) throw new NativeFileJobError('invalid-source', 'The backup file has a truncated entry')
            const nameLength = new DataView(await file.slice(offset, offset + 4).arrayBuffer()).getUint32(0, true)
            offset += 4
            if (nameLength > file.size - offset - 4) throw new NativeFileJobError('invalid-source', 'The backup file has a truncated entry')
            const name = new TextDecoder().decode(await file.slice(offset, offset + nameLength).arrayBuffer())
            offset += nameLength
            const dataLength = new DataView(await file.slice(offset, offset + 4).arrayBuffer()).getUint32(0, true)
            offset += 4
            if (dataLength > file.size - offset) throw new NativeFileJobError('invalid-source', 'The backup file has a truncated entry')
            counts.entriesRead += 1
            currentItem = name
            if (name === 'encryption.risudat' || name === 'database.risudat') {
                const data = new Uint8Array(await file.slice(offset, offset + dataLength).arrayBuffer())
                if (name === 'encryption.risudat') {
                    if (sawEncryption) throw new NativeFileJobError('invalid-source', 'Duplicate encryption entry')
                    sawEncryption = true
                    try {
                        const meta = JSON.parse(new TextDecoder().decode(data)) as typeof encryptionMeta
                        if (meta.type === 'account' && meta.time) {
                            encryptionMeta.type = 'account'
                            encryptionMeta.time = meta.time
                        } else {
                            alertError('Invalid encryption metadata, will attempt to load database backup without decryption.')
                        }
                    } catch (e) {
                        console.error('Failed to parse encryption metadata:', e)
                        alertError('Failed to parse encryption metadata, will attempt to load database backup without decryption.')
                    }
                } else if (name === 'database.risudat') {
                    if (pendingDatabase) throw new NativeFileJobError('invalid-source', 'Duplicate database entry')
                    pendingDatabase = new Uint8Array(data)
                }
            } else if (getColdStorageBackupKey(name)) {
                const data = new Uint8Array(await file.slice(offset, offset + dataLength).arrayBuffer())
                await restoreAttachment(name, data)
            } else {
                deferredEntries.push({ name, start: offset, length: dataLength })
            }
            offset += dataLength
            bytesRead = offset
            report('reading-archive')
            checkCancelled()
        }
        counts.entriesTotal = counts.entriesRead
        currentItem = undefined

        if (!pendingDatabase) {
            throw new NativeFileJobError('invalid-source', 'The backup file has no database entry')
        }
        checkCancelled()
        report('decoding-database')

        let db = pendingDatabase
        if (encryptionMeta.type === 'account' && encryptionMeta.time) {
            try {
                const key = (await (await fetch(`https://sv.risuai.xyz/cryptokey?key=${encryptionMeta.time}`)).json()).key
                const decrypted = await decryptBuffer(db, key)
                db = new Uint8Array(decrypted)
            }
            catch (e) {
                console.error('Failed to decrypt database backup:', e)
                alertError('Failed to decrypt database backup, will attempt to load it without decryption.')
            }
        }
        const dbData = await decodeRisuSave(db)
        counts.characters = dbData.characters?.length ?? 0
        counts.charactersTotal = counts.characters
        counts.presets = dbData.botPresets?.length ?? 0
        report('decoding-database')

        const missingColdStorageKeys = (await listColdDataKeys(dbData))
            .filter((key) => !restoredColdStoragePayloads.has(key))
        const affectedCold = getColdStorageAffectedCharacters(dbData, missingColdStorageKeys)
        upstreamLosses.coldMissing = missingColdStorageKeys.length
        const description = missingColdStorageKeys.length === 0 ? language.checkboxConfirmation.dataReplacementDescription
            : `${language.checkboxConfirmation.dataReplacementDescription}\n\n${language.errors.coldStorageIncompleteRestoreConfirm(affectedCold.characterNames.join(', '), missingColdStorageKeys.length, affectedCold.unresolvedKeys.length, false)}`
        if (!isTauri && !(await alertCheckboxConfirm({
            title: confirmationTitle,
            description,
            checkboxLabel: language.checkboxConfirmation.dataReplacement,
            actionLabel: language.confirm,
            cancelLabel: language.cancel,
            requireChecked: true,
        })).confirmed) {
            throw cancelledError()
        }
        if (missingColdStorageKeys.length > 0 && !warningCodes.includes('upstream-restore-losses')) warningCodes.push('upstream-restore-losses')
        await expandColdPayloads(dbData, async (key) => restoredColdStoragePayloads.get(key) ?? null)
        checkCancelled()

        report('activating')
        await installLocalBackup(dbData, {
            ...(isTauri ? {upstreamImportWarnings: [description]} : {}),
            replaceDatabase: async (...args) => {
                await lifecycle.beforeActivation?.()
                checkCancelled()
                const outcome = await replacePersistentDatabase(...args)
                committedRevision = outcome.revision
                lifecycle.onCommitted?.()
                if (outcome.projection === 'refresh-required') throw new NativeFileJobActivationCommittedError(outcome.revision, new Error('Committed backup projection requires recovery'))
                for (const entry of deferredEntries) {
                    checkCancelled()
                    currentItem = entry.name
                    const data = new Uint8Array(await file.slice(entry.start, entry.start + entry.length).arrayBuffer())
                    await restoreAttachment(entry.name, data)
                }
                await pocketRisuInlays.finish()
                if (attachmentStagingError) throw attachmentStagingError
                upstreamLosses.pocketInlays = pocketRisuInlays.failedIds.length
                if (upstreamLosses.pocketInlays > 0) {
                    counts.skipped += upstreamLosses.pocketInlays
                    if (!warningCodes.includes('upstream-restore-losses')) warningCodes.push('upstream-restore-losses')
                }
                await attachments.activate(checkCancelled)
                currentItem = undefined
                report('activating')
                return outcome
            },
            onPostCommitError: (error) => {
                console.error('Committed local restore follow-up failed', error)
                alertError(language.risuNest.persistentData.followupFailed)
            },
            publishAcceptedRevision: publishCurrentOfficialRevision,
            relaunch: async () => {
                report('restarting-app')
                // Android has no process relauncher, so the WebView reloads instead.
                if (isTauriDesktop) {
                    await relaunch()
                } else {
                    location.search = ''
                }
            },
        })

        return { warningCodes, upstreamLosses }
    } catch (error) {
        if (committedRevision !== null) {
            context.setPartialWritesPossible(true)
            if (error instanceof NativeFileJobActivationCommittedError) throw error
            throw new NativeFileJobActivationCommittedError(committedRevision, error)
        }
        throw error
    } finally {
        await attachments.dispose().catch((error) => {
            console.error('Backup attachment staging cleanup failed', error)
        })
    }
}
