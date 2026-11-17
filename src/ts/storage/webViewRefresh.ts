import { get } from 'svelte/store'
import { doingChat } from '../process/generationState'
import { hasMobileBackgroundTasks } from '../mobileBackgroundTask'
import { getDeviceMarkers } from './deviceMarkers'
import { reserveLibraryFileOperation } from './libraryFileOperation'
import { nativeFileOperation } from './nativeFileJobManager'
import { getPersistentDataRuntime, persistentWorkingSetInputBlocked } from './persistentDataRuntime.svelte'

export async function refreshWebView(): Promise<void> {
    if (get(doingChat) || get(nativeFileOperation) || hasMobileBackgroundTasks() || get(persistentWorkingSetInputBlocked)) {
        throw new Error('webview-refresh-busy')
    }
    const releaseReservation = reserveLibraryFileOperation()
    let fence: { release(): void } | undefined
    try {
        const runtime = getPersistentDataRuntime()
        const token = await runtime.capturePersistentMutationToken('webview-refresh', { publishOfficial: false })
        fence = await runtime.acquireDestructiveReplacementFence(token)
        await getDeviceMarkers().flush()
        if (get(doingChat) || get(nativeFileOperation) || hasMobileBackgroundTasks()) throw new Error('webview-refresh-busy')
        // Keep writes blocked until the new document takes over.
        window.location.reload()
    } catch (error) {
        fence?.release()
        releaseReservation()
        throw error
    }
}
