import { v4 } from 'uuid'
import type { Database } from './database.svelte'
import { prepareImportedIdentityState } from './effectiveIdentityState'
import { safeStructuredClone } from '../polyfill'

export function prepareUpstreamImport(database: Database): Database {
    const candidate = safeStructuredClone(database)
    prepareImportedIdentityState(candidate)
    for (const records of [candidate.modules, candidate.loadouts, candidate.customModels]) {
        const used = new Set<string>()
        for (const record of records ?? []) {
            if (typeof record.id !== 'string' || !record.id || used.has(record.id)) record.id = v4()
            used.add(record.id)
        }
    }
    for (const persona of candidate.personas ?? []) {
        if (persona.embeddedModule && !persona.embeddedModule.id) persona.embeddedModule.id = v4()
    }
    return candidate
}
