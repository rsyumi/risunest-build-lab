import type { PersistentUnitMutation } from './persistentDataStore'

interface PersistentIdentityHooks {
    beforeCapture(): void
    afterRemoteApply(): void
    translateRootUnitIntents?(mutations: readonly PersistentUnitMutation[]): PersistentUnitMutation[]
}

let hooks: PersistentIdentityHooks | null = null

export function configurePersistentIdentityHooks(value: PersistentIdentityHooks): void {
    hooks = value
}

export function flushPersistentIdentityEdits(): void { hooks?.beforeCapture() }
export function derivePersistentIdentityMirrors(): void { hooks?.afterRemoteApply() }
export function translatePersistentRootUnitIntents(mutations: readonly PersistentUnitMutation[]): PersistentUnitMutation[] {
    return hooks?.translateRootUnitIntents?.(mutations) ?? [...mutations]
}
