// A received record can name an asset before its body is downloaded. Display lookups that found
// no local body are resolved again once a download may have brought it, without touching the rest.
const revisions = $state<Record<string, number>>({})
const missing = new Set<string>()
let epoch = 0

function resolveAgain(key: string): void {
    revisions[key] = (revisions[key] ?? 0) + 1
}

/**
 * Makes a reactive caller resolve `key` again after a download that may have brought its body.
 * Returns the token to pass to `noteMissingAssetBody`, or undefined outside a reactive caller.
 */
export function trackAssetBodyArrival(key: string): number | undefined {
    if (!$effect.tracking()) return undefined
    void revisions[key]
    return epoch
}

/** Records that a lookup begun with `token` found no local body for `key`. */
export function noteMissingAssetBody(key: string, token: number): void {
    // A download that finished during the lookup may have landed the body after it was checked.
    if (token !== epoch) resolveAgain(key)
    else missing.add(key)
}

/** Resolves every lookup that found no body again, after a download that may have brought bodies. */
export function announceAssetBodyArrivals(): void {
    epoch++
    const keys = [...missing]
    missing.clear()
    for (const key of keys) resolveAgain(key)
}
