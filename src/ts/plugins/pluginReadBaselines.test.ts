import { describe, expect, it } from 'vitest'
import { attachPluginReadProvenance, collectPluginReadProvenance, diffPluginReadBaseline, PluginReadBaselines, rebasePluginFieldIntents, type PluginSnapshotChunk } from './pluginReadBaselines'
import { createPluginSnapshotStream } from './pluginSnapshotStream'
import { pluginUnitIntents } from './pluginUnitIntents'

/** Every string reachable from `root` through own properties, Map and Set entries; typed array contents are not strings. */
function reachableStrings(root: unknown): string[] {
    const strings: string[] = []
    const seen = new Set<object>()
    const pending: unknown[] = [root]
    while (pending.length) {
        const value = pending.pop()
        if (typeof value === 'string') {
            strings.push(value)
            continue
        }
        if (!value || typeof value !== 'object' || seen.has(value)) continue
        seen.add(value)
        if (ArrayBuffer.isView(value)) continue
        if (value instanceof Map) {
            for (const [key, entry] of value) pending.push(key, entry)
            continue
        }
        if (value instanceof Set) {
            for (const entry of value) pending.push(entry)
            continue
        }
        for (const key of Object.getOwnPropertyNames(value)) {
            strings.push(key)
            pending.push(Object.getOwnPropertyDescriptor(value, key)!.value)
        }
    }
    return strings
}

const retainedLength = (root: unknown) => reachableStrings(root).reduce((total, value) => total + value.length, 0)

function fixture() {
    let authority = 1
    const registry = new PluginReadBaselines('synthetic-plugin', () => authority)
    const character = { chaId: 'character-a', name: 'Before', desc: 'Retained', chats: [{ id: 'chat-a', name: 'Chat', note: 'Before', message: [{ role: 'user', data: 'Synthetic' }] }] }
    return { registry, character, replaceAuthority: () => { authority++; registry.expire() } }
}

describe('plugin immutable read baselines', () => {
    it.each(['database', 'character', 'conversation'] as const)('publishes the same %s provenance and field intents from incremental chunks', kind => {
        const { character } = fixture()
        const characters = [character, { ...structuredClone(character), name: 'Duplicate target' }]
        const presets = [{ id: 'preset', mainPrompt: 'Synthetic preset' }, { mainPrompt: 'Missing ID' }]
        const root = { username: 'Synthetic', characters, botPresets: presets, pluginCustomStorage: JSON.parse('{"__proto__":{"keep":true},"value":0}') }
        const legacy = new PluginReadBaselines('owner', () => 1)
        const incremental = new PluginReadBaselines('owner', () => 1)
        const target = kind === 'database' ? 'database' : kind === 'character' ? character.chaId : JSON.stringify([character.chaId, character.chats[0].id])
        const select = (value: typeof root) => kind === 'database' ? value : kind === 'character' ? value.characters[0] : value.characters[0].chats[0]
        if (kind !== 'database') root.characters = [character]
        const expected = structuredClone(root)
        legacy.track(select(expected), kind, target, 2)
        const builder = incremental.beginSnapshot(kind, target)
        const chunks: PluginSnapshotChunk[] = [
            { type: 'set', key: 'username', value: root.username },
            { type: 'arrayStart', key: 'characters' },
        ]
        for (const item of root.characters) {
            chunks.push({ type: 'characterStart', key: 'characters', value: item })
            for (const chat of item.chats) {
                chunks.push({ type: 'conversationStart', key: 'characters', value: chat })
                chunks.push(...chat.message.map(value => ({ type: 'message' as const, key: 'characters' as const, value })))
            }
        }
        if (kind === 'database') {
            chunks.push({ type: 'arrayStart', key: 'botPresets' })
            chunks.push(...presets.map(value => ({ type: 'arrayPush' as const, key: 'botPresets', value })))
            chunks.push({ type: 'recordStart', key: 'pluginCustomStorage' })
            for (const [entryKey, value] of Object.entries(root.pluginCustomStorage)) chunks.push({ type: 'recordSet', key: 'pluginCustomStorage', entryKey, value })
        }
        for (const chunk of chunks) builder.add(chunk)
        expect(incremental.hasRead(kind, target)).toBe(false)
        const entries = builder.finish()
        const tokenShape = (entries: ReturnType<typeof collectPluginReadProvenance>) => {
            const tokens = new Map<string, number>()
            return entries.map(entry => {
                if (!tokens.has(entry.token)) tokens.set(entry.token, tokens.size)
                return { path: entry.path, token: tokens.get(entry.token) }
            })
        }
        expect(tokenShape(entries)).toEqual(tokenShape(collectPluginReadProvenance(expected)))
        attachPluginReadProvenance(root, entries)
        root.characters[0].chats[0].note = 'Changed note'
        root.characters[0].chats[0].message[0].data = 'Changed message'
        expected.characters[0].chats[0].note = 'Changed note'
        expected.characters[0].chats[0].message[0].data = 'Changed message'
        expect(incremental.intent(select(root), kind, target, {})).toEqual(legacy.intent(select(expected), kind, target, {}))
    })

    it('batches by count and cost, preserves oversized chunks, and does not publish a cancelled read', async () => {
        const { registry, character } = fixture()
        registry.track(structuredClone(character), 'character', character.chaId, 1)
        const chunks: PluginSnapshotChunk[] = [
            { type: 'arrayStart', key: 'characters' },
            { type: 'characterStart', key: 'characters', value: { ...character, name: 'New read', chats: [] } },
            { type: 'conversationStart', key: 'characters', value: { id: 'chat-a' } },
            ...['small', 'x'.repeat(2000), 'last'].map(data => ({ type: 'message' as const, key: 'characters' as const, value: { data } })),
        ]
        const source = async function* () { yield* chunks }
        let released = 0
        const stream = createPluginSnapshotStream(source(), registry.beginSnapshot('character', character.chaId), async () => { released++ }, { maxChunks: 2, maxBytes: 1024 })
        const frames = []
        for await (const frame of stream) frames.push(frame)
        expect(frames.flatMap(frame => frame.type === 'batch' ? frame.chunks : [frame]).slice(0, -1)).toEqual(chunks)
        expect(frames.length).toBeLessThan(chunks.length + 1)
        expect(frames.filter(frame => frame.type === 'batch').every(frame => frame.chunks.length <= 2)).toBe(true)
        expect(frames.some(frame => frame.type === 'message')).toBe(true)
        expect(released).toBe(1)
        const cancelled = createPluginSnapshotStream(source(), registry.beginSnapshot('character', character.chaId), async () => { released++ }, { maxChunks: 1 })
        const reader = cancelled.getReader()
        await reader.read()
        await reader.cancel()
        expect(released).toBe(2)
        const same = { ...character, name: 'New read', chats: [{ id: 'chat-a', message: [{ data: 'small' }, { data: 'x'.repeat(2000) }, { data: 'last' }] }] }
        expect(registry.intent(same, 'character', character.chaId, {})).toEqual([])
    })

    it('composes delayed partial snapshots with the newest nested conversation read', () => {
        const { registry, character } = fixture()
        const root = registry.track({ username: 'Old', characters: [structuredClone(character)], personas: [{ id: 'p', name: 'Keep' }] }, 'database', 'database', 1)
        const delayed = registry.beginSnapshot('database', 'database')
        delayed.add({ type: 'set', key: 'username', value: 'Delayed completion' })
        const quicker = registry.beginSnapshot('database', 'database')
        quicker.add({ type: 'set', key: 'username', value: 'Earlier completion' })
        quicker.finish()
        delayed.finish()
        const conversation = registry.beginSnapshot('conversation', JSON.stringify([character.chaId, 'chat-a']))
        conversation.add({ type: 'arrayStart', key: 'characters' })
        conversation.add({ type: 'characterStart', key: 'characters', value: character })
        conversation.add({ type: 'conversationStart', key: 'characters', value: { ...character.chats[0], note: 'Remote' } })
        for (const value of character.chats[0].message) conversation.add({ type: 'message', key: 'characters', value })
        conversation.finish()
        root.username = 'Delayed completion'
        root.characters[0].chats[0].note = 'Remote'
        expect(registry.intent(root, 'database', 'database', {})).toEqual([])
        root.personas[0].name = 'Edited'
        expect(registry.intent(root, 'database', 'database', {})).toEqual([{ path: ['personas', 'p', 'name'], type: 'set', value: 'Edited' }])
    })

    it.each(['source', 'release', 'abort', 'authority'] as const)('does not publish a provisional baseline after %s failure', async failure => {
        let authority = 1
        const registry = new PluginReadBaselines('owner', () => authority)
        registry.track({ username: 'Completed' }, 'database', 'database', 1)
        const controller = new AbortController()
        const builder = registry.beginSnapshot('database', 'database')
        const source = async function* (): AsyncGenerator<PluginSnapshotChunk> {
            yield { type: 'set', key: 'username', value: 'Incomplete' }
            if (failure === 'source') throw new Error('source failed')
        }
        let releases = 0
        const stream = createPluginSnapshotStream(source(), builder, async () => {
            releases++
            if (failure === 'release') throw new Error('release failed')
            if (failure === 'abort') controller.abort()
            if (failure === 'authority') {
                authority++
                registry.expire()
                registry.track({ username: 'Completed' }, 'database', 'database', 2)
            }
        }, { signal: controller.signal })
        await expect((async () => { for await (const _frame of stream) { /* Drain the actual stream. */ } })()).rejects.toThrow()
        expect(releases).toBe(1)
        expect(registry.intent({ username: 'Completed' }, 'database', 'database', {})).toEqual([])
    })

    it('uses the exact read and preserves untouched fields changed after that read', () => {
        const { registry, character } = fixture()
        const read = registry.track(structuredClone(character), 'character', character.chaId, 3)
        read.name = 'Edited'
        const intents = registry.intent(read, 'character', character.chaId, {})
        expect(intents).toEqual([{ path: ['name'], type: 'set', value: 'Edited' }])
        expect(rebasePluginFieldIntents({ ...character, desc: 'Remote' }, intents)).toEqual({ ...character, name: 'Edited', desc: 'Remote' })
    })

    it('maps every read of a target to its newest completed read, including delayed writes', () => {
        const { registry, character } = fixture()
        const first = registry.track(structuredClone(character), 'character', character.chaId, 1)
        const second = registry.track({ ...structuredClone(character), name: 'Second' }, 'character', character.chaId, 2)
        const third = registry.track(structuredClone(character), 'character', character.chaId, 3)
        expect(new Set([first, second, third].map(value => collectPluginReadProvenance(value)[0].token)).size).toBe(1)
        second.desc = 'Second edit'
        first.name = 'Delayed edit'
        // The newest read has the original name, so a write from the second read restores its own name.
        expect(registry.intent(second, 'character', character.chaId, {})).toEqual([
            { path: ['desc'], type: 'set', value: 'Second edit' },
            { path: ['name'], type: 'set', value: 'Second' },
        ])
        expect(registry.intent(first, 'character', character.chaId, {})).toEqual([{ path: ['name'], type: 'set', value: 'Delayed edit' }])
        first.name = 'Another delayed edit'
        expect(registry.intent(first, 'character', character.chaId, {})).toEqual([{ path: ['name'], type: 'set', value: 'Another delayed edit' }])
    })

    it.each(['character', 'database'] as const)('tags nested chat returned through %s reads', kind => {
        const { registry, character } = fixture()
        const read = kind === 'character'
            ? registry.track(character, kind, character.chaId, 2)
            : registry.track({ characters: [character] }, kind, 'database', 2).characters[0]
        const nested = read.chats[0]
        nested.note = 'Edited'
        expect(registry.intent(nested, 'conversation', JSON.stringify([character.chaId, nested.id]), {})).toEqual([{ path: ['note'], type: 'set', value: 'Edited' }])
    })

    it('diffs nested reads moved into a newer parent against the newest read of their target', () => {
        const { registry, character } = fixture()
        const old = registry.track(structuredClone(character), 'character', character.chaId, 1)
        old.name = 'Edited'
        const latest = { ...structuredClone(character), desc: 'Remote' }
        const parent = registry.track({ characters: [latest] }, 'database', 'database', 2)
        parent.characters[0] = old
        // The database read is the newest read of the character, so the older copy restores its description.
        expect(registry.intent(parent, 'database', 'database', {})).toEqual([
            { path: ['characters', character.chaId, 'desc'], type: 'set', value: 'Retained' },
            { path: ['characters', character.chaId, 'name'], type: 'set', value: 'Edited' },
        ])
        const fresh = new PluginReadBaselines('other', () => 1)
        const child = fresh.track(structuredClone(character), 'character', character.chaId, 1)
        child.name = 'Edited'
        expect(fresh.intent({ characters: [child] }, 'database', 'database', { characters: [latest] })).toEqual([{ path: ['characters', character.chaId, 'name'], type: 'set', value: 'Edited' }])
    })

    it('retains nested copy history even when no database getter was called', () => {
        const { registry, character, replaceAuthority } = fixture()
        const child = registry.track(structuredClone(character), 'character', character.chaId, 1)
        const copy = JSON.parse(JSON.stringify(child))
        copy.name = 'Edited'
        const admission = { characters: [{ ...character, desc: 'Remote' }] }
        expect(registry.intent({ characters: [copy] }, 'database', 'database', admission)).toEqual([{ path: ['characters', character.chaId, 'name'], type: 'set', value: 'Edited' }])
        replaceAuthority()
        expect(() => registry.intent({ characters: [copy] }, 'database', 'database', admission)).toThrow('stale read baseline')
    })

    it('carries provenance separately through RPC without changing product keys', () => {
        const { registry, character } = fixture()
        registry.track(character, 'character', character.chaId, 2)
        const sidecar = collectPluginReadProvenance(character)
        const transported = structuredClone(character)
        attachPluginReadProvenance(transported, sidecar)
        transported.chats[0].note = 'Edited'
        expect(Object.keys(transported)).toEqual(['chaId', 'name', 'desc', 'chats'])
        expect(JSON.stringify(transported)).not.toContain(sidecar[0].token)
        expect(registry.intent(transported.chats[0], 'conversation', JSON.stringify([character.chaId, 'chat-a']), {})).toEqual([{ path: ['note'], type: 'set', value: 'Edited' }])
    })

    it('accepts a single-baseline JSON copy', () => {
        const { registry, character } = fixture()
        const copy = JSON.parse(JSON.stringify(registry.track(character, 'character', character.chaId, 2)))
        copy.name = 'Copy edit'
        expect(registry.intent(copy, 'character', character.chaId, {})).toEqual([{ path: ['name'], type: 'set', value: 'Copy edit' }])
    })

    it('diffs an untagged copy against the newest compatible read', () => {
        const { registry, character } = fixture()
        registry.track(structuredClone(character), 'character', character.chaId, 1)
        registry.track(structuredClone(character), 'character', character.chaId, 2)
        const copy = { ...structuredClone(character), desc: 'Copy edit' }
        expect(registry.intent(copy, 'character', character.chaId, {})).toEqual([{ path: ['desc'], type: 'set', value: 'Copy edit' }])
        registry.track({ ...structuredClone(character), name: 'Newest' }, 'character', character.chaId, 3)
        expect(registry.intent(copy, 'character', character.chaId, {})).toEqual([
            { path: ['desc'], type: 'set', value: 'Copy edit' },
            { path: ['name'], type: 'set', value: 'Before' },
        ])
    })

    it('rejects expired exact objects and untagged copies after authority replacement', () => {
        const { registry, character, replaceAuthority } = fixture()
        const read = registry.track(character, 'character', character.chaId, 1)
        const copy = JSON.parse(JSON.stringify(read))
        replaceAuthority()
        expect(() => registry.intent(read, 'character', character.chaId, {})).toThrow('stale')
        expect(() => registry.intent(copy, 'character', character.chaId, {})).toThrow('stale')
        expect(() => registry.intent(copy.chats[0], 'conversation', JSON.stringify([character.chaId, 'chat-a']), {})).toThrow('stale')
    })

    it('rejects another owner, mismatched target and writes from a closed execution', () => {
        const { registry, character } = fixture()
        const read = registry.track(character, 'character', character.chaId, 1)
        const another = new PluginReadBaselines('other-plugin', () => 1)
        expect(() => another.intent(read, 'character', character.chaId, {})).toThrow('stale')
        expect(() => registry.intent(read, 'character', 'another-character', {})).toThrow('target')
        registry.close()
        expect(() => registry.intent(read, 'character', character.chaId, {})).toThrow('closed')
        expect(() => registry.track({}, 'database', 'database', 1)).toThrow('closed')
    })

    it('allows a wholly new execution to use a no-read admission baseline', () => {
        const { character } = fixture()
        const fresh = new PluginReadBaselines('synthetic-plugin', () => 2)
        const copy = JSON.parse(JSON.stringify(character))
        copy.name = 'New execution edit'
        expect(fresh.intent(copy, 'character', character.chaId, character)).toEqual([{ path: ['name'], type: 'set', value: 'New execution edit' }])
    })

    it('retains one baseline per target across repeated reads', () => {
        const { registry, character } = fixture()
        const oldest = registry.track(structuredClone(character), 'character', character.chaId, 1)
        for (let revision = 2; revision < 1002; revision++) registry.track(structuredClone(character), 'character', character.chaId, revision)
        for (let revision = 2; revision < 1002; revision++) registry.track({ characters: [structuredClone(character)] }, 'database', 'database', revision)
        // The character, its conversation and the database.
        expect(registry.retainedBaselineCount).toBe(3)
        const retained = retainedLength(registry)
        registry.track(structuredClone(character), 'character', character.chaId, 1002)
        expect(retainedLength(registry)).toBe(retained)
        oldest.desc = 'Old read edit'
        expect(registry.intent(oldest, 'character', character.chaId, {})).toEqual([{ path: ['desc'], type: 'set', value: 'Old read edit' }])
    })

    it('keeps none of the content it read and still diffs a 50,000-message conversation', () => {
        const registry = new PluginReadBaselines('synthetic-plugin', () => 1)
        const messages = (prefix: string, count: number) => Array.from({ length: count }, (_, index) => ({
            role: index % 2 ? 'char' : 'user', data: `retained-marker-${prefix}-${index}`, chatId: `retained-marker-id-${prefix}-${index}`,
        }))
        const target = JSON.stringify(['character-a', 'chat-a'])
        const read = registry.track({ id: 'chat-a', name: 'retained-marker-name', note: '', message: messages('long', 50_000) }, 'conversation', target, 1)
        registry.track({
            username: 'retained-marker-username',
            personas: [{ id: 'persona-a', name: 'retained-marker-persona' }],
            globalChatVariables: { toggle: 'retained-marker-variable' },
            characters: Array.from({ length: 4 }, (_, index) => ({
                chaId: `character-${index}`,
                desc: `retained-marker-desc-${index}`,
                chats: [{ id: `chat-${index}`, message: messages(`database-${index}`, 1000) }],
            })),
        }, 'database', 'database', 1)

        const strings = reachableStrings(registry)
        expect(strings.filter((value) => value.includes('retained-marker'))).toEqual([])
        expect(retainedLength(registry)).toBeLessThan(8192)

        const edited = { ...read, message: read.message.map((message, index) => index === 25_000 ? { ...message, data: 'Edited' } : message) }
        const intents = registry.intent(edited, 'conversation', target, {})
        expect(intents.map((intent) => [intent.path, intent.type])).toEqual([[['message'], 'set']])
        expect((intents[0].value as Array<{ data: string }>)[25_000].data).toBe('Edited')
        expect(registry.intent(structuredClone(read), 'conversation', target, {})).toEqual([])
    })

    it('refuses a read finishing under a different authority', () => {
        const { registry, replaceAuthority } = fixture()
        replaceAuthority()
        expect(() => registry.track({}, 'database', 'database', 1, 1)).toThrow('stale')
    })

    it('diffs a spread copy against the newest read after conversation metadata changed', () => {
        const { registry, character } = fixture()
        const target = JSON.stringify([character.chaId, 'chat-a'])
        registry.track(structuredClone(character.chats[0]), 'conversation', target, 1)
        const fresh = registry.track({ ...structuredClone(character.chats[0]), lastMemory: 'Remote' }, 'conversation', target, 2)
        const edited = { ...fresh, message: fresh.message.map(message => ({ ...message, data: 'Edited' })) }
        expect(registry.intent(edited, 'conversation', target, {})).toEqual([{ path: ['message'], type: 'set', value: [{ role: 'user', data: 'Edited' }] }])
        expect(registry.intent({ ...fresh, message: fresh.message.map(message => message) }, 'conversation', target, {})).toEqual([])
    })

    it('diffs a database copy against the newest read of each nested character', () => {
        const { registry, character } = fixture()
        const database = registry.track({ characters: [structuredClone(character)] }, 'database', 'database', 1)
        registry.track({ ...structuredClone(character), desc: 'Remote' }, 'character', character.chaId, 2)
        const copy = JSON.parse(JSON.stringify(database))
        copy.characters[0].name = 'Edited'
        copy.characters[0].desc = 'Remote'
        expect(registry.intent(copy, 'database', 'database', {})).toEqual([{ path: ['characters', character.chaId, 'name'], type: 'set', value: 'Edited' }])
    })

    it('keeps one database baseline per read key so a reduced read never replaces unread keys', () => {
        const registry = new PluginReadBaselines('synthetic-plugin', () => 1)
        const full = registry.track({ username: 'Before', personas: [{ id: 'p', name: 'Persona' }] }, 'database', 'database', 1)
        registry.track({ username: 'Remote' }, 'database', 'database', 2)
        const copy = { ...JSON.parse(JSON.stringify(full)), personas: [{ id: 'p', name: 'Edited' }] }
        expect(registry.intent(copy, 'database', 'database', {})).toEqual([
            { path: ['personas', 'p', 'name'], type: 'set', value: 'Edited' },
            { path: ['username'], type: 'set', value: 'Before' },
        ])
        expect(registry.intent({ personas: [{ id: 'p', name: 'Edited' }] }, 'database', 'database', {})).toEqual([{ path: ['personas', 'p', 'name'], type: 'set', value: 'Edited' }])
    })
})

describe('plugin read digests', () => {
    // The canonical form the baselines compared before they kept digests.
    const reference = (value: unknown): string => {
        if (value === undefined) return 'undefined'
        if (Array.isArray(value)) return '[' + value.map(reference).join(',') + ']'
        if (value && typeof value === 'object') {
            return '{' + Object.keys(value).sort().map(key => JSON.stringify(key) + ':' + reference((value as any)[key])).join(',') + '}'
        }
        return JSON.stringify(value)
    }
    const values: unknown[] = [
        NaN, null, Infinity, 0, -0, 1, '1', '', 'undefined', undefined, true, 'true', 'null',
        {}, [], new Date(0), { a: undefined }, { a: null }, { a: 1, b: 2 }, { b: 2, a: 1 }, { 'a:b': 1 }, { a: { b: 1 } }, { a: { b: 2 } },
        [1, [2]], [[1], 2], [undefined], [null], ['a', 'b'], ['ab'], [{ id: 'x', v: 1 }], [{ id: 'x', v: '1' }],
    ]

    it('treats two values as unchanged exactly when their canonical forms match', () => {
        for (const before of values) {
            for (const after of values) {
                const unchanged = reference(before) === reference(after)
                const field = diffPluginReadBaseline({ v: before }, { v: after }, 'preset')
                const recordField = diffPluginReadBaseline({ personas: [{ id: 'p', v: before }] }, { personas: [{ id: 'p', v: after }] }, 'preset')
                const wholeRecord = diffPluginReadBaseline({ modules: [{ id: 'm', v: before }] }, { modules: [{ id: 'm', v: after }] }, 'preset')
                const pair = `${reference(before)} -> ${reference(after)}`
                expect(field.length === 0, pair).toBe(unchanged)
                expect(recordField.length === 0, pair).toBe(unchanged)
                expect(wholeRecord.length === 0, pair).toBe(unchanged)
            }
        }
    })
})

describe('plugin changed-unit intent', () => {
    it('preserves removal, independent metadata and entire-message-list intent', () => {
        const { character } = fixture()
        const before = character.chats[0]
        const after = { ...before, name: 'Edited', message: [] } as any
        delete after.note
        const intents = diffPluginReadBaseline(before, after, 'conversation')
        expect(pluginUnitIntents(intents, 'conversation', 'synthetic-plugin', after, 'character-a', 'chat-a')).toEqual({
            units: [{ key: '["conversation","character-a","chat-a","name"]', type: 'set', value: 'Edited' }, { key: '["conversation","character-a","chat-a","note"]', type: 'delete' }],
            wholeMessages: [{ characterId: 'character-a', conversationId: 'chat-a', messages: [] }],
        })
    })

    it('rebases nested records by stable ID and preserves concurrent additions', () => {
        const { character } = fixture()
        const before = { characters: [character] }
        const after = structuredClone(before)
        after.characters[0].chats[0].note = 'Edited'
        const intents = diffPluginReadBaseline(before, after, 'database')
        const latest = structuredClone(before)
        latest.characters[0].chats[0].name = 'Remote'
        latest.characters[0].chats.push({ ...structuredClone(character.chats[0]), id: 'chat-b' })
        const result = rebasePluginFieldIntents(latest, intents)
        expect(result.characters[0].chats.map(chat => chat.id)).toEqual(['chat-a', 'chat-b'])
        expect(result.characters[0].chats[0]).toMatchObject({ name: 'Remote', note: 'Edited' })
        expect(pluginUnitIntents(intents, 'database', 'synthetic-plugin', after).units).toEqual([{ key: '["conversation","character-a","chat-a","note"]', type: 'set', value: 'Edited' }])
    })

    it('emits per-variable and per-owner storage diffs, keeping untouched stale values', () => {
        const before = { globalChatVariables: { toggle_a: '1', other: 'old' }, pluginCustomStorage: { retained: 1, edited: 2 } }
        const after = { globalChatVariables: { toggle_a: '2', other: 'old' }, pluginCustomStorage: { retained: 1, edited: 3 } }
        const changes = pluginUnitIntents(diffPluginReadBaseline(before, after, 'database'), 'database', 'synthetic-plugin', after)
        expect(changes.units).toEqual([
            { key: '["toggle","toggle_a"]', type: 'set', value: '2' },
            { key: '["plugin","synthetic-plugin","edited"]', type: 'set', value: 3 },
        ])
    })

    it.each(['character', 'conversation'] as const)('preserves explicit opaque own-field edits for %s without emitting untouched opaque values', kind => {
        const before: any = kind === 'character'
            ? { chaId: 'c', type: 'character', chats: [], retainedOpaque: { data: 'Imported' }, editedOpaque: { data: 'Before' }, deletedOpaque: true, chatPage: 0, statics: { messages: 2, count: 1 } }
            : { id: 'q', message: [], retainedOpaque: { data: 'Imported' }, editedOpaque: { data: 'Before' }, deletedOpaque: true }
        const registry = new PluginReadBaselines('p', () => 1)
        const target = kind === 'character' ? 'c' : JSON.stringify(['c', 'q'])
        const read: any = registry.track(structuredClone(before), kind, target, 1)
        read.editedOpaque = { data: 'Edited' }
        read.addedOpaque = [1, 2]
        delete read.deletedOpaque
        if (kind === 'character') { read.chatPage = 1; read.statics.messages = 3 }
        const intents = registry.intent(read, kind, target, {})
        const changes = pluginUnitIntents(intents, kind, 'p', read, 'c', 'q')
        const prefix = kind === 'character' ? ['character', 'c'] : ['conversation', 'c', 'q']
        expect(changes.units).toContainEqual({ key: JSON.stringify([...prefix, 'editedOpaque']), type: 'set', value: { data: 'Edited' } })
        expect(changes.units).toContainEqual({ key: JSON.stringify([...prefix, 'addedOpaque']), type: 'set', value: [1, 2] })
        expect(changes.units).toContainEqual({ key: JSON.stringify([...prefix, 'deletedOpaque']), type: 'delete' })
        expect(changes.units.some(unit => JSON.parse(unit.key).at(-1) === 'retainedOpaque')).toBe(false)
        expect(changes.units.some(unit => ['chaId', 'type', 'id'].includes(JSON.parse(unit.key).at(-1)))).toBe(false)
        expect(changes.wholeMessages).toEqual([])
        if (kind === 'character') {
            expect(changes.units).toContainEqual({ key: '["character","c","chatPage"]', type: 'set', value: 1 })
            expect(changes.units).toContainEqual({ key: '["character","c","statics"]', type: 'set', value: { messages: 3, count: 1 } })
        }
    })

    it('emits whole generic records and permanent deletion only for non-plugin collections', () => {
        const before = { plugins: [{ name: 'p', script: 'before' }], modules: [{ id: 'm', name: 'Before' }] }
        const after = { plugins: [{ name: 'p', script: 'after' }], modules: [] }
        expect(pluginUnitIntents(diffPluginReadBaseline(before, after, 'database'), 'database', 'p', after).units).toEqual([
            { key: '["order","modules"]', type: 'set', value: [] },
            { key: '["exists","modules","m"]', type: 'delete' },
            { key: '["record","modules","m"]', type: 'delete' },
            { key: '["record","plugins","p"]', type: 'set', value: { name: 'p', script: 'after' } },
        ])
    })
})
