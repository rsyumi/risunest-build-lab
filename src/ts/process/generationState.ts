import { get, writable, type Writable } from 'svelte/store'
import { isLibraryFileOperationReserved } from '../storage/libraryFileOperation'

let activeReservation: symbol | null = null
const doingChatState = writable(false)
const generationCancellationState = writable<(() => void) | null>(null)
export const generationCancellation = { subscribe: generationCancellationState.subscribe }
export const doingChat: Writable<boolean> = {
    subscribe: doingChatState.subscribe,
    set(busy) {
        if (!busy && activeReservation) return
        doingChatState.set(busy)
    },
    update(updater) {
        doingChat.set(updater(get(doingChatState)))
    },
}

/** Keeps a reserved generation running after the app leaves the screen. */
export interface GenerationBackground {
    /** Aborts when the platform's background allowance expires. */
    readonly signal: AbortSignal
    progress(completed: number): void
    settle(success: boolean): void
    /** True once after expiry, so nested sends show one interrupted notice. */
    takeExpiry(): boolean
    end(): Promise<void>
}

export interface GenerationReservation {
    readonly background?: GenerationBackground
    isCurrent(): boolean
    release(options?: { preserveBusy?: boolean }): void
}

export const isGenerationReserved = () => activeReservation !== null

let startBackground: (() => GenerationBackground) | undefined
export function setGenerationBackground(start: (() => GenerationBackground) | undefined): void {
    startBackground = start
}

/** The caller's cancellation joined with background expiry. */
export function joinGenerationSignal(caller: AbortSignal | undefined, background: AbortSignal | undefined) {
    if (!background || !caller) return { signal: caller ?? background, release() {} }
    const controller = new AbortController()
    const abortCaller = () => controller.abort(caller.reason)
    const abortBackground = () => controller.abort(background.reason)
    if (caller.aborted) abortCaller()
    else if (background.aborted) abortBackground()
    caller.addEventListener('abort', abortCaller, { once: true })
    background.addEventListener('abort', abortBackground, { once: true })
    return {
        signal: controller.signal,
        release() {
            caller.removeEventListener('abort', abortCaller)
            background.removeEventListener('abort', abortBackground)
        },
    }
}

export function reserveGeneration(cancel?: () => void): GenerationReservation | null {
    if (activeReservation || get(doingChat) || isLibraryFileOperationReserved()) return null
    const token = Symbol('generation-reservation')
    activeReservation = token
    // Begins while the app is still on screen, before the input phase runs.
    const background = startBackground?.()
    let released = false
    let cancelled = false
    generationCancellationState.set(cancel ? () => {
        if (released || cancelled || activeReservation !== token) return
        cancelled = true
        cancel()
    } : null)
    doingChat.set(true)
    return {
        background,
        isCurrent: () => !released && activeReservation === token,
        release(options) {
            if (released) return
            released = true
            void background?.end()
            if (activeReservation !== token) return
            activeReservation = null
            generationCancellationState.set(null)
            if (!options?.preserveBusy) doingChat.set(false)
        },
    }
}
