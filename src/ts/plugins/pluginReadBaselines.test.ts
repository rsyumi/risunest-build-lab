import { describe, expect, it } from 'vitest'
import { attachPluginReadProvenance, collectPluginReadProvenance, diffPluginReadBaseline, PluginReadBaselines, rebasePluginFieldIntents } from './pluginReadBaselines'
import { pluginUnitIntents } from './pluginUnitIntents'

function fixture() {
    let authority = 1
    const registry = new PluginReadBaselines('synthetic-plugin', () => authority)
    const character = { chaId: 'character-a', name: 'Before', desc: 'Retained', chats: [{ id: 'chat-a', name: 'Chat', note: 'Before', message: [{ role: 'user', data: 'Synthetic' }] }] }
    return { registry, character, replaceAuthority: () => { authority++; registry.expire() } }
}

describe('plugin immutable read baselines', () => {
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
        oldest.desc = 'Old read edit'
        expect(registry.intent(oldest, 'character', character.chaId, {})).toEqual([{ path: ['desc'], type: 'set', value: 'Old read edit' }])
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
