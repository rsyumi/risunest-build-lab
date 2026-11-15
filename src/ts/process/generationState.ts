import { get, writable, type Writable } from 'svelte/store'
import { isLibraryFileOperationReserved } from '../storage/libraryFileOperation'
import { createRegexTimeoutScope, type RegexTimeoutScope } from './regexTimeoutScope'
import type { PersistentDataRuntime } from '../storage/persistentDataRuntime'
import type { NativeGenerationStatus, NativeGenerationTicket } from '../storage/nativeGeneration'

interface GenerationOwner {
    regexScope: RegexTimeoutScope
    reservation?: GenerationReservation
    characterId?: string
    conversationId?: string
    nativeNavigationOwner?: symbol
    nativeMessageEdit?: NativeGenerationMessageEdit
    cancelled?: boolean
}
let activeReservation: GenerationOwner | null = null
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
    readonly regexScope?: RegexTimeoutScope
    readonly background?: GenerationBackground
    isCurrent(): boolean
    release(options?: { preserveBusy?: boolean }): void
}

export const isGenerationReserved = () => activeReservation !== null

export const isNativeGenerationNavigationAllowed = () => activeReservation?.nativeNavigationOwner !== undefined

/** Called only after native preparation commits have been adopted and the save pause ends. */
export function allowNativeGenerationNavigation(reservation: GenerationReservation): () => void {
    const token = activeReservation
    if (!token || !reservation.isCurrent()) return () => {}
    const owner = Symbol('native-generation-navigation')
    token.nativeNavigationOwner = owner
    return () => {
        if (activeReservation === token && token.nativeNavigationOwner === owner) delete token.nativeNavigationOwner
    }
}

interface NativeGenerationMessageEditTarget {
    characterId: string
    conversationId: string
}

export interface NativeGenerationMessageEdit {
    isCurrent(target: NativeGenerationMessageEditTarget): boolean
}

/** The original-message editor alone may enter the guarded edit cancellation path. */
export function allowNativeGenerationMessageEdit(reservation: GenerationReservation, options: {
    runtime: PersistentDataRuntime
    ticket: NativeGenerationTicket
    status: NativeGenerationStatus
    target: NativeGenerationMessageEditTarget
    authorityEpoch: number
    signal?: AbortSignal
}): () => void {
    const owner = activeReservation
    const { runtime, status, signal, authorityEpoch } = options
    const ticket = { ...options.ticket }
    const target = { ...options.target }
    if (!owner || owner.reservation !== reservation || !reservation.isCurrent() || owner.cancelled || signal?.aborted
        || status.phase !== 'running' || status.dispatch !== 'issued' || status.pendingCommit
        || !status.preparationReady || status.cancelDecision !== null
        || status.ticket.epoch !== ticket.epoch || status.ticket.sequence !== ticket.sequence
        || status.characterId !== target.characterId || status.conversationId !== target.conversationId) return () => {}
    const capability: NativeGenerationMessageEdit = {
        isCurrent(current) {
            if (activeReservation !== owner || owner.reservation !== reservation || owner.nativeMessageEdit !== capability
                || !reservation.isCurrent() || owner.cancelled || signal?.aborted
                || current.characterId !== target.characterId || current.conversationId !== target.conversationId) return false
            try { runtime.assertPersistentMutationAllowed(authorityEpoch) } catch { return false }
            return runtime.getNativeGenerationGuards().some(guard => guard.ticket.epoch === ticket.epoch
                && guard.ticket.sequence === ticket.sequence && guard.characterId === target.characterId
                && guard.conversationId === target.conversationId && guard.state === 'holding' && !guard.cancelled)
        },
    }
    owner.nativeMessageEdit = capability
    if (!capability.isCurrent(target)) delete owner.nativeMessageEdit
    return () => {
        if (activeReservation === owner && owner.nativeMessageEdit === capability) delete owner.nativeMessageEdit
    }
}

export function captureNativeGenerationMessageEdit(target: NativeGenerationMessageEditTarget): NativeGenerationMessageEdit | null {
    const capability = activeReservation?.nativeMessageEdit
    return capability?.isCurrent(target) ? capability : null
}

export function bindGenerationRegexOwner(reservation: GenerationReservation, characterId: string, conversationId: string | undefined): void {
    if (!reservation.isCurrent() || !activeReservation) return
    activeReservation.characterId = characterId
    activeReservation.conversationId = conversationId
}

export function getGenerationRegexScope(characterId: string, conversationId: string | undefined): RegexTimeoutScope | undefined {
    return activeReservation?.characterId === characterId && activeReservation?.conversationId === conversationId
        ? activeReservation.regexScope : undefined
}

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
    const token: GenerationOwner = { regexScope: createRegexTimeoutScope() }
    activeReservation = token
    // Begins while the app is still on screen, before the input phase runs.
    const background = startBackground?.()
    let released = false
    let cancelled = false
    generationCancellationState.set(cancel ? () => {
        if (released || cancelled || activeReservation !== token) return
        cancelled = true
        token.cancelled = true
        delete token.nativeMessageEdit
        cancel()
    } : null)
    doingChat.set(true)
    const reservation: GenerationReservation = {
        background,
        regexScope: token.regexScope,
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
    token.reservation = reservation
    return reservation
}
