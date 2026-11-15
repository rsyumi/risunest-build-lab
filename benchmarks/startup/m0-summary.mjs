import { readFile } from 'node:fs/promises'
import { percentile, assertObservationIdentity, assertCompleteObservation, semanticCommitCount, IPC_COMMANDS } from './metrics.mjs'
import { summarizeMemorySamples } from './host-metrics.mjs'

const input = JSON.parse(await readFile(process.argv[2], 'utf8'))
const distribution = (values) => {
    if (!values.length || values.some((value) => !Number.isFinite(value)))
        throw new Error('Missing numeric observation')
    return {
        count: values.length,
        p50: percentile(values, 50),
        p95: percentile(values, 95),
        p99: percentile(values, 99),
        max: percentile(values, 100),
    }
}
const android = input.platform === 'android-arm64'
const observation = assertObservationIdentity({ ipcObservation: input.ipcObservation, observerFailure: null }, input.ipcObservation)
if (input.platform != null && !['windows', 'android-arm64'].includes(input.platform)
    || observation.platform !== (android ? 'android' : 'windows')) throw new Error('Unsupported M0 platform boundary')

function windowsMemory(row) {
    const m = row.host?.memory
    if (!m || m.coverageComplete !== true || !Number.isSafeInteger(m.samples) || m.samples < 1
        || m.completeSamples !== m.samples || !Array.isArray(m.series) || m.series.length !== m.samples
        || m.terminalFailure !== null || m.cleanupFailure !== null || m.measurementFailure != null
        || !Number.isSafeInteger(m.intervalMs) || m.intervalMs < 10)
        throw new Error('Incomplete current Windows memory observation')
    const canonical = summarizeMemorySamples(m.series, m.intervalMs, { terminalFailure: m.terminalFailure })
    const fields = ['nativeWorkingSetBytes', 'nativePrivateBytes', 'treeWorkingSetBytes', 'treePrivateBytes', 'processCount']
    if (!canonical.coverageComplete || canonical.completeSamples !== m.samples
        || ['first', 'settled', 'sampledPeak', 'observedCompleteSweepMax'].some(key =>
            fields.some(field => !Number.isSafeInteger(m[key]?.[field]) || m[key][field] < 0
                || m[key][field] !== canonical[key][field])))
        throw new Error('Inconsistent current Windows memory summary')
    return canonical
}

const memoryMappings = {
    nativeSettledWorkingSetBytes: ['settled', 'nativeWorkingSetBytes'],
    nativeSampledMaxWorkingSetBytes: ['sampledPeak', 'nativeWorkingSetBytes'],
    treeSettledWorkingSetBytes: ['settled', 'treeWorkingSetBytes'],
    treeSampledMaxWorkingSetBytes: ['sampledPeak', 'treeWorkingSetBytes'],
    nativeSettledPrivateBytes: ['settled', 'nativePrivateBytes'],
    nativeSampledMaxPrivateBytes: ['sampledPeak', 'nativePrivateBytes'],
    treeSettledPrivateBytes: ['settled', 'treePrivateBytes'],
    treeSampledMaxPrivateBytes: ['sampledPeak', 'treePrivateBytes'],
}
const result = {
    revision: input.revision,
    binarySha256: input.binarySha256,
    platform: input.platform ?? 'windows',
    ipcObservation: observation,
    groups: {},
}
for (const kind of ['reload', 'restart']) {
    const rows = input.samples.filter((row) => !row.warmup && row.kind === kind)
    if (rows.length < 20) throw new Error('M0 requires twenty samples per group')
    for (const row of rows) assertCompleteObservation(row, observation)
    if (
        rows.some(
            (row) =>
                !row.interaction?.success ||
                row.interaction.failure !== null ||
                !row.interaction.inputAccepted ||
                !row.interaction.scrollImmediateChanged ||
                row.documentVisible !== true ||
                row.stabilizationTimeout ||
                !row.host ||
                semanticCommitCount(row) < 1,
        )
    ) {
        throw new Error('Invalid M0 sample')
    }
    const commits = rows.flatMap((row) =>
        row.operations.filter(operation => operation.stage === 'store-commit').map(operation => operation.ms),
    )
    if (commits.length < rows.length) throw new Error('Missing local commit observations')
    result.groups[kind] = {
        samples: rows.length,
        inputMs: distribution(rows.map((row) => row.interaction.inputMs)),
        scrollMs: distribution(rows.map((row) => row.interaction.scrollMs)),
        semanticCommitMs: distribution(commits),
        semanticCommitCount: distribution(rows.map(semanticCommitCount)),
        sdkInvocationsByCommand: Object.fromEntries(IPC_COMMANDS.flatMap(command => {
            const calls = rows.flatMap(row => row.calls.filter(call => call.command === command))
            return calls.length ? [[command, { count: calls.length,
                failed: calls.filter(call => !call.success).length, durationMs: distribution(calls.map(call => call.ms)) }]] : []
        })),
        longTaskCount: distribution(rows.map((row) => row.longTasks.length)),
        longestTaskMs: distribution(
            rows.map((row) => Math.max(0, ...row.longTasks.map((task) => task.ms))),
        ),
        jsHeapBytes: distribution(rows.map((row) => row.usedHeapBytes)),
        ...(android
            ? {
                  appPssBytes: distribution(rows.map((row) => row.host.memory.appPssBytes)),
                  appRssBytes: distribution(rows.map((row) => row.host.memory.appRssBytes)),
                  walStartBytes: distribution(rows.map((row) => row.host.wal.startBytes)),
              }
            : {
                  memoryObservationKind: 'owned-tree-current-sweeps-v2',
                  nativeLifetimePeakAvailability: 'not-collected-by-current-stream',
                  ...(() => {
                      const memory = rows.map(windowsMemory)
                      return Object.fromEntries(Object.entries(memoryMappings).map(([name, [part, field]]) =>
                          [name, distribution(memory.map(value => value[part][field]))]))
                  })(),
                  walPeakBytes: distribution(rows.map((row) => row.host.wal.peakBytes)),
              }),
        walEndBytes: distribution(rows.map((row) => row.host.wal.endBytes)),
    }
}
process.stdout.write(JSON.stringify(result, null, 2) + '\n')
