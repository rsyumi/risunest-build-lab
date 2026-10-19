// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    os: 'android' as 'android' | 'ios' | 'windows',
    keepAlive: true,
    ios: undefined as unknown,
}))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule(() => ({ runtime: 'tauri', os: mocks.os })))
vi.mock('../mobileBackgroundTask', async () => (await import('../mobileBackgroundTask.testSupport')).mobileBackgroundTaskModule())
vi.mock('../storage/deviceSettings', async importOriginal => ({
    ...await importOriginal<typeof import('../storage/deviceSettings')>(),
    getDeviceSettings: () => ({ androidKeepAliveDuringGeneration: mocks.keepAlive }),
}))
vi.mock('../iosNative', async importOriginal => ({
    ...await importOriginal<typeof import('../iosNative')>(),
    beginIOSGeneration: vi.fn(async () => mocks.ios),
}))

import { beginDeferredMobileBackgroundTask, beginMobileBackgroundTask } from '../mobileBackgroundTask'
import { BackgroundExecutionExpiredError } from '../iosNative'
import { beginGenerationBackground, holdModelCall } from './generationBackground'
import { reserveGeneration } from './generationState'

function hold(signal?: AbortSignal) {
    return { signal, progress: vi.fn(), dispose: vi.fn(async (_success?: boolean) => {}) }
}

beforeEach(() => {
    mocks.os = 'android'
    mocks.keepAlive = true
    vi.mocked(beginMobileBackgroundTask).mockReset()
    vi.mocked(beginDeferredMobileBackgroundTask).mockReset()
})

describe('reserved generation background', () => {
    it('holds the Android keep-alive only while the setting is on and ends it once with the outcome', async () => {
        const held = hold()
        vi.mocked(beginMobileBackgroundTask).mockReturnValue(held)
        const background = beginGenerationBackground()
        expect(beginMobileBackgroundTask).toHaveBeenCalledExactlyOnceWith('generation')
        background.settle(true)
        await background.end()
        await background.end()
        expect(held.dispose).toHaveBeenCalledExactlyOnceWith(true)

        mocks.keepAlive = false
        vi.mocked(beginMobileBackgroundTask).mockClear()
        await beginGenerationBackground().end()
        expect(beginMobileBackgroundTask).not.toHaveBeenCalled()
    })

    it('aborts the generation when the platform allowance expires and reports it once', async () => {
        const expiry = new AbortController()
        vi.mocked(beginMobileBackgroundTask).mockReturnValue(hold(expiry.signal))
        const background = beginGenerationBackground()
        await vi.waitFor(() => expect(beginMobileBackgroundTask).toHaveBeenCalledOnce())
        await Promise.resolve()
        expect(background.takeExpiry()).toBe(false)
        expiry.abort(new BackgroundExecutionExpiredError())
        expect(background.signal.aborted).toBe(true)
        expect(background.takeExpiry()).toBe(true)
        expect(background.takeExpiry()).toBe(false)
        await background.end()
    })

    it('reports progress to the iOS generation task and completes it only on success', async () => {
        mocks.os = 'ios'
        const ios = { signal: undefined, admitted: () => true, expired: () => false, progress: vi.fn(), dispose: vi.fn(async (_success?: boolean) => {}) }
        mocks.ios = ios
        const background = beginGenerationBackground()
        background.progress(3)
        await vi.waitFor(() => expect(ios.progress).toHaveBeenCalledWith(3))
        background.settle(true)
        await background.end()
        expect(ios.dispose).toHaveBeenCalledExactlyOnceWith(true)
    })
})

describe('model calls outside a reserved generation', () => {
    it('hold a deferred generation hold until the call settles', async () => {
        const held = hold()
        vi.mocked(beginDeferredMobileBackgroundTask).mockReturnValue(held)
        let finish!: (value: string) => void
        const call = holdModelCall(() => new Promise<string>(resolve => { finish = resolve }))
        expect(beginDeferredMobileBackgroundTask).toHaveBeenCalledExactlyOnceWith('generation')
        await Promise.resolve()
        expect(held.dispose).not.toHaveBeenCalled()
        finish('done')
        await expect(call).resolves.toBe('done')
        await vi.waitFor(() => expect(held.dispose).toHaveBeenCalledOnce())
    })

    it('hold a streamed response until its body is read to the end', async () => {
        const held = hold()
        vi.mocked(beginDeferredMobileBackgroundTask).mockReturnValue(held)
        const response = await holdModelCall(async () => ({
            type: 'streaming',
            result: new ReadableStream<string>({ start(controller) { controller.enqueue('partial'); controller.close() } }),
        }))
        await Promise.resolve()
        expect(held.dispose).not.toHaveBeenCalled()
        const reader = response.result.getReader()
        expect(await reader.read()).toEqual({ done: false, value: 'partial' })
        expect(await reader.read()).toEqual({ done: true, value: undefined })
        await vi.waitFor(() => expect(held.dispose).toHaveBeenCalledOnce())
    })

    it('take no hold on desktop, with the keep-alive setting off, or inside a reserved generation', async () => {
        mocks.os = 'windows'
        await expect(holdModelCall(async () => 'desktop')).resolves.toBe('desktop')
        mocks.os = 'android'
        mocks.keepAlive = false
        await expect(holdModelCall(async () => 'off')).resolves.toBe('off')
        mocks.keepAlive = true
        const reservation = reserveGeneration()!
        try { await expect(holdModelCall(async () => 'reserved')).resolves.toBe('reserved') }
        finally { reservation.release() }
        expect(beginDeferredMobileBackgroundTask).not.toHaveBeenCalled()
    })
})
