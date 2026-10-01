import '../../src/ts/polyfill'
import { invoke } from '@tauri-apps/api/core'
import { join } from '@tauri-apps/api/path'
import { nativeDataPath } from '../../src/ts/storage/nativePaths'
import { exists, mkdir, readTextFile, remove } from '@tauri-apps/plugin-fs'
import { SandboxHost } from '../../src/ts/plugins/apiV3/factory'
import { createPluginStorageStore } from '../../src/ts/plugins/pluginStorageStore'
import { SqlitePersistentDataStore } from '../../src/ts/storage/sqlitePersistentDataStore'
import type { Database } from '../../src/ts/storage/database.svelte'
import fixture from '../../src-tauri/fixtures/persistent-fixture.json'

const configuredIdentifier = 'io.github.rsyumi.risunest'
const identifier = 'io.github.rsyumi.risunest.pluginreview'
const title = 'RisuNest synthetic plugin review'
const owner = 'synthetic-plugin-page-owner'
const pause = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))
function check(condition: unknown, label: string): asserts condition {
    if (!condition) throw new Error(label)
}

async function isolation(directory: string) {
    const positive = await join(directory, 'trusted-positive')
    await mkdir(positive, { recursive: true })
    check(await exists(positive), 'Trusted native filesystem positive control failed')
    const raw = await join(directory, 'guest-raw')
    const injected = await join(directory, 'guest-injected')
    const container = document.createElement('div')
    document.body.append(container)
    let resolveReport!: (value: Record<string, boolean>) => void
    const reported = new Promise<Record<string, boolean>>(resolve => { resolveReport = resolve })
    const host = new SandboxHost({
        _getPropertiesForInitialization: () => ({ list: [] }), _getAliases: () => ({}),
        report: (value: Record<string, boolean>) => resolveReport(value),
    })
    try {
        host.run(container, `
            const internals = window.__TAURI_INTERNALS__;
            const result = {
                opaque: location.origin === 'null',
                hasInvoke: typeof internals?.invoke === 'function',
                hasCredentialedTransport: typeof internals?.postMessage === 'function',
                hasGlobalKey: typeof window.__TAURI_INVOKE_KEY__ === 'string',
                rawAvailable: typeof window.ipc?.postMessage === 'function',
                rawAttempted: false, parentDomBlocked: false,
            };
            try { void parent.document; } catch { result.parentDomBlocked = true; }
            const packet = {cmd:'plugin:fs|mkdir',callback:900000001,error:900000002,
                payload:{path:${JSON.stringify(raw)},options:{recursive:true}},options:{headers:{},customProtocolIpcBlocked:true}};
            if (result.rawAvailable) {
                for (const key of [undefined, 'synthetic-invalid-key']) {
                    try { window.ipc.postMessage(JSON.stringify({...packet,__TAURI_INVOKE_KEY__:key})); result.rawAttempted=true; } catch {}
                }
            }
            if (result.hasInvoke) void internals.invoke('plugin:fs|mkdir',
                {path:${JSON.stringify(injected)},options:{recursive:true}}).catch(()=>{});
            await risuai.report(result);
        `, 'synthetic-native-boundary')
        const guest = await Promise.race([reported, pause(10000).then(() => { throw new Error('Guest report timeout') })])
        await pause(1000)
        const rawMutation = await exists(raw)
        const injectedMutation = await exists(injected)
        const passed = guest.opaque && guest.parentDomBlocked && !guest.hasInvoke
            && !guest.hasCredentialedTransport && !guest.hasGlobalKey
            && (!guest.rawAvailable || guest.rawAttempted) && !rawMutation && !injectedMutation
        return { passed, ...guest, rawMutation, injectedMutation, trustedNativeFilesystem: true }
    } finally { host.terminate(); container.remove() }
}

async function paging() {
    const store = new SqlitePersistentDataStore()
    await store.open()
    const rows = Array.from({ length: 2000 }, (_, index) => ({
        owner, key: `key-${2000 - index}`, value: { index, text: `synthetic-${index}` },
    }))
    const large = 'x'.repeat(1024 * 1024 + 32)
    const database = { ...structuredClone(fixture), characters: [], plugins: [], pluginCustomStorage: {} } as unknown as Database
    await store.replaceFromDatabase(database, undefined, [], [
        ...rows, { owner: 'foreign-owner', key: rows[0].key, value: 'foreign' },
        { owner: 'large-owner', key: 'large', value: large },
        { owner: 'large-owner', key: 'tail', value: 'tail' },
    ])
    const storage = createPluginStorageStore({ store, getStorageAuthorityEpoch: () => 0,
        assertPersistentMutationAllowed() {}, async mutate() { throw new Error('Snapshot must be read-only') } })
    const counts: Record<string, number> = {}
    const scope = globalThis as typeof globalThis & { __pluginReviewCountInvoke?: (command: string) => void }
    scope.__pluginReviewCountInvoke = command => { counts[command] = (counts[command] ?? 0) + 1 }
    const start = performance.now()
    let snapshot: Record<string, unknown>
    try { snapshot = await storage.forOwner(owner).snapshot() }
    finally { delete scope.__pluginReviewCountInvoke }
    const snapshotMs = performance.now() - start
    const expected = Object.fromEntries(rows.map(row => [row.key, row.value]))
    check(JSON.stringify(snapshot) === JSON.stringify(expected), 'Owner snapshot differs in values, ownership, or insertion order')
    check(counts.pds_read_plugin_storage_page === 8, '2000-key snapshot did not use eight native value pages')
    check(!counts.pds_read_plugin_storage && !counts.pds_query_plugin_storage, 'Snapshot used per-key/all-owner catalog reads')
    const revision = (await store.readRoot()).revision
    const lease = await store.acquireRevision(revision)
    let largePages
    const largeStart = performance.now()
    try {
        const first = await lease.readPluginStorageValues({ owner: 'large-owner' })
        check(first.items.length === 1 && first.items[0].value === large && first.nextCursor, 'Oversized first row was not isolated')
        const second = await lease.readPluginStorageValues({ owner: 'large-owner', afterKey: first.nextCursor })
        check(second.items.length === 1 && second.items[0].value === 'tail' && !second.nextCursor, 'Tail after oversized row differs')
        check(first.revision === revision && second.revision === revision, 'Paged revision changed')
        largePages = 2
    } finally { await lease.release() }
    return { keys: rows.length, pageInvokes: counts.pds_read_plugin_storage_page, nativeInvokes: counts,
        snapshotMs, largePages, largeValueBytes: large.length, largeValueMs: performance.now() - largeStart,
        exactOutput: true, ownerIsolated: true }
}

let running = false
Object.assign(window, { __pluginReview: {
    marker: 'synthetic-plugin-review-v1',
    async run(token: string) {
        check(!running && /^[a-f0-9]{32}$/.test(token), 'Invalid synthetic ownership token')
        running = true
        check(document.title === title && await invoke('plugin:app|identifier') === configuredIdentifier, 'Wrong synthetic native profile')
        const root = await nativeDataPath()
        check(new RegExp('/' + identifier.replaceAll('.', '\\.') + '/files/?$').test(root), 'Native root is not the isolated plugin profile')
        check((await readTextFile(await join(root, 'risunest-plugin-review-owner'))).trim() === token, 'Synthetic ownership marker mismatch')
        const directory = await join(root, `plugin-review-${token}`)
        check(!(await exists(directory)), 'Synthetic probe directory already exists')
        await mkdir(directory)
        try {
            const version = await invoke<string>('plugin:app|version')
            check(typeof version === 'string' && version.length > 0, 'Trusted main-frame IPC failed')
            const boundary = await isolation(directory)
            const values = await paging()
            return { passed: boundary.passed, trustedMainFrameIpc: true, boundary, values }
        } finally { await remove(directory, { recursive: true }) }
    },
} })
