import { readFile } from 'node:fs/promises'

const [baselinePath, candidatePath] = process.argv.slice(2)
if (!baselinePath || !candidatePath) throw new Error('Usage: node benchmarks/generation-send/compare.mjs <baseline-results.json> <candidate-results.json>')
const read = async file => JSON.parse((await readFile(file, 'utf8')).replace(/^\uFEFF/, ''))
const [baseline, candidate] = await Promise.all([read(baselinePath), read(candidatePath)])
const rows = result => Array.isArray(result) ? result : result.rows
const coarsePhases = new Set(['complete-promotion', 'current-chat-parser', 'tokenizer', 'output-listeners', 'indexeddb-commit', 'history-window'])
const mode = row => row.probeMode ?? (Object.keys(row.phases ?? {}).every(key => coarsePhases.has(key)) ? 'coarse' : 'fine')
const group = result => {
    const groups = new Map()
    if (!Array.isArray(rows(result)) || !rows(result).length) throw new Error('Expected nonempty measurement rows')
    for (const row of rows(result)) {
        if (mode(row) !== 'coarse') throw new Error('Performance comparisons require coarse probes; fine results diagnose individual costs and counts')
        if (Object.keys(row.phases ?? {}).some(key => !coarsePhases.has(key))) throw new Error('Unexpected fine phase in coarse results')
        if (!row.phases?.tokenizer || !row.phases?.['current-chat-parser']
            || !row.phases?.[row.historyLimit ? 'history-window' : 'complete-promotion']) {
            throw new Error('Missing required coarse phase probes')
        }
        const key = `${row.historyTokens}:${row.historyLimit}:${Boolean(row.templateEnabled)}`
        const samples = groups.get(key) || []
        samples.push(row)
        groups.set(key, samples)
    }
    return groups
}
const before = group(baseline)
const after = group(candidate)
if (before.size !== after.size || [...before.keys()].some(key => !after.has(key))) throw new Error('Measurement case sets differ')
for (const key of ['platform', 'node']) {
    if (baseline[key] && candidate[key] && baseline[key] !== candidate[key]) throw new Error(`Measurement ${key} differs`)
}
const median = values => {
    const sorted = [...values].sort((a, b) => a - b)
    if (sorted.some(value => !Number.isFinite(value))) throw new Error('Missing or nonfinite measurement')
    const middle = Math.floor(sorted.length / 2)
    return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2
}
const metrics = {
    preparationMs: row => row.preparationMs,
    completionMs: row => row.completionMs,
    preparationEncodingMs: row => row.phases.tokenizer.preparationMs,
    preparationOtherMs: row => row.preparationMs - row.phases.tokenizer.preparationMs,
    maxEventLoopGapMs: row => row.maxEventLoopGapMs,
    encodeCalls: row => row.phases.tokenizer.calls,
}
const invariantKeys = ['tokenizer', 'historyCharacters', 'historyUtf8Bytes', 'messages', 'metadataUtf8Bytes',
    'configuredContext', 'responseTokens', 'inputTokens', 'promptMessages', 'promptCharacters', 'saveRouteAtProvider']
const comparisons = []
for (const [key, oldRows] of before) {
    const newRows = after.get(key)
    const first = oldRows[0]
    for (const row of [...oldRows, ...newRows]) {
        for (const field of invariantKeys) {
            if (row[field] !== first[field]) throw new Error(`${key} changed ${field}`)
        }
        if ((row.tokenizerStringCache ?? row.tokenizerCache) !== false) throw new Error(`${key} does not disable the global string cache`)
    }
    const oldHashes = new Set(oldRows.map(row => row.promptSha256).filter(Boolean))
    if (oldHashes.size > 1) throw new Error(`${key} baseline prompts differ across repetitions`)
    const exactRequestVerified = oldHashes.size === 1
    if (exactRequestVerified && newRows.some(row => !oldHashes.has(row.promptSha256))) throw new Error(`${key} changed the exact request`)
    comparisons.push({ historyTokens: first.historyTokens, generationRoute: first.generationRoute,
        templateEnabled: Boolean(first.templateEnabled), probeMode: 'coarse',
        baselineObservations: oldRows.length, candidateObservations: newRows.length, exactRequestVerified,
        metrics: Object.fromEntries(Object.entries(metrics).map(([name, select]) => {
            const baselineMedian = median(oldRows.map(select))
            const candidateMedian = median(newRows.map(select))
            return [name, { baselineMedian, candidateMedian,
                changePercent: baselineMedian ? (candidateMedian - baselineMedian) / baselineMedian * 100 : null }]
        })),
    })
}
process.stdout.write(JSON.stringify({ schema: 'generation-send-comparison/v1',
    platform: candidate.platform ?? null, node: candidate.node ?? null,
    note: 'Matching coarse probes and fixture/output counts verified. Host workload must be established separately. Missing original prompt hashes cannot establish exact request equivalence.',
    comparisons,
}, null, 2) + '\n')
