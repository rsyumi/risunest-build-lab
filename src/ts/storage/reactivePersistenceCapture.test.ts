import { describe, expect, it, vi } from 'vitest'
import { createPersistenceCanonicalCapture } from './reactivePersistenceCapture.svelte'
import { createPersistentSaveObserverHarness } from './tests/persistentSaveObserverHarness.svelte'
import {
    canonicalJson,
    pluginStorageJson,
    PluginStorageCaptureCache,
} from './saveCoordinatorHelpers'
import type { Database } from './database.svelte'
import { captureMaterializedCharacter } from './persistentUnitCapture'
import { cloneConversationByMessage } from './conversationInsertPages'

function fixture() {
    return createPersistentSaveObserverHarness({
        username: 'Before',
        setting: { nested: { count: 0 } },
        large: 'x'.repeat(6 * 1024 * 1024),
        pluginCustomStorage: { nested: { count: 0 } },
        botPresets: [{ name: 'Preset', nested: [1] }],
        characters: [{ chaId: 'synthetic', chats: [{ message: [{ data: 'hello' }] }] }],
    } as unknown as Database)
}
function capture(state: ReturnType<typeof fixture>, root = () => state.database) {
    return createPersistenceCanonicalCapture({
        root,
        pluginStorage: () => state.database.pluginCustomStorage,
        presets: () => state.database.botPresets,
        character: () => state.database.characters[state.selectedIndex] ?? null,
    })
}
function root(state: ReturnType<typeof fixture>) {
    const {
        characters: _,
        pluginCustomStorage: __,
        pluginStorageMeta: ___,
        botPresets: ____,
        ...value
    } = state.database
    return value
}

describe('production reactive persistence captures', () => {
    it('preserves full chat hook receivers, transformed messages and nested serialization order', () => {
        const state = fixture()
        const calls: string[] = []
        let outside = 'before'
        const chat = state.database.characters[0].chats[0] as any
        chat.id = 'hook-chat'
        chat.toJSON = function (key: string) {
            calls.push(`chat:${key}`)
            return { id: this.id, note: `${this.message.length}:${outside}`,
                message: this.message.map((value: { data: string }) => ({
                    toJSON(messageKey: string) { calls.push(`message:${messageKey}`); return { data: `${value.data}:${outside}` } },
                })) }
        }
        const cached = createPersistenceCanonicalCapture({ root: () => state.database, pluginStorage: () => null,
            presets: () => [], character: () => state.database.characters[0], characters: () => state.database.characters })
        const first = cached.materializedCharacters!().get('synthetic')!
        expect(first.chats[0]).toEqual({ id: 'hook-chat', note: '1:before', message: [{ data: 'hello:before' }] })
        expect(calls).toEqual(['chat:', 'message:0'])
        calls.length = 0
        expect(cached.materializedCharacters!().get('synthetic')).toBe(first)
        expect(calls).toEqual(['chat:', 'message:0'])
        outside = 'after'
        expect(cached.materializedCharacters!().get('synthetic')!.chats[0].message[0].data).toBe('hello:after')
        expect(cached.characterShell!()!.value.chats[0]).toEqual({ id: 'hook-chat', note: '1:after' })
        expect(cloneConversationByMessage(chat)).toEqual(JSON.parse(canonicalJson(chat)))
        expect(captureMaterializedCharacter(state.database.characters[0]).chats[0]).toEqual(JSON.parse(canonicalJson(chat)))
    })

    it('preserves full character hook receivers and transformed chat bodies', () => {
        const state = fixture()
        const calls: string[] = []
        const owner = state.database.characters[0] as any
        owner.toJSON = function (key: string) {
            calls.push(`character:${key}`)
            return { chaId: this.chaId, name: `messages:${this.chats[0].message.length}`,
                chats: this.chats.map((chat: { message: unknown[] }) => ({
                    toJSON(chatKey: string) { calls.push(`chat:${chatKey}`); return { id: 'transformed', message: [...chat.message, { data: 'extra' }] } },
                })) }
        }
        const cached = createPersistenceCanonicalCapture({ root: () => state.database, pluginStorage: () => null,
            presets: () => [], character: () => owner, characters: () => state.database.characters })
        const first = cached.materializedCharacters!().get('synthetic')!
        expect(first.name).toBe('messages:1')
        expect(first.chats[0].message.map((message) => message.data)).toEqual(['hello', 'extra'])
        expect(calls).toEqual(['character:', 'chat:0'])
        owner.chats[0].message.push({ data: 'second' })
        expect(cached.materializedCharacters!().get('synthetic')!.name).toBe('messages:2')
        expect(cached.characterShell!()!.value.chats).toEqual([{ id: 'transformed' }])
        expect(captureMaterializedCharacter(owner)).toEqual(JSON.parse(canonicalJson(owner)))
    })

    it('keeps accessor side effects in complete chat serialization order', () => {
        let note = 'before'
        const chat = { id: 'accessor-chat',
            get note() { return note },
            get message() { note = 'after'; return [{ data: 'body' }] },
        }
        const owner = { chaId: 'accessor-owner', chats: [chat] }
        const cached = createPersistenceCanonicalCapture({ root: () => ({}), pluginStorage: () => null,
            presets: () => [], character: () => owner, characters: () => [owner as any] })
        expect(cached.materializedCharacters!().get(owner.chaId)!.chats[0].note).toBe('before')
        expect(cached.materializedCharacters!().get(owner.chaId)!.chats[0].note).toBe('after')
    })

    it.each(['hidden', 'inherited'] as const)('does not read %s toJSON accessors on message bodies or messages', (placement) => {
        const message = { data: 'body' }
        const body = [message]
        const ignored = { get() { throw new Error('ignored serialization accessor was evaluated') }, configurable: true }
        for (const value of [body, message]) {
            if (placement === 'hidden') Object.defineProperty(value, 'toJSON', ignored)
            else {
                const prototype = Object.create(Object.getPrototypeOf(value))
                Object.defineProperty(prototype, 'toJSON', { ...ignored, enumerable: true })
                Object.setPrototypeOf(value, prototype)
            }
        }
        const owner = { chaId: 'ignored-hooks', chats: [{ id: 'chat', message: body }] }
        const cached = createPersistenceCanonicalCapture({ root: () => ({}), pluginStorage: () => null,
            presets: () => [], character: () => owner, characters: () => [owner as any] })
        expect(cached.materializedCharacters!().get(owner.chaId)!.chats[0].message).toEqual([{ data: 'body' }])
    })

    it('returns to ordinary character capture after a serialization hook is removed', () => {
        const state = fixture()
        const owner = state.database.characters[0] as any
        owner.name = 'normal'
        owner.chats = []
        const cached = createPersistenceCanonicalCapture({ root: () => state.database, pluginStorage: () => null,
            presets: () => [], character: () => owner, characters: () => state.database.characters })
        expect(cached.materializedCharacters!().get(owner.chaId)!.name).toBe('normal')
        owner.toJSON = () => ({ chaId: owner.chaId, name: 'hook', chats: [] })
        expect(cached.materializedCharacters!().get(owner.chaId)!.name).toBe('hook')
        delete owner.toJSON
        expect(cached.materializedCharacters!().get(owner.chaId)!.name).toBe('normal')
    })

    it('owns shell fields without reading bodies and invalidates nested metadata from reactive evidence', () => {
        const state = fixture()
        const owner = state.database.characters[0] as any
        owner.metadata = { nested: { value: 'x'.repeat(1024 * 1024) } }
        const chat = { id: 'shell', note: '' }
        Object.defineProperty(chat, 'message', { enumerable: false, get: () => { throw new Error('body must stay unread') } })
        owner.chats = [chat]
        const cached = createPersistenceCanonicalCapture({ root: () => state.database, pluginStorage: () => null,
            presets: () => [], character: () => owner, characters: () => state.database.characters })
        const first = cached.characterShell!()!
        expect(cached.characterShell!()).toBe(first)
        owner.lastInteraction = 42
        const next = cached.characterShell!()!
        expect((next.value as typeof owner).metadata).toBe((first.value as typeof owner).metadata)
        owner.metadata.nested.value = 'nested edit'
        const edited = cached.characterShell!()!
        expect((edited.value as typeof owner).metadata).not.toBe((first.value as typeof owner).metadata)
        expect((edited.value as any).metadata.nested.value).toBe('nested edit')
        expect((first.value as any).metadata.nested.value.length).toBe(1024 * 1024)
    })

    it('preserves message serialization hook indices and detects nonreactive hook changes', () => {
        const state = fixture()
        let outside = 'before'
        state.database.characters[0].chats[0].message = [0, 1].map(() => ({
            role: 'user', data: '', toJSON(key: string) { return { role: 'user', data: `${key}:${outside}` } },
        })) as any
        const cached = createPersistenceCanonicalCapture({ root: () => state.database, pluginStorage: () => null,
            presets: () => [], character: () => state.database.characters[0], characters: () => state.database.characters })
        const first = cached.materializedCharacters!().get('synthetic')!
        expect(first.chats[0].message.map((message) => message.data)).toEqual(['0:before', '1:before'])
        expect(cached.materializedCharacters!().get('synthetic')).toBe(first)
        outside = 'after'
        expect(cached.materializedCharacters!().get('synthetic')!.chats[0].message.map((message) => message.data)).toEqual(['0:after', '1:after'])
    })

    it('reuses unchanged message encoding and decoded values when resident chat metadata changes', () => {
        const state=fixture()
        state.database.characters[0].chats[0].id='conversation'
        const cached=createPersistenceCanonicalCapture({root:()=>state.database,pluginStorage:()=>state.database.pluginCustomStorage,
            presets:()=>state.database.botPresets,character:()=>state.database.characters[0],characters:()=>state.database.characters})
        const before=cached.materializedCharacters!().get('synthetic')!
        cached.characters!()
        const stringify=vi.spyOn(JSON,'stringify')
        try {
            state.database.characters[0].chats[0].note='metadata only'
            const after=cached.materializedCharacters!().get('synthetic')!
            expect(after.chats[0].note).toBe('metadata only')
            expect(after.chats[0].message).toBe(before.chats[0].message)
            expect(stringify.mock.calls.some(([value])=>Array.isArray(value))).toBe(false)
        } finally { stringify.mockRestore() }
    })

    it('returns an unchanged resident character without composing its JSON again', () => {
        const state=fixture()
        state.database.characters[0].chats[0].id='conversation'
        state.database.characters[0].chats.push({id:'second',message:[{data:'second'}]} as never)
        const cached=createPersistenceCanonicalCapture({root:()=>state.database,pluginStorage:()=>state.database.pluginCustomStorage,
            presets:()=>state.database.botPresets,character:()=>state.database.characters[0],characters:()=>state.database.characters})
        const before=cached.characters!().get('synthetic')!
        const stringify=vi.spyOn(JSON,'stringify')
        const join=vi.spyOn(Array.prototype,'join')
        try {
            expect(cached.characters!().get('synthetic')).toBe(before)
            expect(stringify).not.toHaveBeenCalled()
            expect(join).not.toHaveBeenCalled()
        } finally { stringify.mockRestore(); join.mockRestore() }
        state.database.characters[0].chats[1].note='changed'
        expect(JSON.parse(cached.characters!().get('synthetic')!).chats[1].note).toBe('changed')
    })

    it('keeps plugin storage ownership metadata out of the persistent root', () => {
        const state = fixture()
        state.database.pluginStorageMeta = {
            nested: { plugin: 'plugin-a', updatedAt: 1 },
        }
        const cached = capture(state)
        expect(JSON.parse(cached.root())).not.toHaveProperty('pluginStorageMeta')
    })

    it('reuses initialized strings on first reactive capture and defers whole-object encoding', () => {
        const value = 'x'.repeat(16 * 1024 * 1024)
        const state = fixture()
        state.database.pluginCustomStorage = { payload: value }
        const seeded = new PluginStorageCaptureCache().capture({ payload: value })
        const cached = capture(state)
        cached.seedPluginStorage!(seeded)
        const stringify = vi.spyOn(JSON, 'stringify')
        const join = vi.spyOn(Array.prototype, 'join')
        let result: ReturnType<typeof cached.pluginStorage>
        try {
            result = cached.pluginStorage()
            expect(stringify.mock.calls.some(([input]) => input === value)).toBe(false)
            expect(join).not.toHaveBeenCalled()
            expect(result!.entries).toEqual(seeded.entries)
        } finally {
            stringify.mockRestore()
            join.mockRestore()
        }
        expect(result!.json).toBe(pluginStorageJson({ payload: value }))
        state.database.pluginCustomStorage.payload = 'changed'
        expect(cached.pluginStorage()!.json).toBe(pluginStorageJson({ payload: 'changed' }))
    })

    it('does not reuse a seed when the value changed before its first reactive capture', () => {
        const state = fixture()
        state.database.pluginCustomStorage = { payload: 'after', nested: { count: 2 } }
        const cached = capture(state)
        cached.seedPluginStorage!(
            new PluginStorageCaptureCache().capture({
                payload: 'before',
                nested: { count: 1 },
            }),
        )
        expect(cached.pluginStorage()!.json).toBe(
            pluginStorageJson(state.database.pluginCustomStorage),
        )
        state.database.pluginCustomStorage.nested.count++
        expect(cached.pluginStorage()!.json).toBe(
            pluginStorageJson(state.database.pluginCustomStorage),
        )
    })

    it('does not cache plain or raw objects whose edits cannot invalidate a derived', () => {
        const raw = {
            root: { nested: { value: 1 } },
            storage: { a: { count: 1 } },
            presets: [1],
            character: { name: 'first' },
        }
        const cached = createPersistenceCanonicalCapture({
            root: () => raw.root,
            pluginStorage: () => raw.storage,
            presets: () => raw.presets,
            character: () => raw.character,
        })
        cached.root()
        cached.pluginStorage()
        cached.presets()
        cached.character()
        raw.root.nested.value++
        raw.storage.a.count++
        raw.presets.push(2)
        raw.character.name = 'second'
        expect(cached.root()).toBe(canonicalJson(raw.root))
        expect(cached.pluginStorage()!.json).toBe(pluginStorageJson(raw.storage))
        expect(cached.presets()).toBe(canonicalJson(raw.presets))
        expect(cached.character()).toBe(canonicalJson(raw.character))
    })

    it('observes immediate nested edits without waiting for effects and creates only changed root fields', () => {
        const state = fixture()
        const cached = capture(state)
        const before = cached.root()
        ;(state.database as unknown as Record<string, any>).setting.nested.count++
        const after = cached.root()
        expect(after).toBe(canonicalJson(root(state)))
        expect(cached.diffRoot(before, after)).toEqual([
            { type: 'set', key: 'setting', value: { nested: { count: 1 } } },
        ])
        expect(JSON.stringify(cached.diffRoot(before, after)).length).toBeLessThan(256)
        delete (state.database as unknown as Record<string, any>).setting
        expect(cached.diffRoot(after, cached.root())).toEqual([{ type: 'delete', key: 'setting' }])
    })

    it('does not visit unchanged fields during a setting save', () => {
        const state = fixture()
        const reads = vi.fn()
        const cached = capture(
            state,
            () =>
                new Proxy(state.database, {
                    get(target, key, receiver) {
                        reads(key)
                        return Reflect.get(target, key, receiver)
                    },
                }),
        )
        cached.root()
        reads.mockClear()
        state.database.username = 'After'
        cached.root()
        expect(reads.mock.calls.map(([key]) => key)).not.toContain('large')
        expect(reads.mock.calls.map(([key]) => key)).not.toContain('setting')
    })

    it('keeps plugin order, array holes, selection and detached values correct', () => {
        const state = fixture()
        const cached = capture(state)
        cached.pluginStorage()
        cached.presets()
        cached.character()
        state.database.pluginCustomStorage.nested.count++
        state.database.pluginCustomStorage['__proto__'] = { own: true }
        expect(cached.pluginStorage()!.json).toBe(
            pluginStorageJson(state.database.pluginCustomStorage),
        )
        const detached = cached.pluginStorage()!.value
        detached.nested.count = 999
        expect(cached.pluginStorage()!.value.nested.count).toBe(1)
        delete state.database.botPresets[0]
        expect(cached.presets()).toBe(canonicalJson(state.database.botPresets))
        state.database.characters[0].chats[0].message[0].data += '!'
        expect(cached.character()).toBe(canonicalJson(state.database.characters[0]))
        state.selectedIndex = 1
        expect(cached.character()).toBeNull()
    })

    it('refreshes getters and callable hooks backed by non-reactive values', () => {
        const state = fixture()
        let outside = 1
        state.database = {
            ...state.database,
            get getter() {
                return outside
            },
        } as Database
        ;(state.database as unknown as Record<string, any>).setting = {
            toJSON() {
                return outside
            },
        }
        state.database.pluginCustomStorage.hook = {
            toJSON(key: string) {
                return `${key}:${outside}`
            },
        }
        const cached = capture(state)
        expect(cached.root()).toBe(canonicalJson(root(state)))
        outside++
        expect(cached.root()).toBe(canonicalJson(root(state)))
        expect(cached.pluginStorage()!.json).toBe(
            pluginStorageJson(state.database.pluginCustomStorage),
        )
    })
})
