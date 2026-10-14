// @vitest-environment node

import { beforeAll, expect, test, vi } from 'vitest'

const runtimeState = vi.hoisted(() => ({
    createLuaFactory: vi.fn(),
}))

vi.mock('../parser/parser.svelte', async () => (await import('./tests/scriptingsTestHarness')).parserModule())
vi.mock('../alert', async () => (await import('./tests/scriptingsTestHarness')).alertModule())
vi.mock('../globalApi.svelte', () => ({ fetchNative: vi.fn(), readImage: vi.fn() }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'tauri', os: 'android' }))
vi.mock('../tokenizer', async () => (await import('./tests/scriptingsTestHarness')).tokenizerModule())
vi.mock('../util', async () => (await import('./tests/scriptingsTestHarness')).utilModule())
vi.mock('../storage/database.svelte', async () => (await import('./tests/scriptingsTestHarness')).databaseModule())
vi.mock('../stores.svelte', async () => (await import('./tests/scriptingsTestHarness')).storesModule())
vi.mock('./modules', async () => (await import('./tests/scriptingsTestHarness')).modulesModule())
vi.mock('./files/inlays', async () => (await import('./tests/scriptingsTestHarness')).inlaysModule())
vi.mock('./lorebook.svelte', async () => (await import('./tests/scriptingsTestHarness')).lorebookModule())
vi.mock('./memory/hypamemory', async () => (await import('./tests/scriptingsTestHarness')).hypamemoryModule())
vi.mock('./request/request', async () => (await import('./tests/scriptingsTestHarness')).requestModule())
vi.mock('./stableDiff', async () => (await import('./tests/scriptingsTestHarness')).stableDiffModule())
vi.mock('./luaRuntime', () => ({
    createLuaFactory: () => runtimeState.createLuaFactory(),
    runLuaSource: vi.fn(async (engine, source) => engine.doString(source)),
}))

let runScripted: typeof import('./scriptings').runScripted

beforeAll(async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('', { status: 200 })))
    runScripted = (await import('./scriptings')).runScripted
})

test('rethrows a shared factory failure to concurrent callers and retries later', async () => {
    const failure = new Error('factory initialization failed')
    let rejectFactory!: (error: Error) => void
    const failedFactory = new Promise<never>((_resolve, reject) => {
        rejectFactory = reject
    })
    runtimeState.createLuaFactory.mockReturnValueOnce(failedFactory)

    const arg = {
        char: { chaId: 'factory-failure' } as never,
        chat: { message: [] } as never,
        mode: 'factory-failure',
    }
    const first = runScripted('', arg)
    const concurrent = runScripted('', arg)
    const failedResultsPromise = Promise.allSettled([first, concurrent])
    await vi.waitFor(() => expect(runtimeState.createLuaFactory).toHaveBeenCalledTimes(1))
    rejectFactory(failure)

    const failedResults = await failedResultsPromise
    expect(failedResults).toEqual([
        { status: 'rejected', reason: failure },
        { status: 'rejected', reason: failure },
    ])
    expect(runtimeState.createLuaFactory).toHaveBeenCalledTimes(1)

    runtimeState.createLuaFactory.mockResolvedValueOnce({
        mountFile: vi.fn(),
        createEngine: vi.fn(async () => ({
            global: { close: vi.fn(), get: vi.fn(), set: vi.fn() },
            doString: vi.fn(),
        })),
    })

    await expect(runScripted('', arg)).resolves.toEqual(
        expect.objectContaining({ stopSending: false }),
    )
    expect(runtimeState.createLuaFactory).toHaveBeenCalledTimes(2)
})
