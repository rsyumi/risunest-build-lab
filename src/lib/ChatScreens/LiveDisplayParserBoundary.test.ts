import {afterEach, beforeEach, expect, test, vi} from 'vitest'
import {createRawSnippet, mount, tick, unmount} from 'svelte'
import LiveDisplayParserBoundary from './LiveDisplayParserBoundary.svelte'

const state = vi.hoisted(() => ({
    acquire: vi.fn(),
    selection: 'synthetic-selection',
    listeners: new Set<(identity: string) => void>(),
}))
vi.mock('src/ts/liveDisplayParserLease', () => ({
    acquireLiveDisplayParserLease: (...args: unknown[]) => state.acquire(...args),
    captureLiveDisplayParserInputs: (source: unknown) => ({source}),
    captureLiveDisplayParserSelection: () => state.selection,
    subscribeLiveDisplayParserSelection: (listener: (identity: string) => void) => {
        state.listeners.add(listener)
        listener(state.selection)
        return () => state.listeners.delete(listener)
    },
}))
vi.mock('src/ts/storage/database.svelte', () => ({
    getCurrentCharacter: () => ({chaId: 'synthetic-character'}),
    getCurrentChat: () => ({id: 'synthetic-chat'}),
}))

let host: HTMLDivElement
let component: ReturnType<typeof mount> | undefined
beforeEach(() => {
    state.acquire.mockReset()
    state.selection = 'synthetic-selection'
    host = document.createElement('div')
    document.body.append(host)
})
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    host.remove()
    expect(state.listeners.size).toBe(0)
})
function start() {
    component = mount(LiveDisplayParserBoundary, {
        target: host,
        props: {
            source: '<style>.synthetic { color: red }</style>',
            character: null,
            children: createRawSnippet<[AbortSignal]>(() => ({
                render: () => '<div data-synthetic-ready><style>.synthetic { color: red }</style></div>',
            })),
        },
    })
}
function notifySelection() {
    for (const listener of state.listeners) listener(state.selection)
}

test('recovers failed background styles when the same conversation becomes ready', async () => {
    const release = vi.fn()
    state.acquire.mockRejectedValueOnce(new Error('Synthetic preparation failure'))
        .mockResolvedValueOnce({release})
    start()
    await vi.waitFor(() => expect(host.querySelector('[data-live-display-load-error]')).not.toBeNull())
    expect(host.querySelector('style')).toBeNull()
    notifySelection()
    await vi.waitFor(() => expect(host.querySelector('[data-synthetic-ready] style')).not.toBeNull())
    expect(host.querySelector('[data-live-display-load-error]')).toBeNull()
    expect(state.acquire).toHaveBeenCalledTimes(2)
    expect(release).not.toHaveBeenCalled()
    notifySelection()
    await tick()
    expect(state.acquire).toHaveBeenCalledTimes(2)
    await unmount(component!); component = undefined
    expect(release).toHaveBeenCalledOnce()
})

test('does not miss readiness published before the pending attempt rejects', async () => {
    let reject!: (error: Error) => void
    state.acquire.mockImplementationOnce(() => new Promise((_resolve, fail) => {reject = fail}))
        .mockResolvedValueOnce(null)
    start()
    await vi.waitFor(() => expect(state.acquire).toHaveBeenCalledOnce())
    notifySelection()
    reject(new Error('Synthetic outdated preparation'))
    await vi.waitFor(() => expect(host.querySelector('[data-synthetic-ready]')).not.toBeNull())
    expect(state.acquire).toHaveBeenCalledTimes(2)
})

test('retains a real failure until state changes or the user retries', async () => {
    state.acquire.mockRejectedValue(new Error('Synthetic storage failure'))
    start()
    await vi.waitFor(() => expect(host.querySelector('[data-live-display-load-error]')).not.toBeNull())
    await tick()
    expect(state.acquire).toHaveBeenCalledOnce()
    state.acquire.mockResolvedValueOnce(null)
    host.querySelector<HTMLButtonElement>('[data-live-display-load-error] button')!.click()
    await vi.waitFor(() => expect(host.querySelector('[data-synthetic-ready]')).not.toBeNull())
    expect(state.acquire).toHaveBeenCalledTimes(2)
})

test('releases a late lease after navigation without publishing the old content', async () => {
    const release = vi.fn()
    let resolve!: (value: {release(): void}) => void
    state.acquire.mockImplementationOnce(() => new Promise(done => {resolve = done}))
        .mockRejectedValueOnce(new Error('Synthetic new conversation failure'))
    start()
    await vi.waitFor(() => expect(state.acquire).toHaveBeenCalledOnce())
    const signal = state.acquire.mock.calls[0][0].signal as AbortSignal
    state.selection = 'another-synthetic-selection'
    notifySelection()
    resolve({release})
    await vi.waitFor(() => expect(state.acquire).toHaveBeenCalledTimes(2))
    expect(signal.aborted).toBe(true)
    expect(release).toHaveBeenCalledOnce()
    expect(host.querySelector('[data-synthetic-ready]')).toBeNull()
})
