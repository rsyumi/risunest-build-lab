import { invoke } from '@tauri-apps/api/core'
import { normalizeDatabaseDefaults, type Database } from '../database.svelte'
import { hasNonDefaultBindingData, validateBindingCount, type BindingLocalContent } from './bindingDefaults'

interface NativeBindingContent {
    library: Record<string, unknown>
    characterCount: string
    managedAliasCount: string
    ordinaryPluginValueCount: string
    hypaValueCount: string
    pluginLocalValueCount: string
    opaqueSharedUnitCount: string
    pluginLocalParticipating: boolean
    protectedValues?: Record<string, unknown>
    sharedVariables?: Record<string, unknown>
}

async function readLocalBindingData(): Promise<{ content: BindingLocalContent; pluginLocalParticipating: boolean }> {
    const { pluginLocalParticipating, ...native } = await invoke<NativeBindingContent>('pds_lww_binding_content')
    for (const key of ['characterCount', 'managedAliasCount', 'ordinaryPluginValueCount', 'hypaValueCount', 'pluginLocalValueCount', 'opaqueSharedUnitCount'] as const) {
        validateBindingCount(native[key])
    }
    if (typeof pluginLocalParticipating !== 'boolean') throw new Error('Invalid binding content')
    const factoryLibrary = structuredClone(normalizeDatabaseDefaults({} as Database)) as unknown as Record<string, unknown>
    return { content: { ...native, factoryLibrary, factoryManagedAliasCount: '0' }, pluginLocalParticipating }
}

export async function inspectLocalBindingData(): Promise<BindingLocalContent> {
    return (await readLocalBindingData()).content
}

export async function hasLocalBindingData(): Promise<boolean> {
    return hasNonDefaultBindingData(await inspectLocalBindingData())
}

/**
 * Whether replacing this library loses content: anything beyond what a new device stores,
 * leaving out root settings such as the language the onboarding chooses.
 */
export async function hasLocalLibraryContent(): Promise<boolean> {
    const [content, { prepareDatabaseForBootstrap }, { NEW_DATABASE_SEED }] = await Promise.all([
        inspectLocalBindingData(), import('../databasePreparation'), import('../persistentBootstrap'),
    ])
    const { database } = await prepareDatabaseForBootstrap({ ...NEW_DATABASE_SEED } as Database)
    return hasNonDefaultBindingData({ ...content, factoryLibrary: database as unknown as Record<string, unknown> }, { rootScalars: false })
}

export async function hasLocalSharedBindingData(): Promise<boolean> {
    const { content, pluginLocalParticipating } = await readLocalBindingData()
    // Plugin-local values are shared only while this device takes part in plugin-local sync.
    return hasNonDefaultBindingData(pluginLocalParticipating ? content : { ...content, pluginLocalValueCount: '0' })
}
