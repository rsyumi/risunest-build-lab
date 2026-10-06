import { expect, test, vi } from 'vitest'
import { createLiveDisplayParserLeaseAcquirer } from './liveDisplayParserLease'
import { getDatabase, getCurrentCharacter, getCurrentChat } from './storage/database.svelte'
import { getModuleRegexScripts, getModuleTriggers } from './process/modules'

vi.mock('./storage/database.svelte', () => ({
    getDatabase: vi.fn(),
    getCurrentCharacter: vi.fn(),
    getCurrentChat: vi.fn(),
}))
vi.mock('./storage/persistentDataRuntime.svelte', () => ({ getPersistentDataRuntime: vi.fn() }))
vi.mock('./process/modules', () => ({ getModuleRegexScripts: vi.fn(), getModuleTriggers: vi.fn() }))
vi.mock('./plugins/plugins.svelte', () => ({ pluginV2: { editdisplay: new Set() } }))
vi.mock('./util', () => ({ findCharacterbyId: vi.fn(), getPersonaPrompt: vi.fn() }))

function fixture() {
    let selected = {
        characterId: 'owner',
        conversationId: 'chat',
        navigationGeneration: 1,
        storeRevision: 1,
    }
    const release = vi.fn()
    const acquireCompleteConversation = vi.fn(async () => ({ release, target: selected }))
    const runtime = {
        captureSelectedConversationTarget: () => selected,
        acquireCompleteConversation,
    }
    const acquire = createLiveDisplayParserLeaseAcquirer({
        runtime: () => runtime as never,
        classify: (source) => ({ source }),
    })
    return {
        acquire,
        release,
        runtime,
        navigate: () => {
            selected = { ...selected, conversationId: 'other', navigationGeneration: 2 }
        },
    }
}

test('keeps plain live display windowed without acquiring history', async () => {
    const f = fixture()
    await expect(
        f.acquire({
            source: '<b>plain</b>',
            character: null,
            signal: new AbortController().signal,
        }),
    ).resolves.toBeNull()
    expect(f.runtime.acquireCompleteConversation).not.toHaveBeenCalled()
})

test('holds a history lease until explicitly released and releases once', async () => {
    const f = fixture()
    const lease = await f.acquire({
        source: '{{history}}',
        character: null,
        signal: new AbortController().signal,
    })
    expect(f.release).not.toHaveBeenCalled()
    lease!.release()
    lease!.release()
    expect(f.release).toHaveBeenCalledTimes(1)
})

test.each(['abort', 'navigate'])('releases a late history lease after %s', async (action) => {
    const f = fixture()
    let resolve!: () => void
    f.runtime.acquireCompleteConversation.mockImplementationOnce(
        () =>
            new Promise((done) => {
                resolve = () =>
                    done({
                        release: f.release,
                        target: f.runtime.captureSelectedConversationTarget(),
                    })
            }),
    )
    const controller = new AbortController()
    const pending = f.acquire({ source: '{{history}}', character: null, signal: controller.signal })
    if (action === 'abort') controller.abort()
    else f.navigate()
    resolve()
    await expect(pending).rejects.toBeInstanceOf(Error)
    expect(f.release).toHaveBeenCalledTimes(1)
})
