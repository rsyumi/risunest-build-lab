const operations = new Set([
    'prepare-import',
    'prepare-bootstrap',
    'normalize',
    'canonical',
    'baseline',
    'capture',
    'capture-database',
    'flush',
    'bootstrap',
    'clean-chunks',
    'save-observer',
    'save-effect-root',
    'save-effect-character',
    'store-commit',
])
export const SDK_VERSION = '2.12.1'
export const IPC_BOUNDARY = 'tauri-sdk-invoke-v1'
export const SEMANTIC_BOUNDARY = 'sqlite-store-commit-v1'
export const OBSERVATION_MANIFEST = 'startup-observation.local.json'
export const OBSERVER_INPUTS = [
    'benchmarks/startup/observe.mjs',
    'benchmarks/startup/android-observation.mjs',
    'benchmarks/startup/cdp.mjs',
    'benchmarks/startup/metrics.mjs',
    'benchmarks/startup/before-build.mjs',
]
export const IPC_COMMANDS = [
    'pds_open',
    'pds_materialize',
    'pds_commit',
    'pds_replace_commit',
    'pds_read_root',
    'pds_read_revision',
    'pds_query_characters',
    'pds_snapshot_create',
    'pds_snapshot_list',
    'pds_read_character',
    'pds_query_conversations',
    'pds_read_conversation_window',
    'pds_read_conversation',
    'pds_asset_gc_maintenance',
    'plugin:fs|read_dir',
    'plugin:fs|stat',
    'asset_cas_read_bytes',
    'asset_cas_stat_object',
    'pds_commit_raw',
    'pds_commit_android_open',
    'pds_commit_android_chunk',
    'pds_commit_android_finish',
    'pds_commit_android_cancel',
    'pds_replace_android_finish',
    'pds_commit_shared_open',
    'pds_commit_shared_chunk',
    'pds_commit_shared_finish',
    'pds_commit_shared_cancel',
]
const commands = new Set(IPC_COMMANDS)
const phases = new Set(['storage', 'data', 'compatibility', 'plugins', 'sync'])
const marks = new Set([
    'boot:local-data-ready',
    'boot:account-ready',
    'boot:cold-storage-ready',
    'boot:plugins-ready',
    'boot:interactive',
])
const number = (value) => (typeof value === 'number' && Number.isFinite(value) ? value : null)
const list = (value) => (Array.isArray(value) ? value : [])

export function sanitizeObservationManifest(value) {
    const hash = (v) => typeof v === 'string' && /^[a-f0-9]{64}$/.test(v)
    if (!value || value.schema !== 1 || value.boundary !== IPC_BOUNDARY
        || value.semanticBoundary !== SEMANTIC_BOUNDARY || !hash(value.inputFingerprint)
        || !/^[a-f0-9]{40}$/.test(value.sourceRevision ?? '') || typeof value.diagnosticStages !== 'boolean'
        || !['windows', 'android', 'linux', 'macos', 'ios'].includes(value.platform)
        || value.sdk?.version !== SDK_VERSION || !hash(value.sdk.sourceSha256)
        || !hash(value.sdk.transformedSha256) || !hash(value.store?.sourceSha256)
        || !hash(value.store.transformedSha256)
        || !OBSERVER_INPUTS.every(key => hash(value.observerInputs?.[key]))
        || Object.keys(value.observerInputs ?? {}).length !== OBSERVER_INPUTS.length
        || JSON.stringify(value.selectedCommands) !== JSON.stringify(IPC_COMMANDS)) return null
    return {
        schema: 1, boundary: IPC_BOUNDARY, semanticBoundary: SEMANTIC_BOUNDARY,
        platform: value.platform, inputFingerprint: value.inputFingerprint,
        sourceRevision: value.sourceRevision,
        diagnosticStages: value.diagnosticStages,
        sdk: { version: SDK_VERSION, sourceSha256: value.sdk.sourceSha256, transformedSha256: value.sdk.transformedSha256 },
        store: { sourceSha256: value.store.sourceSha256, transformedSha256: value.store.transformedSha256 },
        observerInputs: Object.fromEntries(OBSERVER_INPUTS.map(key => [key, value.observerInputs[key]])),
        selectedCommands: [...IPC_COMMANDS],
    }
}

export function assertObservationIdentity(raw, expected) {
    const actual = sanitizeObservationManifest(raw?.ipcObservation)
    const wanted = sanitizeObservationManifest(expected)
    if (!actual || !wanted || JSON.stringify(actual) !== JSON.stringify(wanted))
        throw new Error('Missing or mismatched SDK invocation observation manifest')
    if (raw.observerFailure !== null) throw new Error('Startup observer failed')
    return actual
}

export function assertCompleteObservation(raw, expected) {
    assertObservationIdentity(raw, expected)
    if (!Array.isArray(raw.calls) || !Array.isArray(raw.operations)
        || raw.calls.some(call => !commands.has(call.command) || call.bytes !== null || typeof call.success !== 'boolean'
            || !Number.isFinite(call.start) || call.start < 0 || !Number.isFinite(call.ms) || call.ms < 0)
        || raw.operations.some(operation => typeof operation.success !== 'boolean'
            || !Number.isFinite(operation.start) || operation.start < 0
            || !Number.isFinite(operation.ms) || operation.ms < 0)
        || (raw.unfinishedOperations ?? []).length
        || Object.values(raw.active ?? {}).some(count => count > 0))
        throw new Error('Incomplete SDK invocation observation')
}

export function semanticCommitCount(sample) {
    const rows = sample.operations.filter(operation => operation.stage === 'store-commit')
    if (rows.some(operation => operation.success !== true || !Number.isFinite(operation.ms)))
        throw new Error('Incomplete or failed semantic commit')
    return rows.length
}

export function sanitizeMetrics(raw) {
    return {
        ipcObservation: sanitizeObservationManifest(raw.ipcObservation),
        observerFailure: raw.observerFailure === null ? null : 'observer-failed',
        interactiveMs: number(raw.interactiveMs),
        firstPaintMs: number(raw.firstPaintMs),
        usedHeapBytes: number(raw.usedHeapBytes),
        sampledPeakHeapBytes: number(raw.sampledPeakHeapBytes),
        maxMediaInFlight: number(raw.maxMediaInFlight),
        documentVisible: raw.documentVisible === true,
        cleanChunksReason: number(raw.cleanChunksReason),
        imageResources: list(raw.imageResources).map((v) => ({
            start: number(v.start),
            ms: number(v.ms),
        })),
        firstRevision: number(raw.firstRevision),
        lastRevision: number(raw.lastRevision),
        stabilizationTimeout: raw.stabilizationTimeout === true,
        elapsedSeen: raw.elapsedSeen === true,
        elapsedVisibleAfterStartup: raw.elapsedVisibleAfterStartup === true,
        calls: list(raw.calls)
            .filter((v) => commands.has(v.command))
            .map((v) => ({
                command: v.command,
                start: number(v.start),
                ms: number(v.ms),
                bytes: number(v.bytes),
                success: v.success === true,
            })),
        operations: list(raw.operations)
            .filter((v) => operations.has(v.stage))
            .map((v) => ({
                stage: v.stage,
                start: number(v.start),
                ms: number(v.ms),
                success: v.success === true,
            })),
        phases: list(raw.phases)
            .filter((v) => phases.has(v.stage))
            .map((v) => ({ stage: v.stage, ms: number(v.ms) })),
        marks: list(raw.marks)
            .filter((v) => marks.has(v.stage))
            .map((v) => ({ stage: v.stage, ms: number(v.ms) })),
        longTasks: list(raw.longTasks).map((v) => ({ start: number(v.start), ms: number(v.ms) })),
        unfinishedOperations: Object.entries(raw.active ?? {})
            .filter(([key, value]) => operations.has(key) && value > 0)
            .map(([stage, count]) => ({ stage, count: number(count) })),
        interaction: raw.interaction
            ? {
                  selectionMs: number(raw.interaction.selectionMs),
                  inputMs: number(raw.interaction.inputMs),
                  scrollMs: number(raw.interaction.scrollMs),
                  success: raw.interaction.success === true,
                  inputAccepted: raw.interaction.inputAccepted === true,
                  scrollChanged: raw.interaction.scrollChanged === true,
                  scrollImmediateChanged: raw.interaction.scrollImmediateChanged === true,
                  failure: number(raw.interaction.failure),
                  firstUiReadyMs: number(raw.interaction.firstUiReadyMs),
                  firstSelectionMs: number(raw.interaction.firstSelectionMs),
                  firstScrollReadyMs: number(raw.interaction.firstScrollReadyMs),
              }
            : null,
    }
}

export function percentile(values, percent) {
    const sorted = values
        .filter((value) => typeof value === 'number' && Number.isFinite(value))
        .sort((a, b) => a - b)
    return sorted.length ? sorted[Math.ceil((sorted.length * percent) / 100) - 1] : null
}

export function maxConcurrentResources(resources) {
    const events = resources
        .flatMap(({ start, ms }) =>
            Number.isFinite(start) && Number.isFinite(ms) && ms > 0
                ? [
                      { at: start, delta: 1 },
                      { at: start + ms, delta: -1 },
                  ]
                : [],
        )
        .sort((a, b) => a.at - b.at || a.delta - b.delta)
    let active = 0,
        peak = 0
    for (const event of events) {
        active += event.delta
        peak = Math.max(peak, active)
    }
    return peak
}
