import type { DisplayCbsContext } from './displayCbsCompiler'
import { Mutex } from '../mutex'
import { hasCbsSyntax } from '../parser/cbsSyntax'
import { ByteBudgetLru } from '../util/byteBudgetLru'
import type { RegexExecutionPlan, RegexExecutionPlanEntry, RegexExecutionResult } from './regexExecutionPlan'
import { executeRegexOperation, unmatchedRegexOperation, type RegexOperation, type RegexOperationResults } from './regexOperations'
import { DEFAULT_REGEX_WORKER_TIMEOUT_MS, getSharedRegexWorkerClient, isRegexWorkerAvailable, RegexExecutionTimeoutError, RegexWorkerResetError, type RegexWorkerClient } from './regexWorkerClient'
import { createRegexTimeoutScope, type RegexTimeoutScope } from './regexTimeoutScope'

export const DEFAULT_REGEX_INVOCATION_TIMEOUT_MS = 30_000
// The most inputs a caller passes to one `testEach` call.
export const REGEX_TEST_BATCH_SIZE = 64
export class RegexRuleSkippedError extends Error {}

// Hold the queue through isolation so other rows cannot retry the culprit first.
const executionMutex = new Mutex()
export class RegexTimeoutMemory {
    private readonly entries: ByteBudgetLru<string, string[]>
    constructor(maxBytes = 2 * 1024 * 1024, maxEntries = 128) {
        this.entries = new ByteBudgetLru(maxBytes,
            (key, inputs) => 2 * (key.length + inputs.reduce((size, input) => size + input.length, 0)), maxEntries)
    }
    has(key: string, input: string): boolean { return this.entries.get(key)?.includes(input) === true }
    hasRule(key: string): boolean { return this.entries.get(key) !== undefined }
    get size(): number { return this.entries.size }
    remember(key: string, input: string): void {
        this.entries.set(key, [...(this.entries.get(key) ?? []).filter((text) => text !== input).slice(-3), input])
    }
}
const recentTimeouts = new RegexTimeoutMemory()

async function runExclusive<T>(signal: AbortSignal | undefined, action: () => Promise<T>): Promise<T> {
    signal?.throwIfAborted()
    const acquisition = executionMutex.acquire()
    let abort: (() => void) | undefined
    let release: (() => void) | undefined
    try {
        release = signal ? await Promise.race([acquisition, new Promise<never>((_, reject) => {
            abort = () => reject(signal.reason ?? new DOMException('The operation was aborted', 'AbortError'))
            signal.addEventListener('abort', abort, { once: true })
        })]) : await acquisition
        signal?.throwIfAborted()
        return await action()
    } finally {
        if (abort) signal?.removeEventListener('abort', abort)
        if (release) release()
        else void acquisition.then((release) => release())
    }
}

export interface RegexExecutorOptions {
    // Only callers whose parse callback is identity on CBS-free text may opt in.
    skipPlainCbs?: boolean
    displayCbs?: { project(keys: readonly string[]): DisplayCbsContext | undefined, record(reads: readonly string[]): void }
    signal?: AbortSignal
    scope?: RegexTimeoutScope
    worker?: boolean
    client?: RegexWorkerClient
    requestTimeoutMs?: number
    invocationTimeoutMs?: number
    onTimeout?: () => void
    memory?: RegexTimeoutMemory
    // Display output may skip a rule that timed out on other text; stored messages may not.
    skipTimedOutRules?: boolean
}

export function regexRuleKey(entry: RegexExecutionPlanEntry): string {
    return JSON.stringify([entry.script.type, entry.pattern, entry.flags, entry.replacement, entry.actions])
}

export class RegexExecutor {
    lastOperationSkipped = false
    private readonly scope: RegexTimeoutScope
    private timedOutMs = 0
    private worker: boolean
    private readonly client: RegexWorkerClient

    constructor(private readonly options: RegexExecutorOptions = {}) {
        this.scope = options.scope ?? createRegexTimeoutScope()
        this.worker = options.worker ?? isRegexWorkerAvailable()
        this.client = options.client ?? getSharedRegexWorkerClient()
    }

    private skipped(key: string, input: string): boolean {
        const memory = this.options.memory ?? recentTimeouts
        return this.timedOutMs >= (this.options.invocationTimeoutMs ?? DEFAULT_REGEX_INVOCATION_TIMEOUT_MS)
            || this.scope.skipped.has(key) || (this.options.skipTimedOutRules ? memory.hasRule(key) : memory.has(key, input))
    }

    private timeout(key: string, input: string): void {
        this.scope.skipped.add(key)
        const memory = this.options.memory ?? recentTimeouts
        memory.remember(key, input)
        if (!this.scope.noticeShown) {
            this.scope.noticeShown = true
            this.options.onTimeout?.()
        }
    }

    private async request<T>(run: (timeoutMs: number) => Promise<T>, fallback: () => T): Promise<T> {
        this.options.signal?.throwIfAborted()
        if (!this.worker) return fallback()
        const timeoutMs = Math.min(this.options.requestTimeoutMs ?? DEFAULT_REGEX_WORKER_TIMEOUT_MS,
            (this.options.invocationTimeoutMs ?? DEFAULT_REGEX_INVOCATION_TIMEOUT_MS) - this.timedOutMs)
        let resetRetry = false
        try {
            try {
                return await run(timeoutMs)
            } catch (error) {
                if (!(error instanceof RegexWorkerResetError) || this.options.signal?.aborted) throw error
                resetRetry = true
                return await run(timeoutMs)
            }
        } catch (error) {
            this.options.signal?.throwIfAborted()
            if (error instanceof RegexExecutionTimeoutError) {
                this.timedOutMs += timeoutMs
                throw error
            }
            if (resetRetry || error instanceof RegexWorkerResetError) throw error
            // Only unavailable Workers fall back. Operation errors are delivered as
            // normal regex errors and must not execute the expression twice.
            if (error instanceof Error && error.name === 'RegexWorkerOperationError') throw error
            this.worker = false
            return fallback()
        }
    }

    async operation<K extends keyof RegexOperationResults>(operation: RegexOperation<K>, key = JSON.stringify([operation.pattern, operation.flags])): Promise<RegexOperationResults[K]> {
        return runExclusive(this.options.signal, () => this.operationUnlocked(operation, key))
    }

    private async operationUnlocked<K extends keyof RegexOperationResults>(operation: RegexOperation<K>, key: string): Promise<RegexOperationResults[K]> {
        this.options.signal?.throwIfAborted()
        this.lastOperationSkipped = this.skipped(key, operation.input)
        if (this.lastOperationSkipped) return unmatchedRegexOperation(operation)
        try {
            return await this.request(
                (timeoutMs) => this.client.operate(operation, { timeoutMs, signal: this.options.signal }),
                () => executeRegexOperation(operation),
            )
        } catch (error) {
            if (!(error instanceof RegexExecutionTimeoutError)) throw error
            this.timeout(key, operation.input)
            this.lastOperationSkipped = true
            return unmatchedRegexOperation(operation)
        }
    }

    /**
     * Tests inputs in order, as one `test` operation each would, under one lock acquisition and
     * one Worker request, so other regex work waits at most one batch. A timeout keeps the
     * earlier results and skips the rest.
     */
    async testEach(pattern: string, flags: string, inputs: readonly string[], key: string): Promise<boolean[]> {
        if (inputs.length === 0) return []
        return runExclusive(this.options.signal, async () => {
            const sent = inputs.map((input) => !this.skipped(key, input))
            const sentInputs = inputs.filter((_, index) => sent[index])
            let matches: boolean[] = []
            if (sentInputs.length > 0) {
                try {
                    matches = await this.request(
                        (timeoutMs) => this.client.testEach(pattern, flags, sentInputs, { timeoutMs, signal: this.options.signal }),
                        () => sentInputs.map((input) => executeRegexOperation({ kind: 'test', pattern, flags, input })),
                    )
                } catch (error) {
                    if (!(error instanceof RegexExecutionTimeoutError)) throw error
                    matches = error.matches ?? []
                    if (matches.length < sentInputs.length) this.timeout(key, sentInputs[matches.length])
                }
            }
            let next = 0
            return inputs.map((_, index) => sent[index] && (matches[next++] ?? false))
        })
    }

    async scriptOperation<K extends keyof RegexOperationResults>(operation: RegexOperation<K>, key: string): Promise<RegexOperationResults[K]> {
        if (this.skipped(key, operation.input)) throw new RegexRuleSkippedError()
        const result = await this.operationUnlocked(operation, key)
        if (this.skipped(key, operation.input)) throw new RegexRuleSkippedError()
        return result
    }

    async execute(plan: RegexExecutionPlan, input: string, parse: (text: string, ruleInput: string) => string | Promise<string>,
        host: (entry: RegexExecutionPlanEntry, input: string, executor: RegexExecutor) => Promise<string>): Promise<RegexExecutionResult> {
        return runExclusive(this.options.signal, async () => {
            let data = input
            let index = 0
            const stuck = new Set<number>()
            let compiledCbs = true
            const errors: RegexExecutionResult['errors'] = []
            const keys: string[] = []
            const keyAt = (index: number) => keys[index] ??= regexRuleKey(plan.entries[index])
            // CBS-only replacements still require host parsing after each change.
            const skipPlainCbs = this.options.skipPlainCbs
                && plan.entries.some((entry) => !hasCbsSyntax(entry.replacement))
            const memory = this.options.memory ?? recentTimeouts
            const skipCandidates = this.scope.skipped.size || memory.size
                ? plan.entries.flatMap((_, index) => this.scope.skipped.has(keyAt(index)) || memory.hasRule(keyAt(index)) ? [index] : [])
                : []
            try {
                while (index < plan.entries.length) {
                    this.options.signal?.throwIfAborted()
                    if (stuck.has(index)) {
                        this.timeout(keyAt(index), data)
                        index++
                        continue
                    }
                    const entry = plan.entries[index]
                    const needsSkipCheck = this.scope.skipped.size || memory.size
                        || this.timedOutMs >= (this.options.invocationTimeoutMs ?? DEFAULT_REGEX_INVOCATION_TIMEOUT_MS)
                    if (entry.pattern === '' || (needsSkipCheck && this.skipped(keyAt(index), data))) { index++; continue }
                    if (entry.requiresHostExecution || entry.dynamicPattern || !this.worker) {
                        data = await host(entry, data, this)
                        index++
                        continue
                    }
                    let end = plan.entries.length
                    for (const i of skipCandidates) {
                        if (i > index && i < end) end = i
                    }
                    let result: RegexExecutionResult
                    let cbsContext: DisplayCbsContext | undefined
                    try {
                        result = await this.request<RegexExecutionResult>((timeoutMs) => {
                            cbsContext = compiledCbs && this.options.skipPlainCbs && plan.displayCbs
                                ? this.options.displayCbs?.project(plan.displayCbs.keys) : undefined
                            return this.client.execute(plan, data, {
                                timeoutMs, signal: this.options.signal, continuation: { start: index, end, skipPlainCbs },
                                ...(cbsContext ? { cbsContext } : {}),
                            })
                        }, () => ({ data, errors: [], nextIndex: index, steps: [] }))
                    } catch (error) {
                        if (!(error instanceof RegexExecutionTimeoutError)) throw error
                        // The Worker names each rule before running it. The rules before that
                        // one finished, and run again because their output was lost with it.
                        const culprit = error.ruleIndex ?? index
                        stuck.add(culprit)
                        skipCandidates.push(culprit)
                        continue
                    }
                    this.options.signal?.throwIfAborted()
                    if (!this.worker) continue
                    if (cbsContext && result.cbsReads?.length) {
                        const current = this.options.displayCbs?.project(result.cbsReads)
                        if (!current || result.cbsReads.some((key) => current.variables[key] !== cbsContext!.variables[key])) {
                            // Only this read-only Worker run is discarded. Earlier host checkpoints are committed.
                            compiledCbs = false
                            continue
                        }
                    }
                    if (result.cbsReads) this.options.displayCbs?.record(result.cbsReads)
                    const next = result.nextIndex!
                    let replayChanged = false
                    for (const step of result.steps!) {
                        const current = plan.entries[step.index]
                        const before = step.index === next - 1 ? result.data : data
                        if (current.actions.length === 0 || step.matched) {
                            data = await parse(before, step.index === next - 1 ? result.checkpointInput ?? data : data)
                            if (data !== before) {
                                index = step.index + 1
                                replayChanged = true
                                break
                            }
                            if (step.index === next - 1 && result.unchangedCbsFallback) compiledCbs = false
                        } else data = before
                    }
                    // Speculative failures after a parse changed the input are discarded too.
                    errors.push(...result.errors.filter((error) => !replayChanged
                        || plan.entries.findIndex((candidate) => candidate.sourceIndex === error.sourceIndex) < index))
                    if (!replayChanged) {
                        data = result.data
                        index = next
                    }
                }
                return { data, errors }
            } finally {
                this.client.releaseContinuation()
            }
        })
    }
}
