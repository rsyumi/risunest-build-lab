import { writable } from 'svelte/store'

const state = writable<{ confirmationPending: boolean } | null>(null)
export const serverSyncRecovery = { subscribe: state.subscribe }
let retry: (() => Promise<void>) | undefined
let active: Promise<void> | undefined
let timer: ReturnType<typeof setTimeout> | undefined
let attempts = 0

export function setServerSyncRecovery(
    confirmationPending: boolean | null,
    recover?: () => Promise<void>,
): void {
    if (confirmationPending === null) {
        if (timer !== undefined) clearTimeout(timer)
        timer = undefined
        retry = undefined
        attempts = 0
        state.set(null)
        return
    }
    retry = recover
    state.set({ confirmationPending })
    if (!active && timer === undefined && attempts < 3) {
        timer = setTimeout(() => {
            timer = undefined
            attempts += 1
            void retryServerSyncRecovery().catch(() => {})
        }, [500, 1500, 4000][attempts])
    }
}

export function retryServerSyncRecovery(): Promise<void> {
    if (active) return active
    if (!retry) return Promise.resolve()
    const recover = retry
    active = recover().finally(() => {
        active = undefined
        if (retry && timer === undefined && attempts < 3) {
            timer = setTimeout(() => {
                timer = undefined
                attempts += 1
                void retryServerSyncRecovery().catch(() => {})
            }, [500, 1500, 4000][attempts])
        }
    })
    return active
}
