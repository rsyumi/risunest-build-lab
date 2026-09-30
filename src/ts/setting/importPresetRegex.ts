import type { Database, customscript } from '../storage/database.svelte'
import { assertPresetNames, capturePresetNames } from '../storage/presetWorkingSetOperations'

export async function importPresetRegex(getDatabase: () => Database, read: () => Promise<customscript[]>, onFailure: () => void): Promise<void> {
    const before = getDatabase()
    const index = before.botPresetsId
    const names = capturePresetNames(before.botPresets)
    try {
        const incoming = await read()
        if (!incoming.length) return
        const current = getDatabase()
        assertPresetNames(current.botPresets, names)
        if (current.botPresetsId !== index) throw new Error('Preset changed')
        current.presetRegex = [...current.presetRegex, ...incoming]
    } catch { onFailure() }
}
