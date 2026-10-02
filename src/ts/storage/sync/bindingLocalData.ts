import { invoke } from '@tauri-apps/api/core'
import { normalizeDatabaseDefaults, type Database } from '../database.svelte'
import { hasNonDefaultBindingData, validateBindingCount, type BindingLocalContent } from './bindingDefaults'

interface NativeBindingContent {
    library: Record<string, unknown>
    managedAliasCount: string
    ordinaryPluginValueCount: string
    hypaValueCount: string
    pluginLocalValueCount: string
    opaqueSharedUnitCount: string
    protectedValues?: Record<string, unknown>
    sharedVariables?: Record<string, unknown>
}

export async function inspectLocalBindingData(): Promise<BindingLocalContent> {
    const native = await invoke<NativeBindingContent>('pds_lww_binding_content')
    for (const key of ['managedAliasCount', 'ordinaryPluginValueCount', 'hypaValueCount', 'pluginLocalValueCount', 'opaqueSharedUnitCount'] as const) {
        validateBindingCount(native[key])
    }
    const factoryLibrary = structuredClone(normalizeDatabaseDefaults({} as Database)) as unknown as Record<string, unknown>
    return { ...native, factoryLibrary, factoryManagedAliasCount: '0' }
}

export async function hasLocalBindingData(): Promise<boolean> {
    return hasNonDefaultBindingData(await inspectLocalBindingData())
}

export async function hasLocalSharedBindingData(): Promise<boolean> {
    const content = await inspectLocalBindingData()
    return hasNonDefaultBindingData({ ...content, pluginLocalValueCount: '0' })
}
