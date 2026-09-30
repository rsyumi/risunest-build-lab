import { isTauri } from '../platform'
import { getDeviceMarkers, reloadDeviceMarkers } from './deviceMarkers'
import { reloadDeviceSettings } from './deviceSettings'
import { reloadAppUpdateSettings } from '../update/settings'

export async function flushDeviceStateBeforeRestore(): Promise<void> {
    await getDeviceMarkers().flush()
}

export async function refreshDeviceStateAfterRestore(): Promise<void> {
    await reloadDeviceMarkers()
    const settings = reloadDeviceSettings()
    reloadAppUpdateSettings()
    const { applyHubSelection } = await import('../characterCards')
    applyHubSelection(getDeviceMarkers())
    if (isTauri) {
        const { setNativeLogFileEnabled } = await import('../nativeLog')
        await setNativeLogFileEnabled(settings.nativeFileLogEnabled)
    }
}
