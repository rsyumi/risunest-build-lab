import type { Database } from './database.svelte'

export function importMessageNameSettings(value: Record<string, unknown>): void {
    for (const [upstream, local] of [
        ['groupTemplate', 'messageNameTemplate'],
        ['groupOtherBotRole', 'namedMessageRole'],
    ]) {
        if (value[upstream] !== undefined) value[local] = value[upstream]
        delete value[upstream]
    }
}

export function exportMessageNameSettings(value: Record<string, unknown>): Record<string, unknown> {
    const exported = { ...value }
    for (const [upstream, local] of [
        ['groupTemplate', 'messageNameTemplate'],
        ['groupOtherBotRole', 'namedMessageRole'],
    ]) {
        if (exported[local] !== undefined) exported[upstream] = exported[local]
        delete exported[local]
    }
    return exported
}

/** Converts a detached upstream database before it enters the persistent store. */
export function prepareExternalDatabaseImport(database: Database): Database {
    const excluded = new Set<string>(['§temp'])
    database.characters = database.characters.filter(character => {
        const record = character as unknown as Record<string, unknown>
        if (record.type === 'group' || record.chaId === '§temp') {
            if (typeof record.chaId === 'string') excluded.add(record.chaId)
            return false
        }
        return true
    })
    if (Array.isArray(database.characterOrder)) {
        database.characterOrder = database.characterOrder.filter(entry => {
            if (typeof entry === 'string') return !excluded.has(entry)
            entry.data = entry.data.filter(id => !excluded.has(id))
            return entry.data.length > 0
        })
    }
    for (const loadout of database.loadouts ?? []) {
        loadout.characterIds = loadout.characterIds?.filter(id => !excluded.has(id))
    }
    importMessageNameSettings(database as unknown as Record<string, unknown>)
    for (const preset of database.botPresets ?? []) {
        importMessageNameSettings(preset as unknown as Record<string, unknown>)
    }
    if (database.protectedPresetValues) importMessageNameSettings(database.protectedPresetValues)
    return database
}
