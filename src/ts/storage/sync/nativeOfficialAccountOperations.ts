import { get } from 'svelte/store'
import { tick } from 'svelte'
import { doingChat } from '../../process/generationState'
import { nativeFileOperationOutcome, runSharedNativeFileOperation, waitForActiveNativeFileOperation } from '../nativeFileJobManager'
import { syntheticNativeFileJobStatus } from '../nativeFileJobs'
import { getServerSyncController } from './serverSyncProduction'
import { getNativeOfficialAccountFlow, type NativeOfficialAssetResumeOptions } from './nativeOfficialAccountFlow'
import { isBackgroundExpiryReason } from '../../iosNative'

export function inspectNativeOfficialAccountAssets() {
    return getNativeOfficialAccountFlow().inspectAssets()
}

export function resumeNativeOfficialAccountAssets(options: Pick<NativeOfficialAssetResumeOptions, 'operationId' | 'currentLibrary'> = {}, userInitiated = true) {
    if (userInitiated) getServerSyncController().assertFileOperationAvailable()
    return runSharedNativeFileOperation('import', `official-account-assets:${options.operationId ?? 'current-library'}`, async context => {
        context.setPartialWritesPossible(true)
        await getNativeOfficialAccountFlow().resumeAssets({ ...options, signal: context.signal,
            onStatus: context.onStatus, onBlockingChange: context.setBlocking })
        return { completed: true }
    }, { presentation: 'dialog', format: 'library-backup', userInitiated,
        beforeSettlement: async signal => {
            if (signal.aborted && !isBackgroundExpiryReason(signal.reason) && options.operationId) await getNativeOfficialAccountFlow().stopAssets(options.operationId)
        },
    })
}

export function retryNativeOfficialAccountAssetsFromOutcome() {
    const operationId = get(nativeFileOperationOutcome)?.status?.officialAssets?.operationId
    if (!operationId) return Promise.reject(new Error('Official account continuation is unavailable'))
    return resumeNativeOfficialAccountAssets({ operationId })
}

export async function recoverNativeOfficialAccountAssets() {
    await tick()
    const initial = await inspectNativeOfficialAccountAssets()
    if (initial.kind !== 'interrupted') return
    while (true) {
        await waitForActiveNativeFileOperation()
        const outcome = get(nativeFileOperationOutcome)
        if (outcome) {
            let stop: (() => void) | undefined
            await new Promise<void>(resolve => { stop = nativeFileOperationOutcome.subscribe(value => { if (value !== outcome) resolve() }) })
            stop?.(); continue
        }
        if (get(doingChat)) {
            let stop: (() => void) | undefined
            await new Promise<void>(resolve => { stop = doingChat.subscribe(value => { if (!value) resolve() }) })
            stop?.(); continue
        }
        const pending = await inspectNativeOfficialAccountAssets()
        if (pending.kind !== 'interrupted' || pending.operationId !== initial.operationId) return
        try { await resumeNativeOfficialAccountAssets({ operationId: pending.operationId }, false); return }
        catch (error) {
            if (error instanceof Error && error.name === 'NativeFileOperationBusyError') continue
            if (get(nativeFileOperationOutcome)?.status?.officialAssets?.operationId === pending.operationId) return
            throw error
        }
    }
}

export function restoreNativeOfficialAccountBackup() {
    getServerSyncController().assertFileOperationAvailable()
    return runSharedNativeFileOperation('import', 'official-account-restore', context =>
        getNativeOfficialAccountFlow().restore({
            signal: context.signal,
            onStatus: status => {
                if (status.activationRevision !== undefined || status.officialAssets) context.setPartialWritesPossible(true)
                context.onStatus(status)
            },
            onBlockingChange: context.setBlocking,
        }),
    { presentation: 'dialog', format: 'library-backup' })
}

export function publishNativeOfficialAccountBackup() {
    getServerSyncController().assertFileOperationAvailable()
    return runSharedNativeFileOperation('export', 'official-account-publish', async context => {
        await getNativeOfficialAccountFlow().publish(context.signal, (completed, total) => {
            context.onStatus(syntheticNativeFileJobStatus(
                { kind: 'official-publication-upload' }, 'publishing-destination',
                { stageCompleted: completed, stageTotal: total, stageUnit: 'items' },
            ))
        }, context.onStatus)
        return { published: true }
    }, { presentation: 'dialog', format: 'library-backup' })
}
