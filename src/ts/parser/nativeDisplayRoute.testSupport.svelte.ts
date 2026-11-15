/** `value` as reactive as `DBState.db`, so a test sees which of its keys a render tracks. */
export function reactiveCopy<T extends object>(value: T): T {
    const proxied = $state(value)
    return proxied
}
