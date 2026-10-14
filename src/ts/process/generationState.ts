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

export interface GenerationReservation {
    isCurrent(): boolean
    release(options?: { preserveBusy?: boolean }): void
}

export function reserveGeneration(cancel?: () => void): GenerationReservation | null {
    if (activeReservation || get(doingChat) || isLibraryFileOperationReserved()) return null
    const token = Symbol('generation-reservation')
    activeReservation = token
    let released = false
    let cancelled = false
    generationCancellationState.set(cancel ? () => {
        if (released || cancelled || activeReservation !== token) return
        cancelled = true
        cancel()
    } : null)
    doingChat.set(true)
    return {
        isCurrent: () => !released && activeReservation === token,
        release(options) {
            if (released) return
            released = true
            if (activeReservation !== token) return
            activeReservation = null
            generationCancellationState.set(null)
            if (!options?.preserveBusy) doingChat.set(false)
        },
    }
}
