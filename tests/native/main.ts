import { runContractScenarios, verifyContractReadback } from './contractScenarios'
import { endsByNativeExit, runNativeExitPhase, verifySessionEndReadback } from './lifecycleScenarios'
import { invoke } from '@tauri-apps/api/core'
import { SqlitePersistentDataStore } from '../../src/ts/storage/sqlitePersistentDataStore'
import { RevisionConflictError } from '../../src/ts/storage/persistentDataStore'
import { stableSelectionRoot } from '../../src/ts/storage/persistentSelectionBoundary'
import type { Database } from '../../src/ts/storage/database.svelte'

const fixture = {
    username: '', botPresets: [], botPresetsId: 0, pluginCustomStorage: {},
    syntheticUnknown: { zero: 0, disabled: false, empty: '', nested: [null, 0, false] },
    characters: ['a', 'b'].map(id => ({ type: 'character', chaId: `char-${id}`, name: `Synthetic ${id}`, chatPage: 0,
        chats: [0, 1].map(index => ({ id: `chat-${id}-${index}`, name: '', note: '', localLore: [],
            message: [{ role: 'user', data: `first-${id}-${index}`, chatId: `user-${id}-${index}` },
                { role: 'char', data: '', chatId: `reply-${id}-${index}`, custom: { zero: 0, enabled: false } }],
        })),
    })),
} as unknown as Database
const pluginValues = [{ owner: 'synthetic-plugin', key: 'fixture', value: { zero: 0, enabled: false, text: '' } }]
const canonical = (value: unknown): string => JSON.stringify(value, (_key, item) =>
    item && typeof item === 'object' && !Array.isArray(item)
        ? Object.fromEntries(Object.keys(item).sort().map(key => [key, item[key]])) : item)
const equal = (actual: unknown, expected: unknown, name: string) => {
    if (canonical(actual) !== canonical(expected)) throw new Error(name)
}
const blockedRequests: string[] = []
window.addEventListener('securitypolicyviolation', event => blockedRequests.push(event.violatedDirective))
const phase = await invoke<{ runId: string; phase: string }>('boundary_phase')
const cases: string[] = []
try {
    const store = new SqlitePersistentDataStore()
    await store.open()
    if (endsByNativeExit(phase.phase)) {
        // The app's own native exit ends these phases, after they record their report.
        await runNativeExitPhase(phase, store, fixture)
        await new Promise<never>(() => {})
    }
    if (phase.phase === 'contract') {
        await runContractScenarios(store, fixture, async () => {
            const reopened = new SqlitePersistentDataStore()
            await reopened.open()
            return reopened
        })
        cases.push('delta-lease-pagination-large-commit')
    } else if (phase.phase === 'read-contract') {
        await verifyContractReadback(store)
        cases.push('contract-restart-without-reseed')
    } else if (phase.phase === 'read-session-end') {
        await verifySessionEndReadback(store)
        cases.push('saved-before-exit')
    } else {
        if (phase.phase === 'write' || phase.phase === 'abort') {
            equal(store.lastOpenResult?.revision, 0, 'fresh store')
            const committed = await store.replaceFromDatabase(fixture, undefined, [], pluginValues)
            equal(committed.revision, 1, 'acknowledged revision')
            cases.push('commit')
        } else if (phase.phase !== 'read' && phase.phase !== 'read-abort') throw new Error('unknown phase')
        const verify = async () => {
            const root = stableSelectionRoot(fixture)
            Reflect.deleteProperty(root, 'pluginCustomStorage')
            equal(await store.readRoot(), { revision: 1, value: root }, 'root readback')
            for (const character of fixture.characters) for (const chat of character.chats) {
                equal(await store.readConversation(character.chaId, chat.id!), { revision: 1, value: chat }, 'ordered conversation readback')
            }
            equal(await store.readPluginStorage('synthetic-plugin', 'fixture'), { revision: 1, value: pluginValues[0].value }, 'plugin owner readback')
        }
        await verify()
        cases.push('readback')
        if (phase.phase === 'write') {
            let conflict = false
            try { await store.replaceFromDatabase({ ...fixture, username: 'rejected' }, 0) }
            catch (error) { conflict = error instanceof RevisionConflictError }
            if (!conflict) throw new Error('stale revision accepted')
            await verify()
            cases.push('stale-rejected')
        } else if (phase.phase === 'abort') {
            const { stagingId } = await invoke<{ stagingId: string }>('pds_replace_begin')
            await invoke('pds_replace_put_root', { stagingId, root: { username: 'aborted' } })
            await invoke('pds_replace_abort', { stagingId })
            let released = false
            try { await invoke('pds_replace_put_root', { stagingId, root: { username: 'aborted' } }) } catch { released = true }
            if (!released) throw new Error('aborted staging still live')
            await verify()
            cases.push('abort-released')
        } else {
            equal(store.lastOpenResult?.revision, 1, 'restart revision')
            cases.push('restart-without-reseed')
        }
    }
    if (blockedRequests.length) throw new Error(`Unexpected blocked resources: ${blockedRequests.join(', ')}`)
    await invoke('boundary_finish', { report: { ...phase, cases, success: true } })
} catch (error) {
    await invoke('boundary_finish', { report: { ...phase, cases, success: false, error: String(error) } })
}
