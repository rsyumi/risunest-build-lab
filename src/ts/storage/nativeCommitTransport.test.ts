import { describe, expect, it, vi } from 'vitest'
import {
    NativeCommitTransport,
    isLargeCommit,
    LARGE_COMMIT_BYTES,
    STAGED_REQUEST_BYTES,
    type CommitEnvelope,
    type CommitTransportDependencies,
    type SharedWebview,
} from './nativeCommitTransport'
import { MAX_NATIVE_REQUEST_BYTES, PayloadTooLargeError } from './nativePersistenceValue'

function fixture(large = true): CommitEnvelope {
    return {
        commit: {
            expectedRevision: 1,
            rootMutations: [
                {
                    type: 'set',
                    key: 'username',
                    value: large ? 'x'.repeat(LARGE_COMMIT_BYTES) : 'small',
                },
            ],
        },
        assetAliases: [],
    }
}
function harness(
    options: {
        windows?: boolean
        android?: boolean
        linux?: boolean
        ios?: boolean
        macos?: boolean
        unsupported?: boolean
        fail?: string | string[]
        ack?: number
    } = {},
) {
    let listener: Parameters<SharedWebview['addEventListener']>[1] | undefined
    const webview: SharedWebview = {
        addEventListener: vi.fn((_, fn) => {
            listener = fn
        }),
        removeEventListener: vi.fn(),
        releaseBuffer: vi.fn(),
    }
    const bytes = new Uint8Array([1, 2, 3, 4, 5])
    const received: number[] = []
    const buffer = new ArrayBuffer(3)
    const invoke = vi.fn(async (command: string, args: any) => {
        if (
            options.fail === command
            || (Array.isArray(options.fail) && options.fail.includes(command))
        ) throw new Error('native failed')
        if (command === 'pds_commit_android_open')
            return { capacity: 32 * 1024 }
        if (command === 'pds_commit_android_chunk')
            return args.offset + new TextEncoder().encode(args.chunk).length
        if (command === 'pds_commit_shared_open') {
            if (options.unsupported) return null
            listener!({
                additionalData: {
                    kind: 'pds-commit',
                    requestId: args.requestId,
                    id: 'session',
                },
                getBuffer: () => buffer,
            })
            return { id: 'session', capacity: buffer.byteLength }
        }
        if (command === 'pds_commit_shared_chunk') {
            received.push(...new Uint8Array(buffer).subarray(0, args.length))
            return options.ack ?? args.offset + args.length
        }
        return { revision: 2 }
    })
    const encode = vi.fn(async () => bytes)
    const transport = new NativeCommitTransport({
        windows: () => options.windows ?? true,
        android: () => options.android ?? false,
        linux: () => options.linux ?? false,
        ios: () => options.ios ?? false,
        macos: () => options.macos ?? false,
        invoke,
        encode,
        shared: () => webview,
    })
    return { transport, invoke, encode, webview, received }
}

describe('native commit transport', () => {
    it('replays only a typed pre-commit raw-body rejection and keeps JSON fallback', async () => {
        const input = fixture()
        const invoke = vi.fn(async (command: string) => {
            if (command === 'pds_commit_raw') throw { code: 'raw-body-unavailable' }
            return { revision: 2 }
        })
        const transport = new NativeCommitTransport({
            windows: () => false, linux: () => true, invoke: invoke as CommitTransportDependencies['invoke'],
            encode: async (value) => new TextEncoder().encode(JSON.stringify(value)),
            shared: () => undefined,
        })
        await transport.commit(input)
        await transport.commit(input)
        expect(invoke.mock.calls.map(([command]) => command)).toEqual(['pds_commit_raw', 'pds_commit', 'pds_commit'])
        expect(invoke).toHaveBeenLastCalledWith('pds_commit', input)
    })

    it('does not replay an ambiguous raw transport error', async () => {
        const invoke = vi.fn(async () => { throw new Error('response lost') })
        const transport = new NativeCommitTransport({
            windows: () => false, linux: () => true, invoke: invoke as CommitTransportDependencies['invoke'],
            encode: async (value) => new TextEncoder().encode(JSON.stringify(value)),
            shared: () => undefined,
        })
        await expect(transport.commit(fixture())).rejects.toThrow('response lost')
        expect(invoke).toHaveBeenCalledTimes(1)
    })

    it('sanitizes small native commits before invocation without mutating the caller', async () => {
        const h = harness({ windows: false })
        const input = fixture(false)
        input.commit.rootMutations = [{ type: 'set', key: 'text', value: '\ud800e\u0301' }]
        const diagnostic = vi.spyOn(console, 'warn').mockImplementation(() => {})
        await h.transport.commit(input)
        expect(h.invoke).toHaveBeenCalledWith('pds_commit', {
            ...input, commit: { ...input.commit, rootMutations: [{ type: 'set', key: 'text', value: '\ufffde\u0301' }] },
        })
        expect(input.commit.rootMutations[0]).toEqual({ type: 'set', key: 'text', value: '\ud800e\u0301' })
        diagnostic.mockRestore()
    })
    it.each(['linux', 'ios', 'macos'] as const)(
        'encodes %s large saves and submits raw bytes without touching shared buffers',
        async (os) => {
            const h = harness({ windows: false, [os]: true })
            await expect(h.transport.commit(fixture())).resolves.toEqual({
                revision: 2,
            })
            expect(h.encode).toHaveBeenCalledExactlyOnceWith(fixture())
            expect(h.invoke).toHaveBeenCalledExactlyOnceWith(
                'pds_commit_raw',
                new Uint8Array([1, 2, 3, 4, 5]),
            )
            expect(h.webview.addEventListener).not.toHaveBeenCalled()
        },
    )

    it.each(['linux', 'ios', 'macos'] as const)(
        'keeps small %s saves on JSON and only falls back before raw submission',
        async (os) => {
            const h = harness({ windows: false, [os]: true })
            await h.transport.commit(fixture(false))
            expect(h.encode).not.toHaveBeenCalled()
            expect(h.invoke).toHaveBeenCalledExactlyOnceWith(
                'pds_commit',
                fixture(false),
            )
            h.invoke.mockClear()
            h.encode.mockRejectedValueOnce(
                new DOMException('Cannot clone', 'DataCloneError'),
            )
            await h.transport.commit(fixture())
            expect(h.invoke).toHaveBeenCalledExactlyOnceWith(
                'pds_commit',
                fixture(),
            )
        },
    )

    it.each(['linux', 'ios', 'macos'] as const)(
        'does not replay an ambiguous %s raw commit and allows the next queued save',
        async (os) => {
            const h = harness({
                windows: false,
                [os]: true,
                fail: 'pds_commit_raw',
            })
            await expect(h.transport.commit(fixture())).rejects.toThrow(
                'native failed',
            )
            expect(h.invoke).toHaveBeenCalledTimes(1)
            expect(h.invoke.mock.calls[0][0]).toBe('pds_commit_raw')
            await expect(h.transport.commit(fixture(false))).resolves.toEqual({
                revision: 2,
            })
        },
    )

    it.each(['windows', 'android', 'linux', 'ios', 'macos'] as const)(
        'refuses a commit above the request limit on %s before sending anything',
        async (os) => {
            const h = harness({ windows: os === 'windows', [os]: true })
            h.encode.mockResolvedValueOnce(new Uint8Array(MAX_NATIVE_REQUEST_BYTES + 1))
            const refused = h.transport.commit(fixture())
            await expect(refused).rejects.toBeInstanceOf(PayloadTooLargeError)
            await expect(refused).rejects.toMatchObject({ code: 'payload-too-large', kind: 'commit', byteLength: MAX_NATIVE_REQUEST_BYTES + 1 })
            expect(h.invoke).not.toHaveBeenCalled()
            expect(h.webview.addEventListener).not.toHaveBeenCalled()
            await expect(h.transport.commit(fixture(false))).resolves.toBeDefined()
        },
    )

    it.each(['linux', 'ios', 'macos'] as const)('sends a commit at the request limit raw on %s', async (os) => {
        const h = harness({ windows: false, [os]: true })
        const bytes = new Uint8Array(MAX_NATIVE_REQUEST_BYTES)
        h.encode.mockResolvedValueOnce(bytes)
        await h.transport.commit(fixture())
        expect(h.invoke).toHaveBeenCalledTimes(1)
        expect(h.invoke.mock.calls[0][0]).toBe('pds_commit_raw')
        expect(h.invoke.mock.calls[0][1]).toBe(bytes)
    })

    it('falls back before submission for non-cloneable values, but propagates encoding failures', async () => {
        const h = harness()
        h.encode.mockRejectedValueOnce(new DOMException('Cannot clone', 'DataCloneError'))
        await h.transport.commit(fixture())
        expect(h.invoke).toHaveBeenCalledExactlyOnceWith('pds_commit', fixture())
        h.invoke.mockClear()
        h.encode.mockRejectedValueOnce(new Error('Invalid JSON value'))
        await expect(h.transport.commit(fixture())).rejects.toThrow('Invalid JSON value')
        expect(h.invoke).not.toHaveBeenCalled()
    })

    it.each(['releaseBuffer', 'removeEventListener'] as const)('preserves the confirmed commit when %s fails', async (cleanup) => {
        const h = harness()
        const report = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        vi.mocked(h.webview[cleanup]).mockImplementation(() => {
            throw new Error('Detached')
        })
        await expect(h.transport.commit(fixture())).resolves.toEqual({ revision: 2 })
        expect(report).toHaveBeenCalledWith(expect.stringContaining('cleanup failed'), expect.any(Error))
        report.mockRestore()
        const requestId = h.invoke.mock.calls.find(
            ([command]) => command === 'pds_commit_shared_open',
        )?.[1].requestId
        expect(h.invoke).toHaveBeenLastCalledWith('pds_commit_shared_cancel', {
            requestId,
        })
        expect(
            h.invoke.mock.calls.filter(([command]) => command === 'pds_commit_shared_finish'),
        ).toHaveLength(1)
    })

    it('bounds the routing probe for both strings and many small fields', () => {
        expect(isLargeCommit(fixture(false))).toBe(false)
        expect(isLargeCommit(fixture())).toBe(true)
        expect(isLargeCommit(Array.from({ length: 10_000 }, () => 1))).toBe(true)
    })
    it('keeps other platforms and small requests on ordinary invoke without encoding', async () => {
        for (const [windows, input] of [
            [false, fixture()],
            [true, fixture(false)],
        ] as const) {
            const h = harness({ windows })
            await h.transport.commit(input)
            expect(h.invoke).toHaveBeenCalledExactlyOnceWith('pds_commit', input)
            expect(h.encode).not.toHaveBeenCalled()
        }
    })
    it('routes large Android commits through the Worker and strings, retaining small JSON saves', async () => {
        const h = harness({ windows: false, android: true })
        await h.transport.commit(fixture(false))
        expect(h.encode).not.toHaveBeenCalled()
        h.invoke.mockClear()
        h.encode.mockResolvedValueOnce(new TextEncoder().encode('{"한글":"🐿️"}'))
        await h.transport.commit(fixture())
        expect(h.invoke.mock.calls.map(([command]) => command)).toEqual([
            'pds_commit_android_open',
            'pds_commit_android_chunk',
            'pds_commit_android_finish',
            'pds_commit_android_cancel',
        ])
        expect(h.webview.addEventListener).not.toHaveBeenCalled()
    })
    it('preserves Android clone fallback, errors and queue recovery', async () => {
        const h = harness({
            windows: false,
            android: true,
            fail: 'pds_commit_android_finish',
        })
        h.encode.mockRejectedValueOnce(new DOMException('Cannot clone', 'DataCloneError'))
        await h.transport.commit(fixture())
        h.encode.mockResolvedValueOnce(new TextEncoder().encode('{}'))
        await expect(h.transport.commit(fixture())).rejects.toThrow('native failed')
        await h.transport.commit(fixture(false))
        expect(h.invoke.mock.calls.filter(([cmd]) => cmd === 'pds_commit')).toHaveLength(2)
    })
    it('copies every byte once in ordered acknowledged chunks and releases both sides', async () => {
        const h = harness()
        expect(await h.transport.commit(fixture())).toEqual({ revision: 2 })
        expect(h.received).toEqual([1, 2, 3, 4, 5])
        expect(h.invoke.mock.calls.map(([command]) => command)).toEqual([
            'pds_commit_shared_open',
            'pds_commit_shared_chunk',
            'pds_commit_shared_chunk',
            'pds_commit_shared_finish',
            'pds_commit_shared_cancel',
        ])
        expect(h.webview.releaseBuffer).toHaveBeenCalledOnce()
        expect(h.webview.removeEventListener).toHaveBeenCalledOnce()
    })
    it('falls back to Worker raw only when shared support is unavailable before commit', async () => {
        const h = harness({ unsupported: true })
        await h.transport.commit(fixture())
        expect(h.invoke.mock.calls.map(([command]) => command)).toEqual([
            'pds_commit_shared_open',
            'pds_commit_raw',
            'pds_commit_shared_cancel',
        ])
    })
    it('cancels by the client request ID when the shared-open response is lost', async () => {
        const h = harness({ fail: 'pds_commit_shared_open' })

        await expect(h.transport.commit(fixture())).rejects.toThrow('native failed')

        const open = h.invoke.mock.calls.find(
            ([command]) => command === 'pds_commit_shared_open',
        )!
        expect(h.invoke).toHaveBeenLastCalledWith('pds_commit_shared_cancel', {
            requestId: open[1].requestId,
        })
    })
    it('reports cleanup failure without replacing a lost shared-open response', async () => {
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        const h = harness({
            fail: ['pds_commit_shared_open', 'pds_commit_shared_cancel'],
        })

        await expect(h.transport.commit(fixture())).rejects.toThrow('native failed')

        expect(consoleError).toHaveBeenCalledWith(
            'Persistence shared commit cleanup failed',
            expect.objectContaining({ message: 'native failed' }),
        )
        consoleError.mockRestore()
    })
    it.each(['pds_commit_shared_chunk', 'pds_commit_shared_finish'])(
        'never replays a failed %s and permits the next request',
        async (fail) => {
            const h = harness({ fail })
            await expect(h.transport.commit(fixture())).rejects.toThrow('native failed')
            expect(h.invoke.mock.calls.map(([command]) => command)).not.toContain('pds_commit_raw')
            expect(h.webview.releaseBuffer).toHaveBeenCalledOnce()
            const requestId = h.invoke.mock.calls.find(
                ([command]) => command === 'pds_commit_shared_open',
            )?.[1].requestId
            expect(h.invoke).toHaveBeenCalledWith('pds_commit_shared_cancel', {
                requestId,
            })
            await h.transport.commit(fixture(false))
            expect(h.invoke).toHaveBeenLastCalledWith('pds_commit', fixture(false))
        },
    )
    it('rejects an incorrect acknowledgement without submitting the commit', async () => {
        const h = harness({ ack: 100 })
        await expect(h.transport.commit(fixture())).rejects.toThrow('acknowledgement')
        expect(h.invoke.mock.calls.map(([command]) => command)).not.toContain(
            'pds_commit_shared_finish',
        )
    })
    it('serializes overlapping requests even while Worker encoding is pending', async () => {
        const h = harness()
        let release!: (bytes: Uint8Array<ArrayBuffer>) => void
        h.encode.mockImplementationOnce(
            () =>
                new Promise((resolve) => {
                    release = resolve
                }),
        )
        const first = h.transport.commit(fixture())
        const second = h.transport.commit(fixture(false))
        await Promise.resolve()
        expect(h.invoke).not.toHaveBeenCalled()
        release(new Uint8Array([1]))
        await Promise.all([first, second])
        expect(h.invoke.mock.calls.at(-1)?.[0]).toBe('pds_commit')
    })
})

describe('staged replace requests', () => {
    const head = '{"command":"pds_replace_put_root","args":'
    const request = (username: string) => ({ stagingId: 'staging-1', root: { username } })
    const overBudget = request('한글 🐿️'.repeat(Math.ceil(STAGED_REQUEST_BYTES / 14)))

    it('refuses a request above the native limit on every target before sending it', async () => {
        const large = request('x'.repeat(MAX_NATIVE_REQUEST_BYTES))
        for (const target of ['windows', 'android', 'linux', 'ios', 'macos'] as const) {
            const h = harness({ windows: false, [target]: true })
            const error = await h.transport.stage('pds_replace_put_root', 'root', large).catch((caught) => caught)
            expect(error).toBeInstanceOf(PayloadTooLargeError)
            expect(error).toMatchObject({ kind: 'root' })
            expect(h.invoke).not.toHaveBeenCalled()
        }
    })

    it('sends a request whose body is exactly at the limit', async () => {
        const fill = MAX_NATIVE_REQUEST_BYTES - head.length - JSON.stringify(request('')).length - 1
        const h = harness({ windows: false, linux: true })
        const exact = request('x'.repeat(fill))
        await h.transport.stage('pds_replace_put_root', 'root', exact)
        expect(h.invoke).toHaveBeenCalledTimes(1)
        expect(h.invoke.mock.calls[0][0]).toBe('pds_replace_put_root')
        expect(h.invoke.mock.calls[0][1]).toBe(exact)
        await expect(h.transport.stage('pds_replace_put_root', 'root', request('x'.repeat(fill + 1))))
            .rejects.toMatchObject({ code: 'payload-too-large', byteLength: MAX_NATIVE_REQUEST_BYTES + 1 })
        expect(h.invoke).toHaveBeenCalledTimes(1)
    })

    it('sends only an Android request above the ordinary budget in chunks to the replace finish', async () => {
        const windows = harness()
        await windows.transport.stage('pds_replace_put_root', 'root', overBudget)
        expect(windows.invoke.mock.calls).toEqual([['pds_replace_put_root', overBudget]])

        const h = harness({ windows: false, android: true })
        const small = request('small')
        await h.transport.stage('pds_replace_put_root', 'root', small)
        expect(h.invoke.mock.calls).toEqual([['pds_replace_put_root', small]])
        h.invoke.mockClear()
        await h.transport.stage('pds_replace_put_root', 'root', overBudget)
        const commands = h.invoke.mock.calls.map(([command]) => command)
        expect(commands[0]).toBe('pds_commit_android_open')
        expect(commands.slice(-2)).toEqual(['pds_replace_android_finish', 'pds_commit_android_cancel'])
        const chunks = h.invoke.mock.calls.filter(([command]) => command === 'pds_commit_android_chunk')
        expect(chunks.length).toBeGreaterThan(1)
        expect(JSON.parse(chunks.map(([, args]) => args.chunk).join(''))).toEqual({
            command: 'pds_replace_put_root',
            args: overBudget,
        })
        const id = h.invoke.mock.calls[0][1].id
        expect(h.invoke.mock.calls.at(-2)).toEqual(['pds_replace_android_finish', { id }])
    })

    it('sends Android chunked requests in turn with commits', async () => {
        const h = harness({ windows: false, android: true })
        let release!: (bytes: Uint8Array<ArrayBuffer>) => void
        h.encode.mockImplementationOnce(() => new Promise((resolve) => { release = resolve }))
        const commit = h.transport.commit(fixture())
        const staged = h.transport.stage('pds_replace_put_root', 'root', overBudget)
        await new Promise((resolve) => setTimeout(resolve, 0))
        expect(h.invoke).not.toHaveBeenCalled()
        release(new TextEncoder().encode('{}'))
        await Promise.all([commit, staged])
        const commands = h.invoke.mock.calls.map(([command]) => command)
        expect(commands.slice(0, 5)).toEqual([
            'pds_commit_android_open',
            'pds_commit_android_chunk',
            'pds_commit_android_finish',
            'pds_commit_android_cancel',
            'pds_commit_android_open',
        ])
        expect(commands.at(-2)).toBe('pds_replace_android_finish')
    })
})
