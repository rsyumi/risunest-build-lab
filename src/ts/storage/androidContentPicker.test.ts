import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    invoke: vi.fn(), discard: vi.fn(), character: vi.fn(), module: vi.fn(), pick: vi.fn(),
    controller: new AbortController(),
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('./androidSafBridge', () => ({ pickAndroidContentSource: mocks.pick }))
vi.mock('./nativeFileJobs', () => ({ syntheticNativeFileJobStatus: vi.fn() }))
vi.mock('./nativeFileJobManager', () => ({
    runSharedNativeFileOperation: async (_kind: string, _key: string, run: Function) => run({
        signal: mocks.controller.signal, setSource: vi.fn(), onStatus: vi.fn(),
    }),
}))
vi.mock('../characterCards', () => ({ importCharacterProcess: mocks.character }))
vi.mock('../process/modules', () => ({ importModuleData: mocks.module }))
vi.mock('./database.svelte', () => ({ getDatabase: () => ({ characters: [{ chaId: 'imported-character' }] }) }))
import { importAndroidContentFromPicker, importReplayedAndroidContentSpool } from './androidContentPicker'

const source = { token: 'synthetic-token', displayName: 'synthetic.json', bytes: 100 }
const encoded = (text: string) => new TextEncoder().encode(text).buffer
beforeEach(() => {
    vi.resetAllMocks()
    mocks.controller = new AbortController()
    mocks.character.mockResolvedValue(0)
    mocks.pick.mockImplementation(async (options) => {
        options.onSource(source)
        return { type: 'androidSpool', token: source.token }
    })
    Object.assign(window, { RisuSafBridge: { discardSource: mocks.discard } })
})

describe('Android bounded JSON compatibility replay', () => {
    it.each([
        ['CCv2', { spec: 'chara_card_v2', data: {} }, 'character'],
        ['CCv3', { spec: 'chara_card_v3', data: {} }, 'character'],
        ['off-spec', { name: 'Synthetic' }, 'character'],
        ['module', { type: 'risuModule', id: 'synthetic', name: 'Synthetic' }, 'module'],
        ['risu lorebook', { type: 'risu', data: [] }, 'module'],
        ['external lorebook', { entries: {} }, 'module'],
        ['regex module', { type: 'regex', data: [] }, 'module'],
        ['ambiguous preset', { type: 'risu', data: {} }, 'character'],
    ] as const)('uses the same converter for picker and external %s', async (_name, json, destination) => {
        mocks.invoke.mockResolvedValue(encoded(JSON.stringify(json)))
        const external = await importReplayedAndroidContentSpool(source, 'auto')
        const picked = await importAndroidContentFromPicker(destination)
        expect(external).toBe(picked)
        const converter = destination === 'module' ? mocks.module : mocks.character
        const other = destination === 'module' ? mocks.character : mocks.module
        expect(converter).toHaveBeenCalledTimes(2)
        expect(converter.mock.calls[0]).toEqual(converter.mock.calls[1])
        expect(other).not.toHaveBeenCalled()
        expect(mocks.invoke).toHaveBeenCalledTimes(2)
        expect(mocks.invoke).toHaveBeenCalledWith('native_content_source_metadata', { token: source.token })
        expect(mocks.discard).toHaveBeenCalledTimes(2)
    })
    it.each(['oversized', 'unreadable'])('preserves the bounded native reader rejection for %s JSON', async (reason) => {
        const error = new Error(reason)
        mocks.invoke.mockRejectedValue(error)
        await expect(importReplayedAndroidContentSpool(source, 'auto')).rejects.toBe(error)
        expect(mocks.character).not.toHaveBeenCalled()
        expect(mocks.module).not.toHaveBeenCalled()
        expect(mocks.discard).toHaveBeenCalledExactlyOnceWith(source.token)
    })
    it('hands converters the bytes the native reader returned', async () => {
        const text = '﻿{"name":"Synthetic é"}'
        mocks.invoke.mockResolvedValue(encoded(text))
        await importReplayedAndroidContentSpool(source, 'character')
        expect(mocks.character).toHaveBeenCalledWith({
            name: source.displayName,
            data: new TextEncoder().encode(text),
        })
    })
    it('accepts the bytes as a number array', async () => {
        mocks.invoke.mockResolvedValue([...new TextEncoder().encode('{"entries":{}}')])
        await expect(importReplayedAndroidContentSpool(source, 'auto')).resolves.toBe('module')
        expect(mocks.module).toHaveBeenCalledWith({
            name: source.displayName,
            data: new TextEncoder().encode('{"entries":{}}'),
        })
    })
    it('rejects bytes that are not UTF-8 before any converter executes', async () => {
        mocks.invoke.mockResolvedValue(Uint8Array.of(0x7b, 0xff, 0x7d).buffer)
        await expect(importReplayedAndroidContentSpool(source, 'auto')).rejects.toBeInstanceOf(TypeError)
        expect(mocks.character).not.toHaveBeenCalled()
        expect(mocks.module).not.toHaveBeenCalled()
        expect(mocks.discard).toHaveBeenCalledExactlyOnceWith(source.token)
    })
    it('discards malformed JSON exactly once', async () => {
        mocks.invoke.mockResolvedValue(encoded('{'))
        await expect(importReplayedAndroidContentSpool(source, 'auto')).rejects.toBeInstanceOf(SyntaxError)
        expect(mocks.discard).toHaveBeenCalledExactlyOnceWith(source.token)
    })
    it('discards cancelled metadata before any converter executes', async () => {
        mocks.invoke.mockImplementation(async () => {
            mocks.controller.abort()
            return encoded('{}')
        })
        await expect(importReplayedAndroidContentSpool(source, 'auto')).rejects.toMatchObject({ name: 'AbortError' })
        expect(mocks.character).not.toHaveBeenCalled()
        expect(mocks.module).not.toHaveBeenCalled()
        expect(mocks.discard).toHaveBeenCalledOnce()
    })
})
