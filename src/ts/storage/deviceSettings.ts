import {
    setRuntimePerformanceProfile,
    type RuntimePerformanceProfile,
} from '../runtimePerformanceProfile'
import {
    RECOVERY_EXCLUSIONS,
    type RecoveryExclusion,
} from './startupExclusions'
import { getDeviceMarkers, type DeviceMarkerStorage } from './deviceMarkers'

export interface RisuNestDeviceSettings {
    schema: 'risunest.device-settings/v1'
    performanceProfile: RuntimePerformanceProfile
    androidKeepAliveDuringGeneration: boolean
    nativeFileLogEnabled: boolean

}

const storageKey = 'risuNestDeviceSettings'
const exclusionsKey = 'risuNestStartupExclusions'

const defaults: RisuNestDeviceSettings = {
    schema: 'risunest.device-settings/v1',
    performanceProfile: 'normal',
    androidKeepAliveDuringGeneration: true,
    nativeFileLogEnabled: true,
}

function snapshot(settings: RisuNestDeviceSettings): RisuNestDeviceSettings {
    return { ...settings }
}

function isExclusionList(value: unknown): value is RecoveryExclusion[] {
    return (
        Array.isArray(value) &&
        value.every((item) =>
            RECOVERY_EXCLUSIONS.includes(item as RecoveryExclusion),
        )
    )
}

function isValidSettings(value: unknown): value is RisuNestDeviceSettings {
    if (!value || typeof value !== 'object') return false
    const settings = value as Record<string, unknown>
    return (
        Object.keys(settings).length === 4 &&
        settings.schema === defaults.schema &&
        (settings.performanceProfile === 'normal' ||
            settings.performanceProfile === 'low-spec') &&
        typeof settings.androidKeepAliveDuringGeneration === 'boolean' &&
        typeof settings.nativeFileLogEnabled === 'boolean'
    )
}

function readSettings(markers: DeviceMarkerStorage): RisuNestDeviceSettings {
    let stored: string | null
    try {
        stored = markers.getItem(storageKey)
    } catch {
        return snapshot(defaults)
    }

    if (!stored) return snapshot(defaults)

    let parsed: unknown
    try {
        parsed = JSON.parse(stored)
    } catch {
        return snapshot(defaults)
    }

    return isValidSettings(parsed)
        ? snapshot(parsed)
        : snapshot(defaults)
}

let settings: RisuNestDeviceSettings | undefined
let storage: DeviceMarkerStorage | undefined
const listeners = new Set<(settings: RisuNestDeviceSettings) => void>()

function initializeSettings(): RisuNestDeviceSettings {
    if (settings) return settings
    return loadDeviceSettings(getDeviceMarkers())
}

/** Reads the stored settings once and keeps them in memory from then on. */
export function loadDeviceSettings(markers: DeviceMarkerStorage): RisuNestDeviceSettings {
    if (settings && storage === markers) return settings
    storage = markers
    settings = readSettings(markers)
    setRuntimePerformanceProfile(settings.performanceProfile)
    return settings
}

export function reloadDeviceSettings(): RisuNestDeviceSettings {
    storage = getDeviceMarkers()
    settings = readSettings(storage)
    setRuntimePerformanceProfile(settings.performanceProfile)
    for (const listener of listeners) listener(snapshot(settings))
    return snapshot(settings)
}

export function getDeviceSettings(): RisuNestDeviceSettings {
    return snapshot(initializeSettings())
}

export function updateDeviceSettings(
    partial: Partial<Omit<RisuNestDeviceSettings, 'schema'>>,
): RisuNestDeviceSettings {
    const { schema: _schema, ...updates } = partial as Partial<RisuNestDeviceSettings>
    const currentSettings = initializeSettings()
    const next = { ...currentSettings, ...updates }
    const previousPerformanceProfile = currentSettings.performanceProfile
    settings = isValidSettings(next) ? next : snapshot(defaults)
    if (settings.performanceProfile !== previousPerformanceProfile) {
        setRuntimePerformanceProfile(settings.performanceProfile)
    }
    try {
        storage?.setItem(storageKey, JSON.stringify(settings))
        // The change is durable only once its transaction commits, so a commit
        // that fails is reported rather than left in memory unnoticed.
        void flushDeviceSettings().catch(reportStoreFailure)
    } catch (error) {
        reportStoreFailure(error)
    }
    for (const listener of listeners) {
        listener(snapshot(settings))
    }
    return snapshot(settings)
}

function reportStoreFailure(error: unknown): void {
    // The settings stay available in memory for this run.
    console.error('A device setting could not be stored', error)
}

/** Resolves once every stored change has committed. */
export async function flushDeviceSettings(): Promise<void> {
    await storage?.flush()
}

export function subscribeDeviceSettings(
    listener: (settings: RisuNestDeviceSettings) => void,
): () => void {
    initializeSettings()
    listeners.add(listener)
    return () => listeners.delete(listener)
}

/** Recovery exclusions remain on this installation when portable settings are restored. */
export function getStartupExclusions(): RecoveryExclusion[] {
    initializeSettings()
    try {
        const value: unknown = JSON.parse(storage?.getItem(exclusionsKey) ?? '[]')
        return isExclusionList(value) ? [...value] : []
    } catch { return [] }
}

export function updateStartupExclusions(exclusions: readonly RecoveryExclusion[]): void {
    initializeSettings()
    if (!isExclusionList(exclusions)) throw new Error('Invalid startup exclusions')
    storage?.setItem(exclusionsKey, JSON.stringify([...new Set(exclusions)]))
    void flushDeviceSettings().catch(reportStoreFailure)
}
