import { isTauriAndroid, isTauriIOS } from '../platform'
import { getDeviceSettings } from '../storage/deviceSettings'
import { beginDeferredMobileBackgroundTask, beginMobileBackgroundTask, type MobileBackgroundTask } from '../mobileBackgroundTask'
import { beginIOSGeneration, isBackgroundExpiryReason } from '../iosNative'
import { isGenerationReserved, type GenerationBackground } from './generationState'

// On Android the keep-alive setting decides whether generation holds the service.
const androidKeepAlive = () => getDeviceSettings().androidKeepAliveDuringGeneration

/** The Android keep-alive and the iOS generation task for one reserved generation. */
export function beginGenerationBackground(): GenerationBackground {
    const controller = new AbortController()
    const forwardExpiry = <T extends { signal?: AbortSignal }>(task: T): T => {
        const signal = task.signal
        const abort = () => { if (isBackgroundExpiryReason(signal?.reason)) controller.abort(signal!.reason) }
        if (signal?.aborted) abort()
        else signal?.addEventListener('abort', abort, { once: true })
        return task
    }
    const android: Promise<MobileBackgroundTask> | null = isTauriAndroid && androidKeepAlive()
        ? Promise.resolve(beginMobileBackgroundTask('generation')).then(forwardExpiry) : null
    const ios = isTauriIOS ? beginIOSGeneration().then(forwardExpiry) : null
    let succeeded = false
    let expiryTaken = false
    let ended = false
    return {
        signal: controller.signal,
        progress(completed) { void ios?.then(task => task.progress(completed)) },
        settle(success) { succeeded = success },
        takeExpiry() {
            if (expiryTaken || !isBackgroundExpiryReason(controller.signal.reason)) return false
            expiryTaken = true
            return true
        },
        async end() {
            if (ended) return
            ended = true
            await Promise.all([
                android?.then(task => task.dispose(succeeded)),
                ios?.then(task => task.dispose(succeeded && !controller.signal.aborted)),
            ]).catch(error => console.error('Generation background cleanup failed', error))
        },
    }
}

function releaseWhenRead<R>(body: ReadableStream<R>, release: () => void): ReadableStream<R> {
    const reader = body.getReader()
    return new ReadableStream<R>({
        async pull(controller) {
            try {
                const { done, value } = await reader.read()
                if (done) {
                    release()
                    controller.close()
                } else controller.enqueue(value)
            } catch (error) {
                release()
                controller.error(error)
            }
        },
        cancel(reason) {
            release()
            return reader.cancel(reason)
        },
    })
}

/**
 * Keeps a model call outside a reserved generation running after Home without a notification while visible.
 * A streamed response holds until its body is read to the end, cancelled, or the call is aborted.
 */
export async function holdModelCall<T>(operation: () => Promise<T>, signal?: AbortSignal | null): Promise<T> {
    // A reserved generation already holds the platform's protection for its own calls.
    if ((!isTauriAndroid && !isTauriIOS) || (isTauriAndroid && !androidKeepAlive()) || isGenerationReserved()) return operation()
    // Native admission runs beside the call so a busy bridge never delays the request.
    const hold = Promise.resolve(beginDeferredMobileBackgroundTask('generation'))
    let released = false
    const release = () => {
        if (released) return
        released = true
        signal?.removeEventListener('abort', release)
        void hold.then(task => task.dispose())
    }
    signal?.addEventListener('abort', release, { once: true })
    let streaming = false
    try {
        const value = await operation()
        const response = value as { type?: unknown; result?: unknown } | null
        if (response?.type === 'streaming' && response.result instanceof ReadableStream && !signal?.aborted) {
            streaming = true
            return { ...response, result: releaseWhenRead(response.result, release) } as T
        }
        return value
    } finally {
        if (!streaming) release()
    }
}
