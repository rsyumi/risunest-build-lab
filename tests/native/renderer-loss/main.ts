import { invoke } from '@tauri-apps/api/core'
import { Buffer } from 'buffer'
import { createGenerationDiagnostics, type GenerationDiagnosticPhase, type GenerationSavePath } from '../../../src/ts/generationDiagnostics'
import { SqlitePersistentDataStore } from '../../../src/ts/storage/sqlitePersistentDataStore'
import { nativeCommitTransport, type CommitEnvelope } from '../../../src/ts/storage/nativeCommitTransport'

globalThis.Buffer = Buffer

const token = crypto.randomUUID()
const canary = `synthetic-renderer-loss-private-${crypto.randomUUID()}`
const pending = new Set<Promise<unknown>>()
let documentSequence = 0
let initialized = false
let commitCallerAcknowledged = false
let heldNativeRevision: number | null = null
const store = new SqlitePersistentDataStore()
const diagnostics = createGenerationDiagnostics({
    enabled: true,
    invoke(command, args) {
        const request = invoke(command, args).then(value => {
            if ((args.event as { kind?: string }).kind === 'document-start') documentSequence = Number(value)
            return value
        })
        pending.add(request)
        void request.finally(() => pending.delete(request)).catch(() => {})
        return request
    },
})
async function flush() {
    while (pending.size) await Promise.all([...pending])
}
function requireInitialized() {
    if (!initialized) throw new Error('Synthetic native roots were not verified')
}

const harness = {
    token,
    async begin(expectedRoot: string) {
        const roots = await invoke<{ data: string }>('app_paths_roots')
        if (roots.data.toLowerCase().replaceAll('\\', '/') !== expectedRoot.toLowerCase().replaceAll('\\', '/')) {
            throw new Error('Synthetic native root mismatch')
        }
        initialized = true
        diagnostics.startDocument()
        await flush()
        await store.open()
        return { token, documentSequence }
    },
    async prepare(scenario: string) {
        requireInitialized()
        const trace = diagnostics.generation()
        const states: Record<string, [GenerationDiagnosticPhase, GenerationSavePath]> = {
            'send-preparation': ['prompt-preparation', 'unknown'],
            'awaiting-provider': ['awaiting-provider', 'complete'],
            parser: ['current-chat-parser', 'windowed'],
            'before-commit': ['final-acknowledgement', 'fallback'],
            'after-commit-before-js-ack': ['final-acknowledgement', 'complete'],
            settled: ['settled', 'windowed'],
        }
        const state = states[scenario]
        if (!state) throw new Error('Unknown synthetic diagnostic state')
        const [phase, savePath] = state
        trace.admission({
            intent: 'normal', displayMode: 'off', profile: 'normal',
            historyLimitEnabled: false, route: 'complete', fallback: 'none',
            admittedMessages: 3, totalMessages: 3,
        })
        trace.phase(scenario === 'settled' ? 'final-acknowledgement' : phase, 'enter', { savePath })
        await flush()
        const revisionBefore = await store.readRevision()
        const committed = scenario === 'after-commit-before-js-ack' || scenario === 'settled'
        const commit = {
            expectedRevision: revisionBefore,
            rootMutations: [{ type: 'set' as const, key: 'syntheticRendererLoss', value: canary }],
        }
        if (scenario === 'after-commit-before-js-ack') {
            let nativeReplied!: (value: { revision: number }) => void
            let nativeFailed!: (error: unknown) => void
            const reply = new Promise<{ revision: number }>((accept, reject) => {
                nativeReplied = accept
                nativeFailed = reject
            })
            const transport = new Proxy(nativeCommitTransport, {
                get(target, key) {
                    if (key === 'commit') return async (input: CommitEnvelope) => {
                        const result = await target.commit(input)
                        heldNativeRevision = result.revision
                        nativeReplied(result)
                        return await new Promise<never>(() => {})
                    }
                    const value = Reflect.get(target, key, target)
                    return typeof value === 'function' ? value.bind(target) : value
                },
            })
            const heldStore = new SqlitePersistentDataStore(transport)
            void heldStore.commit(commit).then(() => { commitCallerAcknowledged = true }, nativeFailed)
            const nativeResult = await reply
            if (await store.readRevision() !== nativeResult.revision) throw new Error('Native commit is not independently readable')
            if (commitCallerAcknowledged) throw new Error('Store caller observed the held acknowledgement')
        } else if (committed) {
            await store.commit(commit)
        }
        if (scenario === 'settled') {
            trace.phase('final-acknowledgement', 'exit', { savePath, outcome: 'ok' })
            trace.phase('settled', 'exit', { savePath, outcome: 'ok' })
            await flush()
        }
        let unknownFieldRejected = false
        try {
            await invoke('native_generation_diagnostic', { event: {
                kind: 'phase', documentSequence, eventSequence: 999, generationSequence: 1,
                phase, edge: 'enter', prompt: canary, response: canary, apiKey: canary,
            } })
        } catch { unknownFieldRejected = true }
        if (!unknownFieldRejected) throw new Error('Diagnostic payload accepted private fields')
        const log = JSON.stringify(await invoke('native_log_tail', { limit: 500 }))
        if (log.includes(canary)) throw new Error('Synthetic private canary reached native log')
        return {
            documentSequence, phase, savePath, edge: scenario === 'settled' ? 'exit' : 'enter',
            committed, revisionBefore, revisionAfter: await store.readRevision(),
            acknowledgement: scenario === 'after-commit-before-js-ack' ? {
                boundary: 'store caller promise held after underlying native transport reply',
                callerObserved: commitCallerAcknowledged, nativeRevision: heldNativeRevision,
            } : null,
            canary,
        }
    },
    acknowledgement() {
        requireInitialized()
        return { callerObserved: commitCallerAcknowledged, nativeRevision: heldNativeRevision }
    },
    async readback() {
        requireInitialized()
        return { revision: await store.readRevision(), root: await store.readRoot() }
    },
}

Object.assign(window, { __rendererLoss: harness })
