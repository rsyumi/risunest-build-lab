import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it, vi } from 'vitest'

vi.mock('../alert', () => ({ alertConfirm: vi.fn() }))
vi.mock('./persistentDataRuntime.svelte', () => ({ appendPersistentRootModule: vi.fn() }))

import type {
    PreparedNativeContent,
    PreparedNativeContentActivationLifecycle,
    PreparedNativeModuleMapping,
} from './nativeFileJobs'
import {
    activatePreparedNativeModuleContent,
    type PreparedRootModuleAppend,
} from './nativeModuleContentActivation'
import { runNativePreparedContentRoute } from './nativePreparedContentRoute'
import { PersistentRootModuleAppendRejectedError } from './saveCoordinator'
import { canonicalClone } from './saveCoordinatorHelpers'
import { StdioModuleImportError } from '../process/mcp/moduleImport'

const hash = (byte: string) => byte.repeat(64)

function risumContent(assets: unknown = [
    ['same', '', 'PNG'],
    ['same', '', 'bin'],
]): PreparedNativeContent {
    return {
        casSessionId: 'content-job',
        format: 'risu-module',
        metadata: {
            id: 'source-id',
            name: 'Imported module',
            lowLevelAccess: true,
            unknownFutureField: { retained: true },
            ...(assets === undefined ? {} : { assets }),
        },
        assets: assets === undefined ? [] : [
            {
                position: 0,
                logicalId: `assets/${hash('a')}.PNG`,
                objectHash: hash('a'),
                byteSize: 4,
                mime: '',
                name: '',
                ext: 'PNG',
            },
            {
                position: 1,
                logicalId: `assets/${hash('b')}.bin`,
                objectHash: hash('b'),
                byteSize: 0,
                mime: '',
                name: '',
                ext: 'bin',
            },
        ],
        ownerHead: assets === undefined
            ? { present: false, manifestHash: null, entryCount: 0 }
            : { present: true, manifestHash: hash('c'), entryCount: 2 },
    }
}

describe('native RISUM module activation', () => {
    it.each([false, true, undefined])('rejects stdio before confirmation, sealing or persistence with lowLevelAccess=%s', async lowLevelAccess => {
        const content = risumContent()
        content.metadata.lowLevelAccess = lowLevelAccess
        content.metadata.mcp = { url: 'stdio:{"command":"node","args":["synthetic.js"]}' }
        const receipt = {
            jobId: 'synthetic-job', warningCodes: [],
            content,
            prepareOwnerManifestAndSeal: vi.fn(), sealPreparedContent: vi.fn(),
            cancel: vi.fn(), confirmActivated: vi.fn(),
        }
        const dependencies = { confirmLowLevelAccess: vi.fn(), createId: vi.fn(), append: vi.fn() }
        await expect(runNativePreparedContentRoute(
            { type: 'desktopPath', path: 'C:/synthetic/module.risum' }, 'module.risum',
            {
                prepare: async () => receipt,
                map: async content => content,
                activate: (content, lifecycle) => activatePreparedNativeModuleContent(content, lifecycle, dependencies),
            },
        )).rejects.toBeInstanceOf(StdioModuleImportError)
        expect(dependencies.confirmLowLevelAccess).not.toHaveBeenCalled()
        expect(dependencies.append).not.toHaveBeenCalled()
        expect(receipt.sealPreparedContent).not.toHaveBeenCalled()
        expect(receipt.prepareOwnerManifestAndSeal).not.toHaveBeenCalled()
        expect(receipt.cancel).toHaveBeenCalledOnce()
        expect(receipt.confirmActivated).not.toHaveBeenCalled()
    })

    it.each(['https://synthetic.invalid/mcp', 'internal:dice', 'plugin:synthetic'])('preserves %s on native import', async url => {
        const content = risumContent()
        content.metadata.mcp = { url }
        const append = vi.fn()
        await activatePreparedNativeModuleContent(content, {
            prepareOwnerManifestAndSeal: vi.fn(), sealPreparedContent: vi.fn(),
        }, { confirmLowLevelAccess: async () => true, createId: () => 'new-id', append })
        expect(append).toHaveBeenCalledWith(expect.objectContaining({
            module: expect.objectContaining({ mcp: { url } }),
        }), undefined)
    })

    it('confirms low-level access, seals native roots, then atomically appends full metadata', async () => {
        const events: string[] = []
        const append = vi.fn(async () => { events.push('append') })
        const lifecycle: PreparedNativeContentActivationLifecycle = {
            prepareOwnerManifestAndSeal: vi.fn(),
            sealPreparedContent: vi.fn(async () => { events.push('seal') }),
        }

        const result = await activatePreparedNativeModuleContent(
            risumContent(),
            lifecycle,
            {
                confirmLowLevelAccess: vi.fn(async () => {
                    events.push('confirm')
                    return true
                }),
                createId: () => 'new-module-id',
                append,
            },
        )

        expect(events).toEqual(['confirm', 'seal', 'append'])
        expect(append).toHaveBeenCalledWith({
            module: {
                id: 'new-module-id',
                name: 'Imported module',
                lowLevelAccess: true,
                unknownFutureField: { retained: true },
                assets: [
                    ['same', `assets/${hash('a')}.PNG`, 'PNG'],
                    ['same', `assets/${hash('b')}.bin`, 'bin'],
                ],
            },
            assetAliases: [
                {
                    kind: 'asset',
                    key: `assets/${hash('a')}.PNG`,
                    objectHash: hash('a'),
                    size: 4,
                    mime: '',
                    name: '',
                    ext: 'PNG',
                },
                {
                    kind: 'asset',
                    key: `assets/${hash('b')}.bin`,
                    objectHash: hash('b'),
                    size: 0,
                    mime: '',
                    name: '',
                    ext: 'bin',
                },
            ],
            ownerHead: {
                present: true,
                manifestHash: hash('c'),
                entryCount: 2,
            },
        }, undefined)
        expect(result).toEqual({ moduleId: 'new-module-id' })
    })

    it('preserves tuple trailing fields and deduplicates identical logical aliases', async () => {
        const content = risumContent([
            ['same', '', '.unsafe/path', { future: true }],
            ['same', '', '.unsafe/path', 'tail'],
        ])
        content.assets[0] = {
            ...content.assets[0],
            logicalId: `assets/${hash('a')}.bin`,
            ext: '.unsafe/path',
        }
        content.assets[1] = {
            ...content.assets[0],
            position: 1,
        }
        const append = vi.fn(async (_input: PreparedRootModuleAppend) => undefined)

        await activatePreparedNativeModuleContent(content, {
            prepareOwnerManifestAndSeal: vi.fn(),
            sealPreparedContent: vi.fn(async () => undefined),
        }, {
            confirmLowLevelAccess: vi.fn(async () => true),
            createId: () => 'new-id',
            append,
        })

        const input = append.mock.calls[0][0]
        expect(input.module.assets).toEqual([
            ['same', `assets/${hash('a')}.bin`, '.unsafe/path', { future: true }],
            ['same', `assets/${hash('a')}.bin`, '.unsafe/path', 'tail'],
        ])
        expect(input.assetAliases).toEqual([{
            kind: 'asset',
            key: `assets/${hash('a')}.bin`,
            objectHash: hash('a'),
            size: 4,
            mime: '',
            name: '',
            ext: '.unsafe/path',
        }])
    })

    it('preserves an absent assets property and does not seal after rejected confirmation', async () => {
        const lifecycle: PreparedNativeContentActivationLifecycle = {
            prepareOwnerManifestAndSeal: vi.fn(),
            sealPreparedContent: vi.fn(),
        }
        const append = vi.fn()

        await expect(activatePreparedNativeModuleContent(
            risumContent(undefined),
            lifecycle,
            {
                confirmLowLevelAccess: vi.fn(async () => false),
                createId: () => 'unused',
                append,
            },
        )).resolves.toBeNull()

        expect(lifecycle.sealPreparedContent).not.toHaveBeenCalled()
        expect(append).not.toHaveBeenCalled()
    })

    it('releases sealed roots and does not append when cancellation arrives during sealing', async () => {
        const controller = new AbortController()
        const reason = new DOMException('cancelled during sealing', 'AbortError')
        const cleanupError = new Error('native cleanup unavailable')
        const append = vi.fn()
        const lifecycle: PreparedNativeContentActivationLifecycle = {
            prepareOwnerManifestAndSeal: vi.fn(),
            sealPreparedContent: vi.fn(async () => controller.abort(reason)),
            abortPreparedContent: vi.fn(async () => { throw cleanupError }),
        }

        await expect(activatePreparedNativeModuleContent(
            risumContent(undefined),
            lifecycle,
            {
                confirmLowLevelAccess: vi.fn(async () => true),
                createId: () => 'unused',
                append,
            },
            controller.signal,
        )).rejects.toBe(reason)

        expect(lifecycle.abortPreparedContent).toHaveBeenCalledOnce()
        expect(append).not.toHaveBeenCalled()
    })

    it('threads cancellation into the persistent append and releases sealed roots', async () => {
        const controller = new AbortController()
        const reason = new DOMException('cancelled during alias read', 'AbortError')
        const lifecycle: PreparedNativeContentActivationLifecycle = {
            prepareOwnerManifestAndSeal: vi.fn(),
            sealPreparedContent: vi.fn(async () => undefined),
            abortPreparedContent: vi.fn(async () => undefined),
        }
        const append = vi.fn(async (_input: PreparedRootModuleAppend, signal?: AbortSignal) => {
            expect(signal).toBe(controller.signal)
            controller.abort(reason)
            signal?.throwIfAborted()
        })

        await expect(activatePreparedNativeModuleContent(
            risumContent(undefined),
            lifecycle,
            {
                confirmLowLevelAccess: vi.fn(async () => true),
                createId: () => 'new-id',
                append,
            },
            controller.signal,
        )).rejects.toBe(reason)

        expect(lifecycle.sealPreparedContent).toHaveBeenCalledOnce()
        expect(lifecycle.abortPreparedContent).toHaveBeenCalledOnce()
    })

    it('aborts the sealed native session when the atomic revision commit loses its CAS', async () => {
        const rejection = new PersistentRootModuleAppendRejectedError('revision conflict')
        const receipt = {
            jobId: 'content-job',
            content: risumContent(undefined),
            warningCodes: [],
            prepareOwnerManifestAndSeal: vi.fn(),
            sealPreparedContent: vi.fn(async () => undefined),
            abortPreparedContent: vi.fn(async () => { throw new Error('native cleanup unavailable') }),
            confirmActivated: vi.fn(async () => undefined),
            cancel: vi.fn(async () => undefined),
        }

        await expect(runNativePreparedContentRoute(
            { type: 'desktopPath', path: 'C:\\chosen\\module.risum' },
            'module.risum',
            {
                prepare: vi.fn(async () => receipt),
                map: async (content) => content,
                activate: (content, lifecycle) => activatePreparedNativeModuleContent(
                    content,
                    lifecycle,
                    {
                        confirmLowLevelAccess: vi.fn(async () => true),
                        createId: () => 'new-id',
                        append: vi.fn(async () => {
                            throw rejection
                        }),
                    },
                ),
            },
        )).rejects.toBe(rejection)

        expect(receipt.sealPreparedContent).toHaveBeenCalledOnce()
        expect(receipt.abortPreparedContent).toHaveBeenCalledOnce()
        expect(receipt.cancel).toHaveBeenCalledOnce()
        expect(receipt.confirmActivated).not.toHaveBeenCalled()
    })

    it('retains sealed roots when the append response is ambiguous', async () => {
        const lifecycle: PreparedNativeContentActivationLifecycle = {
            prepareOwnerManifestAndSeal: vi.fn(),
            sealPreparedContent: vi.fn(async () => undefined),
            abortPreparedContent: vi.fn(async () => undefined),
        }

        await expect(activatePreparedNativeModuleContent(
            risumContent(undefined),
            lifecycle,
            {
                confirmLowLevelAccess: vi.fn(async () => true),
                createId: () => 'new-id',
                append: vi.fn(async () => { throw new Error('commit response lost') }),
            },
        )).rejects.toThrow('commit response lost')

        expect(lifecycle.abortPreparedContent).not.toHaveBeenCalled()
    })
})

// The RISUM receipts of the import wiring oracle with what the TS activation appended for each, recorded by
// benchmarks/rust-core-oracles/formats-wiring.oracle.test.ts. `native` is the mapping the Rust receipt carries.
type WiringCase = {
    id: string
    receipt: PreparedNativeContent
    native: 'mapped' | { reason: 'unsupported' | 'invalid'; name: string }
    result: { mapped?: string; stored?: string; aliases?: unknown; ownerHead?: unknown; error?: string; confirms: number }
}
const fixture = (name: string) => JSON.parse(readFileSync(resolve(process.cwd(), `tests/fixtures/rust-core/formats/${name}.json`), 'utf8'))
const wiringResults = new Map((fixture('wiring.expected').cases as { id: string; result: WiringCase['result'] }[]).map(({ id, result }) => [id, result]))
const wiringCases = (fixture('wiring.cases').cases as Omit<WiringCase, 'result'>[])
    .filter((entry) => entry.receipt.format === 'risu-module')
    .map((entry) => ({ ...entry, result: wiringResults.get(entry.id)! }))

// The module record, aliases and owner head the append keeps, as the oracle records them, beside the dependencies
// the TS steps call: the confirmation answers yes and the module id is the oracle's first draw.
function activateReceipt(entry: WiringCase, nativeMapping: PreparedNativeModuleMapping) {
    let draws = 0
    const kept: Pick<WiringCase['result'], 'stored' | 'aliases' | 'ownerHead'> = {}
    const dependencies = {
        confirmLowLevelAccess: vi.fn(async () => true),
        createId: vi.fn(() => `00000000-0000-4000-8000-${String(++draws).padStart(12, '0')}`),
        append: vi.fn(async (input: PreparedRootModuleAppend) => {
            kept.stored = JSON.stringify(canonicalClone(input.module))
            kept.aliases = canonicalClone(input.assetAliases)
            kept.ownerHead = canonicalClone(input.ownerHead)
        }),
    }
    const result = activatePreparedNativeModuleContent(
        { ...structuredClone(entry.receipt), nativeMapping },
        { prepareOwnerManifestAndSeal: vi.fn(), sealPreparedContent: vi.fn(async () => undefined) },
        dependencies,
    )
    return { result, kept, dependencies }
}

describe('native RISUM mapping', () => {
    const named = <Entry extends WiringCase>(entries: Entry[]) => entries.map((entry) => [entry.id, entry] as const)
    const mapped = wiringCases.filter((entry) => entry.native === 'mapped')
    const declined = wiringCases.flatMap((entry) => entry.native === 'mapped' ? [] : [{ ...entry, native: entry.native }])
    const declinedAndStored = declined.filter((entry) => entry.result.stored !== undefined)
    const declinedAndFailed = declined.filter((entry) => entry.result.error !== undefined)
    const recorded = ({ result }: WiringCase) => ({ stored: result.stored, aliases: result.aliases, ownerHead: result.ownerHead })

    it('replays every RISUM wiring receipt', () => {
        expect([mapped.length, declinedAndStored.length, declinedAndFailed.length]).toEqual([7, 2, 6])
    })

    // With the Rust receipt test, which proves the native mapping is the text the TS steps stringify to, this is the
    // same module through both paths.
    it.each(named(mapped))('appends the native mapping of %s exactly as the TS steps append their own', async (_id, entry) => {
        const module = JSON.parse(entry.result.mapped!) as Record<string, unknown>
        const { result, kept, dependencies } = activateReceipt(entry, { outcome: 'mapped', module })

        await expect(result).resolves.toEqual({ moduleId: module.id })
        expect(kept).toEqual(recorded(entry))
        expect(dependencies.createId).not.toHaveBeenCalled()
        expect(dependencies.confirmLowLevelAccess).not.toHaveBeenCalled()
    })

    it.each(named(declinedAndStored))('runs the TS steps for %s where the native mapping declines it', async (_id, entry) => {
        const { result, kept, dependencies } = activateReceipt(entry, { outcome: 'declined', ...entry.native })

        await expect(result).resolves.toEqual({ moduleId: '00000000-0000-4000-8000-000000000001' })
        expect(kept).toEqual(recorded(entry))
        expect(dependencies.confirmLowLevelAccess).toHaveBeenCalledTimes(entry.result.confirms)
    })

    it.each([
        ['limit', 'moduleBytes'],
        ['cancelled', ''],
        ['host', 'stack'],
        ['failed', 'panic'],
        ['invalid', 'assetMetadata'],
    ] as const)('runs the TS steps for a module the native mapping declines as %s %s', async (reason, name) => {
        const entry = mapped[0]
        const { result, kept, dependencies } = activateReceipt(entry, { outcome: 'declined', reason, name })

        await expect(result).resolves.toEqual({ moduleId: '00000000-0000-4000-8000-000000000001' })
        expect(kept).toEqual(recorded(entry))
        expect(dependencies.createId).toHaveBeenCalledOnce()
    })

    it.each(named(declinedAndFailed))('rejects %s with the exact TS error where the native mapping declines it', async (_id, entry) => {
        const { result, dependencies } = activateReceipt(entry, { outcome: 'declined', ...entry.native })

        await expect(result).rejects.toHaveProperty('message', entry.result.error)
        expect(dependencies.append).not.toHaveBeenCalled()
        expect(dependencies.confirmLowLevelAccess).not.toHaveBeenCalled()
    })
})
