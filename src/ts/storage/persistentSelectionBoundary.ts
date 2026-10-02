import type { Database } from './database.svelte'
import type { PersistentRoot } from './persistentDataStore'

export function stableSelectionRoot(database: Database): PersistentRoot {
    const { characters: _characters, botPresets, ...root } = database
    const result: PersistentRoot = { ...root }
    if (Object.hasOwn(database, 'botPresetsId')) result.botPresetsId = botPresets?.[database.botPresetsId]?.['id'] as string ?? ''
    if (Object.hasOwn(database, 'selectedPersona')) result.selectedPersona = database.personas?.[database.selectedPersona]?.id ?? ''
    return result
}

export function projectSelectionIndexes(database: Database): Database {
    const root = database as unknown as PersistentRoot
    if (typeof root.botPresetsId === 'string') database.botPresetsId = Math.max(0, database.botPresets.findIndex((value) => value?.['id'] === root.botPresetsId))
    if (typeof root.selectedPersona === 'string') database.selectedPersona = Math.max(0, database.personas?.findIndex((value) => value.id === root.selectedPersona) ?? 0)
    return database
}
