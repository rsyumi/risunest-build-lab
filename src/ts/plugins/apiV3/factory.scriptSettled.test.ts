import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { SandboxHost } from './factory'
import { collectPluginReadProvenance, PluginReadBaselines } from '../pluginReadBaselines'

/**
 * The guest reports when its top level script has settled. A window the host
 * opened for that script must stay open until the calls the script already made
 * have answered.
 */
async function runGuest(host: SandboxHost, code: string): Promise<Window & typeof globalThis> {
    const frame = document.createElement('iframe')
    document.body.appendChild(frame)
    host.run(frame, code, 'settled-test')
    const child = frame.contentWindow as Window & typeof globalThis
    const childRealm = child as any
    Object.defineProperty(child, 'ImageBitmap', {
        configurable: true,
        value: globalThis.ImageBitmap,
    })
    vi.spyOn(window, 'postMessage').mockImplementation((data: unknown) => {
        window.dispatchEvent(new MessageEvent('message', { data, source: child }))
    })
    vi.spyOn(child.parent, 'postMessage').mockImplementation((data: unknown) => {
        window.dispatchEvent(new MessageEvent('message', { data, source: child }))
    })
    vi.spyOn(child, 'postMessage').mockImplementation(function (
        data: unknown,
        options?: WindowPostMessageOptions,
    ) {
        const legacyTransfer = arguments[2] as Transferable[] | undefined
        const transfer = options?.transfer ?? legacyTransfer ?? []
        child.dispatchEvent(new childRealm.MessageEvent('message', {
            data,
            source: child.parent,
            ports: transfer as MessagePort[],
        }))
    })
    const source = frame.srcdoc.match(/<script nonce="[^"]+">([\s\S]*)<\/script>/)?.[1]
    if (!source) throw new Error('Sandbox guest script was not found')
    await childRealm.eval(source)
    return child
}

/** The guest bridge asks for these before it hands control to the script. */
function bridgeStubs(): Record<string, unknown> {
    return {
        _getPropertiesForInitialization: () => ({ apiVersion: '3.0', list: ['apiVersion'] }),
        _getAliases: () => ({}),
    }
}

describe('sandbox script completion', () => {
    beforeEach(() => {
        vi.stubGlobal('ImageBitmap', class {})
    })

    afterEach(() => {
        vi.restoreAllMocks()
        for (const frame of document.querySelectorAll('iframe')) frame.remove()
    })

    /** Invariant 24. */
    it('reports completion only after the calls the script made have answered', async () => {
        let release: (() => void) | null = null
        const pending = new Promise<string>((resolve) => {
            release = () => resolve('answered')
        })
        const settled = vi.fn()
        const slowCall = vi.fn(() => pending)
        const host = new SandboxHost({
            ...bridgeStubs(),
            slowCall,
        })
        host.onScriptSettled(settled)

        await runGuest(host, `window.answer = await risuai.slowCall()`)
        await new Promise((resolve) => setTimeout(resolve, 0))
        expect(slowCall).toHaveBeenCalledTimes(1)
        expect(settled).not.toHaveBeenCalled()

        release?.()
        await new Promise((resolve) => setTimeout(resolve, 0))
        expect(settled).toHaveBeenCalledTimes(1)
    })

    it('reports completion once even when the script throws', async () => {
        const settled = vi.fn()
        const host = new SandboxHost(bridgeStubs())
        host.onScriptSettled(settled)

        await runGuest(host, `throw new Error('plugin failed to start')`)
        await new Promise((resolve) => setTimeout(resolve, 0))
        expect(settled).toHaveBeenCalledTimes(1)
    })

    it('ignores malformed parent envelopes while a legitimate response still completes', async () => {
        let resolve!: (value: string) => void
        const host = new SandboxHost({
            ...bridgeStubs(),
            held: () => new Promise<string>(done => { resolve = done }),
        })
        const child = await runGuest(host, 'window.answer = await risuai.held()')
        const request = vi.mocked(child.parent.postMessage).mock.calls
            .map(call => call[0] as any).find(data => data.method === 'held')
        expect(request).toBeDefined()
        const childRealm = child as any
        for (const data of [
            null, [], 'RESPONSE',
            { type: 'RESPONSE', reqId: request.reqId, error: { message: 'forged' } },
            { type: 'RESPONSE', reqId: 1, result: 'forged' },
            { type: 'EXECUTE_CODE', reqId: 'malformed-code', code: 1 },
            { type: 'INVOKE_CALLBACK', reqId: 'malformed-callback', id: 'x', args: {} },
            { type: 'ABORT_SIGNAL', abortId: 1 },
        ]) {
            child.dispatchEvent(new childRealm.MessageEvent('message', { source: child.parent, data }))
        }
        await Promise.resolve()
        expect((child as any).answer).toBeUndefined()
        expect(vi.mocked(child.parent.postMessage).mock.calls.map(call => call[0]))
            .not.toEqual(expect.arrayContaining([expect.objectContaining({ type: 'EXEC_RESULT' })]))
        resolve('real')
        await vi.waitFor(() => expect((child as any).answer).toBe('real'))
        host.terminate()
    })

    it('assembles a streamed snapshot as a plain iframe-owned object', async () => {
        const host = new SandboxHost({
            ...bridgeStubs(),
            streamedSnapshot: () => ({
                __type: 'IFRAME_OBJECT_STREAM',
                value: new ReadableStream({
                    start(controller) {
                        controller.enqueue({ type: 'arrayStart', key: 'characters' })
                        controller.enqueue({
                            type: 'arrayPush',
                            key: 'characters',
                            value: { chaId: 'a', chats: [] },
                        })
                        controller.enqueue({ type: 'set', key: 'username', value: 'Fixture' })
                        controller.enqueue({ type: 'recordStart', key: 'pluginCustomStorage' })
                        controller.enqueue({
                            type: 'recordSet',
                            key: 'pluginCustomStorage',
                            entryKey: 'constructor',
                            value: 'constructor-value',
                        })
                        controller.enqueue({
                            type: 'recordSet',
                            key: 'pluginCustomStorage',
                            entryKey: '__proto__',
                            value: 'prototype-value',
                        })
                        controller.close()
                    },
                }),
            }),
        })

        const child = await runGuest(host, `
            window.snapshot = await risuai.streamedSnapshot();
            window.snapshotIsPlain = window.snapshot.constructor === Object;
            window.snapshot.characters[0].name = 'mutated';
        `)
        await vi.waitFor(() => expect((child as any).snapshot).toBeDefined())
        expect((child as any).snapshotIsPlain).toBe(true)
        const snapshot = (child as any).snapshot
        expect(snapshot.characters).toEqual([{ chaId: 'a', chats: [], name: 'mutated' }])
        expect(snapshot.username).toBe('Fixture')
        expect(snapshot.pluginCustomStorage.constructor).toBe('constructor-value')
        expect(Object.hasOwn(snapshot.pluginCustomStorage, '__proto__')).toBe(true)
        expect(snapshot.pluginCustomStorage.__proto__).toBe('prototype-value')
    })

    it('transports nested read tokens separately from product values in both directions', async () => {
        const registry = new PluginReadBaselines('synthetic-plugin', () => 1)
        const value = registry.track({ characters: [{ chaId: 'a', name: 'Character', chats: [{ id: 'chat', name: 'Before', message: [] }] }] }, 'database', 'database', 1)
        const write = vi.fn((chat: any) => registry.intent(chat, 'conversation', '["a","chat"]', {}))
        const host = new SandboxHost({ ...bridgeStubs(), getDatabase: () => value, setChatToIndex: (_characterIndex: number, _chatIndex: number, chat: any) => write(chat) })
        const child = await runGuest(host, `
            const database = await risuai.getDatabase();
            window.productKeys = Object.keys(database.characters[0].chats[0]);
            database.characters[0].chats[0].name = 'Edited';
            await risuai.setChatToIndex(0, 0, database.characters[0].chats[0]);
            window.finishedWrite = true;
        `)
        await vi.waitFor(() => expect((child as any).finishedWrite).toBe(true))
        expect(write.mock.results[0].value).toEqual([{ path: ['name'], type: 'set', value: 'Edited' }])
        expect((child as any).productKeys).toEqual(['id', 'name', 'message'])
        const sent = vi.mocked(child.parent.postMessage).mock.calls.map(call => call[0] as any).find(message => message.method === 'setChatToIndex')
        expect(sent.argProvenance[2]).toEqual([{ path: [], token: collectPluginReadProvenance(value.characters[0].chats[0])[0].token }])
        expect(sent.args[2]).not.toHaveProperty('token')
        host.terminate()
    })

    it('associates final streamed provenance with the selected character and nested chat', async () => {
        const registry = new PluginReadBaselines('synthetic-plugin', () => 1)
        const value = { characters: [registry.track({ chaId: 'a', name: 'Character', chats: [{ id: 'chat', name: 'Before', message: [] }] }, 'character', 'a', 1)] }
        const write = vi.fn((chat: any) => registry.intent(chat, 'conversation', '["a","chat"]', {}))
        const host = new SandboxHost({
            ...bridgeStubs(),
            getCharacter: () => ({ __type: 'IFRAME_OBJECT_STREAM', select: 'character', value: new ReadableStream({
                start(controller) {
                    controller.enqueue({ type: 'arrayStart', key: 'characters' })
                    controller.enqueue({ type: 'arrayPush', key: 'characters', value: value.characters[0] })
                    controller.enqueue({ type: 'provenance', entries: collectPluginReadProvenance(value) })
                    controller.close()
                },
            }) }),
            setChatToIndex: (_character: number, _chat: number, chat: any) => write(chat),
        })
        const child = await runGuest(host, `
            const character = await risuai.getCharacter();
            character.chats[0].name = 'Stream edit';
            await risuai.setChatToIndex(0, 0, character.chats[0]);
            window.finishedWrite = true;
        `)
        await vi.waitFor(() => expect((child as any).finishedWrite).toBe(true))
        expect(write.mock.results[0].value).toEqual([{ path: ['name'], type: 'set', value: 'Stream edit' }])
        host.terminate()
    })

    it('drains admitted storage mutations after termination and refuses late ingress', async () => {
        let finish!: () => void
        const pending = new Promise<void>(resolve => { finish = resolve })
        const mutation = vi.fn(() => pending)
        const host = new SandboxHost({ ...bridgeStubs(), _setSafeLocalStorage: mutation })
        const child = await runGuest(host, `void risuai._setSafeLocalStorage('synthetic', 'value')`)
        await vi.waitFor(() => expect(mutation).toHaveBeenCalledOnce())
        host.terminate()
        const drained = vi.fn()
        const draining = host.drainStorageMutations().then(drained)
        await Promise.resolve()
        expect(drained).not.toHaveBeenCalled()
        window.dispatchEvent(new MessageEvent('message', { source: child, data: { type: 'CALL_ROOT', reqId: 'late-write', method: '_setSafeLocalStorage', args: ['late', 'value'] } }))
        expect(mutation).toHaveBeenCalledOnce()
        finish()
        await draining
        expect(drained).toHaveBeenCalledOnce()
    })
})

describe('sandbox result lifetime and policy', () => {
    afterEach(() => {
        vi.restoreAllMocks()
        for (const frame of document.querySelectorAll('iframe')) frame.remove()
    })

    it('keeps the opaque sandbox and restrictive CSP', () => {
        const host = new SandboxHost({})
        const frame = document.createElement('iframe')
        document.body.append(frame)
        host.run(frame, '')
        const nonce = frame.srcdoc.match(/<script nonce="([^"]+)">/)![1]
        expect(frame.getAttribute('sandbox')).toBe('allow-scripts allow-modals allow-downloads')
        expect(frame.getAttribute('csp')).toBe("connect-src 'none'; script-src 'nonce-" + nonce + "' 'wasm-unsafe-eval'; frame-src 'none'; object-src 'none'; style-src * 'unsafe-inline'; default-src 'none'; img-src * data: blob:; font-src * data: blob:; media-src * data: blob:; base-uri 'none';")
        host.terminate()
    })

    it.each(['root', 'instance'])('disposes a late %s result without registering streams or remote refs', async (kind) => {
        let resolve!: (value: unknown) => void
        const call = vi.fn(() => new Promise(done => { resolve = done }))
        const host = new SandboxHost({ call })
        const frame = document.createElement('iframe')
        document.body.append(frame)
        host.run(frame, '')
        const internals = host as any
        internals.instanceRegistry.set('instance', { call })
        const post = vi.spyOn(frame.contentWindow!, 'postMessage')
        window.dispatchEvent(new MessageEvent('message', {
            source: frame.contentWindow,
            data: { type: kind === 'root' ? 'CALL_ROOT' : 'CALL_INSTANCE', id: 'instance', reqId: 'late', method: 'call', args: [] },
        }))
        expect(call).toHaveBeenCalledTimes(1)
        host.terminate()
        const cancel = vi.fn()
        resolve({ value: new ReadableStream({ cancel }), __classType: 'REMOTE_REQUIRED' })
        await vi.waitFor(() => expect(internals.pendingHostCalls).toBe(0))
        expect(cancel).toHaveBeenCalledTimes(1)
        expect(internals.activeStreamCleanups.size).toBe(0)
        expect(internals.instanceRegistry.size).toBe(0)
        expect(post).not.toHaveBeenCalled()
        host.terminate()
        expect(cancel).toHaveBeenCalledTimes(1)
    })

    it('cancels an undeliverable response body', async () => {
        let resolve!: (value: unknown) => void
        const host = new SandboxHost({ call: () => new Promise(done => { resolve = done }) })
        const frame = document.createElement('iframe')
        document.body.append(frame)
        host.run(frame, '')
        window.dispatchEvent(new MessageEvent('message', {
            source: frame.contentWindow,
            data: { type: 'CALL_ROOT', reqId: 'late', method: 'call', args: [] },
        }))
        const cancel = vi.fn()
        frame.remove()
        resolve(new Response(new ReadableStream({ cancel })))
        await vi.waitFor(() => expect((host as any).pendingHostCalls).toBe(0))
        expect(cancel).toHaveBeenCalledTimes(1)
        expect((host as any).activeStreamCleanups.size).toBe(0)
        host.terminate()
    })
})
