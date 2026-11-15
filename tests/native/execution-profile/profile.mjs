// Serialized into the verification HTML before the product module starts.
export function installExecutionProfile(manifest, target = globalThis) {
    const name = '__risunestExecutionProfile'
    if (target[name]) throw new Error('execution-profile:already-installed')
    if (!manifest?.identity || !manifest.ledgerSha256 || !manifest.sources || !manifest.entries?.length) {
        throw new Error('execution-profile:manifest-missing')
    }
    const entries = new Map(manifest.entries.map(entry => [entry.id, entry]))
    let started = false
    let active = false
    let counts = {}
    const requireIdentity = identity => {
        if (identity !== manifest.identity) throw new Error('execution-profile:identity-mismatch')
    }
    const snapshot = identity => {
        requireIdentity(identity)
        if (!started) throw new Error('execution-profile:not-started')
        return JSON.parse(JSON.stringify({ identity: manifest.identity, ledgerSha256: manifest.ledgerSha256,
            sources: manifest.sources, instrumentation: manifest.instrumentation, started, active, counts }))
    }
    const api = {
        identity: manifest.identity,
        begin(identity) {
            requireIdentity(identity)
            if (active) throw new Error('execution-profile:already-active')
            counts = Object.fromEntries(manifest.entries.map(entry => [entry.id,
                { 'must-be-native': 0, 'allowed-webview': 0, 'web-only': 0 }]))
            started = active = true
        },
        enter(identity, id, mode) {
            requireIdentity(identity)
            const entry = entries.get(id)
            if (!entry) throw new Error('execution-profile:unknown-entry')
            if (!active) return
            const engine = entry.allowedModes?.includes(mode) ? 'allowed-webview' : entry.class
            const next = counts[id][engine] + 1
            if (!Number.isSafeInteger(next)) throw new Error('execution-profile:counter-overflow')
            counts[id][engine] = next
        },
        snapshot,
        end(identity) {
            requireIdentity(identity)
            if (!active) throw new Error('execution-profile:not-active')
            active = false
            return snapshot(identity)
        },
    }
    Object.defineProperty(target, name, { value: Object.freeze(api) })
    return api
}

export function assertNoJsProcessing(snapshot, manifest) {
    if (!snapshot?.started || snapshot.identity !== manifest.identity || snapshot.ledgerSha256 !== manifest.ledgerSha256) {
        throw new Error('execution-profile:observation-identity')
    }
    const sameKeys = (a, b) => a && b && Object.keys(a).sort().join('\n') === Object.keys(b).sort().join('\n')
    if (!sameKeys(snapshot.sources, manifest.sources)
        || Object.entries(manifest.sources).some(([file, hash]) => snapshot.sources[file] !== hash)) {
        throw new Error('execution-profile:source-fingerprints')
    }
    if (!sameKeys(snapshot.instrumentation, manifest.instrumentation)
        || Object.entries(manifest.instrumentation).some(([file, hash]) => snapshot.instrumentation[file] !== hash)) {
        throw new Error('execution-profile:instrumentation-fingerprints')
    }
    const expected = Object.fromEntries(manifest.entries.map(entry => [entry.id, true]))
    if (!sameKeys(snapshot.counts, expected)) throw new Error('execution-profile:entry-counts')
    const classes = { 'must-be-native': true, 'allowed-webview': true, 'web-only': true }
    for (const entry of manifest.entries) {
        const counts = snapshot.counts[entry.id]
        if (!sameKeys(counts, classes) || Object.values(counts).some(value => !Number.isSafeInteger(value) || value < 0)) {
            throw new Error('execution-profile:invalid-count')
        }
        if (counts['must-be-native'] || counts['web-only']) throw new Error(`execution-profile:js-processing:${entry.id}`)
    }
    if (!(snapshot.counts['send.entry']?.['allowed-webview'] > 0)) throw new Error('execution-profile:send-not-observed')
    return snapshot
}
