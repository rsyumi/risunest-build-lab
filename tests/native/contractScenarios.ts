import { RevisionConflictError, SnapshotReleasedError, type PersistentDataStore, type AssetAlias } from '../../src/ts/storage/persistentDataStore'
import type { Database } from '../../src/ts/storage/database.svelte'

const canonical = (value: unknown): string => JSON.stringify(value, (_key, item) =>
    item && typeof item === 'object' && !Array.isArray(item)
        ? Object.fromEntries(Object.keys(item).sort().map(key => [key, item[key]])) : item)
function equal(actual: unknown, expected: unknown, label: string) {
    if (canonical(actual) !== canonical(expected)) throw new Error(label)
}
const largeText = 'synthetic'.repeat(160_000)
const aliases: AssetAlias[] = ['a', 'b'].map(key => ({ kind: 'asset', key: `assets/${key}.bin`, objectHash: null, size: 0, mime: '', name: key, ext: 'bin' }))

export async function verifyContractReadback(store: PersistentDataStore): Promise<void> {
    const root = await store.readRoot()
    equal(root.value.username, 'contract-committed', 'delta survives reopen')
    equal((root.value as typeof root.value & { syntheticLarge?: string }).syntheticLarge, largeText, 'large transport survives reopen')
    equal((await store.readAssetAliasesByKeys('asset', aliases.map(alias => alias.key))).value, aliases, 'aliases survive reopen')
}

export async function runContractScenarios(store: PersistentDataStore, fixture: Database, reopen: () => Promise<PersistentDataStore>): Promise<void> {
    const imported = await store.replaceFromDatabase(structuredClone(fixture))
    const before = await store.readRoot()
    await store.commitWorkingSetChangeCursor?.(imported.revision)
    const lease = await store.acquireRevision(imported.revision)
    try {
        const committed = await store.commit({ expectedRevision: imported.revision,
            rootMutations: [{ type: 'set', key: 'username', value: 'contract-committed' }], assetAliases: aliases })
        equal(committed.revision, imported.revision + 1, 'delta revision advances once')
        equal(await lease.readRoot(), before, 'lease isolates root from later commit')
        equal((await lease.listAssetAliases({ limit: 1 })).items, [], 'lease isolates aliases')
        let rejected: unknown
        try { await store.commit({ expectedRevision: imported.revision, rootMutations: [{ type: 'set', key: 'username', value: 'rejected' }] }) }
        catch (error) { rejected = error }
        if (!(rejected instanceof RevisionConflictError)) throw new Error('delta conflict type')
        equal([rejected.expectedRevision, rejected.actualRevision], [imported.revision, committed.revision], 'delta conflict fields')
        const first = await store.queryCharacters({ order: 'configured', trash: false, limit: 1 })
        if (!first.nextCursor) throw new Error('character cursor missing')
        const second = await store.queryCharacters({ order: 'configured', trash: false, limit: 1, cursor: first.nextCursor })
        equal([...first.items, ...second.items].map(item => item.id), fixture.characters.slice(0, 2).map(item => item.chaId), 'character pagination')
        const aliasPage = await store.listAssetAliases({ limit: 1 })
        if (!aliasPage.nextCursor) throw new Error('asset cursor missing')
        const aliasTail = await store.listAssetAliases({ limit: 1, cursor: aliasPage.nextCursor })
        equal([...aliasPage.items, ...aliasTail.items], aliases, 'asset pagination')
        const current = await store.acquireRevision(committed.revision)
        try {
            if (current.readWorkingSetChangePage) {
                const changes = await current.readWorkingSetChangePage(imported.revision, null, 1)
                equal(changes.length, 1, 'change page bound')
                const tail = await current.readWorkingSetChangePage(imported.revision, changes[0], 1)
                if (tail.some(item => canonical(item) === canonical(changes[0]))) throw new Error('change cursor repeated')
            }
        } finally { await current.release() }
        await store.commit({ expectedRevision: committed.revision, rootMutations: [{ type: 'set', key: 'syntheticLarge', value: largeText }] })
    } finally { await lease.release() }
    let released: unknown
    try { await lease.readRoot() } catch (error) { released = error }
    if (!(released instanceof SnapshotReleasedError)) throw new Error('released lease still readable')
    await verifyContractReadback(await reopen())
}
