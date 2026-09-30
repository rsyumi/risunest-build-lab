import { runSharedNativeFileOperation } from '../nativeFileJobManager'
import { syntheticNativeFileJobStatus } from '../nativeFileJobs'
import { getServerSyncController } from './serverSyncProduction'
import { getNativeOfficialAccountFlow } from './nativeOfficialAccountFlow'

export function restoreNativeOfficialAccountBackup() {
    getServerSyncController().assertFileOperationAvailable()
    return runSharedNativeFileOperation('import', 'official-account-restore', context =>
        getNativeOfficialAccountFlow().restore({
            signal: context.signal,
            onStatus: context.onStatus,
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
