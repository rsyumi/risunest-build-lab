import { describe, expect, it, vi } from 'vitest'
import { coldItemFromMap, expandColdPayloads, importColdPluginStorage, type ColdItemRead } from './coldPayloadExpansion'
import { coldStorageHeader } from './coldstorageData'

const present = (value: unknown) => ({ kind: 'present' as const, value })

function placeholderChat(key: string) {
    return {
        id: `chat-${key}`,
        message: [{ time: 1, data: `${coldStorageHeader}${key}`, role: 'char' }],
        note: '',
        name: '',
        localLore: [],
    }
}

describe('expandColdPayloads', () => {
    it('restores an archived character and drops its reference', async () => {
        const db = {
            characters: [
                {
                    type: 'character',
                    chaId: 'cha-1',
                    name: 'Stub',
                    chats: [placeholderChat('chat-key')],
                    coldstorage: 'character-key',
                    coldStoragedChats: ['chat-key'],
                },
            ],
        } as any

        const read = vi.fn(async (key: string) => {
            if (key === 'character-key') {
                return present({
                    character: {
                        type: 'character',
                        chaId: 'cha-1',
                        name: 'Restored',
                        chats: [placeholderChat('chat-key')],
                    },
                })
            }
            return present({ message: [{ time: 2, data: 'body', role: 'char' }], localLore: [] })
        })

        const result = await expandColdPayloads(db, read)

        expect(db.characters[0].name).toBe('Restored')
        expect(db.characters[0].chats[0].message[0].data).toBe('body')
        expect(db.characters[0].coldstorage).toBeUndefined()
        expect(db.characters[0].coldStoragedChats).toBeUndefined()
        expect(result.expandedKeys).toEqual(['character-key', 'chat-key'])
        expect(result.unavailableKeys).toEqual([])
    })

    it('accepts a bare message array payload', async () => {
        const db = { characters: [{ chaId: 'cha-1', chats: [placeholderChat('k')] }] } as any
        const messages = [{ time: 3, data: 'array body', role: 'char' }]

        await expandColdPayloads(db, async () => present(messages))

        expect(db.characters[0].chats[0].message).toEqual(messages)
    })

    it.each([
        ['missing', { kind: 'missing' as const }],
        ['unreadable', { kind: 'unreadable' as const }],
        ['a stored null', present(null)],
    ])('keeps the record and reports the key when a payload is %s', async (_name, item) => {
        const db = {
            characters: [
                {
                    chaId: 'cha-1',
                    name: 'Stub',
                    chats: [placeholderChat('missing-chat')],
                    coldstorage: 'missing-character',
                    coldStoragedChats: ['missing-chat'],
                },
            ],
        } as any

        const result = await expandColdPayloads(db, async () => item)

        expect(db.characters[0].name).toBe('Stub')
        expect(db.characters[0].chats[0].message[0].data).toBe(`${coldStorageHeader}missing-chat`)
        expect(db.characters[0].coldstorage).toBe('missing-character')
        expect(db.characters[0].coldStoragedChats).toEqual(['missing-chat'])
        expect(result.expandedKeys).toEqual([])
        expect(result.unavailableKeys).toEqual(['missing-character', 'missing-chat'])
    })

    it('reports a read failure instead of throwing', async () => {
        const db = { characters: [{ chaId: 'cha-1', chats: [placeholderChat('boom')] }] } as any

        const result = await expandColdPayloads(db, async () => {
            throw new Error('unreachable')
        })

        expect(result.unavailableKeys).toEqual(['boom'])
    })

    it('rethrows an abort instead of marking every key unavailable', async () => {
        const db = {
            characters: [{ chaId: 'cha-1', chats: [placeholderChat('a'), placeholderChat('b')] }],
        } as any

        const read = vi.fn(async () => {
            throw new DOMException('aborted', 'AbortError')
        })

        await expect(expandColdPayloads(db, read)).rejects.toThrow('aborted')
        expect(read).toHaveBeenCalledTimes(1)
    })

    it('normalizes PocketRisu swipes while expanding', async () => {
        const db = { characters: [{ chaId: 'cha-1', chats: [placeholderChat('pocket')] }] } as any

        await expandColdPayloads(db, async () => present({
            message: [{ time: 4, data: 'b', role: 'char', swipes: ['a', 'b'], swipeId: 1 }],
        }))

        const message = db.characters[0].chats[0].message[0]
        expect(message.responseVariants.candidates).toHaveLength(2)
        expect(message.responseVariants.selectedId).toBe(message.responseVariants.candidates[1].id)
    })
})

function pluginIndex(entries: unknown[]) {
    return { type: 'risu-plugin-storage-index', version: 1, entries, trash: [{ id: 'trashed', at: 1 }] }
}

async function importPlugins(storage: Record<string, unknown>, items: Record<string, unknown>) {
    const db = { pluginCustomStorage: storage }
    const map = new Map(Object.entries(items))
    const read = vi.fn(async (key: string) => coldItemFromMap(map, key))
    const result = await importColdPluginStorage(db, read)
    return { ...result, storage: db.pluginCustomStorage, read }
}

describe('importColdPluginStorage', () => {
    it('imports index values, small and large, and drops the index reference', async () => {
        const result = await importPlugins({ _coldpluginIndex: 'idx' }, {
            idx: pluginIndex([
                { k: 'theme', s: '"dark"' },
                { k: 'rows', l: 'large-rows' },
                { k: 'both', s: '"small"', l: 'large-both' },
                { k: 'theme', s: '"latest"' },
            ]),
            'large-rows': { rows: [1, 2, 3] },
            'large-both': 'large wins',
        })

        expect(result.omitted).toBe(0)
        expect(result.storage).toEqual({ theme: 'latest', rows: { rows: [1, 2, 3] }, both: 'large wins' })
        expect(result.read).not.toHaveBeenCalledWith('trashed')
    })

    it('imports the legacy map and keeps inline values over index and legacy entries', async () => {
        const result = await importPlugins({
            inline: 'inline value',
            _coldpluginIndex: 'idx',
            _coldplugin: { inline: 'legacy-inline', shadowed: 'legacy-shadowed', legacy: 'legacy-only' },
        }, {
            idx: pluginIndex([{ k: 'inline', s: '"index value"' }, { k: 'shadowed', l: 'gone' }]),
            'legacy-inline': 'old inline',
            'legacy-shadowed': 'old shadowed',
            'legacy-only': { nested: true },
        })

        expect(result.storage).toEqual({ inline: 'inline value', legacy: { nested: true } })
        expect(result.omitted).toBe(1)
    })

    it('keeps every primitive exactly through small, large and legacy entries', async () => {
        const primitives = { nullValue: null, falseValue: false, zero: 0, empty: '' }
        const keys = Object.keys(primitives) as (keyof typeof primitives)[]
        const items: Record<string, unknown> = {
            idx: pluginIndex([
                ...keys.map((k) => ({ k: `s-${k}`, s: JSON.stringify(primitives[k]) })),
                ...keys.map((k) => ({ k: `l-${k}`, l: `large-${k}` })),
            ]),
        }
        for (const k of keys) {
            items[`large-${k}`] = primitives[k]
            items[`legacy-${k}`] = primitives[k]
        }
        const result = await importPlugins({
            _coldpluginIndex: 'idx',
            _coldplugin: Object.fromEntries(keys.map((k) => [`legacy-${k}`, `legacy-${k}`])),
        }, items)

        expect(result.omitted).toBe(0)
        const expected: Record<string, unknown> = {}
        for (const k of keys) {
            for (const route of ['s', 'l', 'legacy']) expected[`${route}-${k}`] = primitives[k]
        }
        expect(result.storage).toStrictEqual(expected)
    })

    it('omits a missing large item and keeps the other values', async () => {
        const result = await importPlugins({ _coldpluginIndex: 'idx' }, {
            idx: pluginIndex([{ k: 'kept', s: '1' }, { k: 'lost', l: 'missing-large' }]),
        })

        expect(result.storage).toEqual({ kept: 1 })
        expect(result.omitted).toBe(1)
    })

    it.each([
        ['missing', {}],
        ['not an index', { idx: { type: 'something-else', entries: [] } }],
        ['an index without trash', { idx: { type: 'risu-plugin-storage-index', version: 1, entries: [{ k: 'x', s: '1' }] } }],
    ])('omits an index that is %s and still imports the legacy map', async (_name, items) => {
        const result = await importPlugins({
            _coldpluginIndex: 'idx',
            _coldplugin: { legacy: 'legacy-id' },
        }, { ...items, 'legacy-id': 'legacy value' })

        expect(result.storage).toEqual({ legacy: 'legacy value' })
        expect(result.omitted).toBe(1)
    })

    it('counts corrupt entries, ids and maps without storing them', async () => {
        const result = await importPlugins({
            _coldpluginIndex: 'idx',
            _coldplugin: { numeric: 42 },
        }, {
            idx: pluginIndex([{ k: 'bad-json', s: '{' }, { k: 7, s: '1' }, { k: 'no-value' }, null, { k: 'good', s: 'true' }]),
        })

        expect(result.storage).toEqual({ good: true })
        expect(result.omitted).toBe(5)

        const corruptMap = await importPlugins({ _coldplugin: 'corrupt', _coldpluginIndex: 3 }, {})
        expect(corruptMap.storage).toEqual({})
        expect(corruptMap.omitted).toBe(2)
    })

    it('counts too large index, large and legacy items apart from other omissions', async () => {
        const db = {
            pluginCustomStorage: {
                _coldpluginIndex: 'idx',
                _coldplugin: { legacy: 'legacy-huge', gone: 'legacy-gone' },
            } as Record<string, unknown>,
        }
        const read = async (key: string): Promise<ColdItemRead> => key === 'idx'
            ? { kind: 'present', value: pluginIndex([{ k: 'large', l: 'large-huge' }, { k: 'kept', s: '1' }]) }
            : key.endsWith('-huge') ? { kind: 'too-large' } : { kind: 'missing' }

        await expect(importColdPluginStorage(db, read)).resolves.toEqual({ omitted: 1, tooLarge: 2 })
        expect(db.pluginCustomStorage).toEqual({ kept: 1 })

        const oversizedIndex = { pluginCustomStorage: { _coldpluginIndex: 'idx' } as Record<string, unknown> }
        await expect(importColdPluginStorage(oversizedIndex, async () => ({ kind: 'too-large' }))).resolves.toEqual({ omitted: 0, tooLarge: 1 })
    })

    it('reports an unreadable item as an omission and rethrows an abort', async () => {
        const db = { pluginCustomStorage: { _coldplugin: { a: 'id-a' } } as Record<string, unknown> }
        await expect(importColdPluginStorage(db, async () => {
            throw new Error('synthetic read failure')
        })).resolves.toEqual({ omitted: 1, tooLarge: 0 })
        expect(db.pluginCustomStorage).toEqual({})

        const aborted = { pluginCustomStorage: { _coldplugin: { a: 'id-a' } } }
        await expect(importColdPluginStorage(aborted, async () => {
            throw new DOMException('aborted', 'AbortError')
        })).rejects.toThrow('aborted')
    })

    it('leaves storage without cold references untouched', async () => {
        const storage = { plain: { value: 1 } }
        const result = await importPlugins(storage, {})

        expect(result.omitted).toBe(0)
        expect(result.storage).toBe(storage)
        expect(result.read).not.toHaveBeenCalled()
    })
})
