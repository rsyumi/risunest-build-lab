import { describe, expect, it, vi } from 'vitest'
import type { customscript } from '../storage/database.svelte'
import {
    canExecuteRegexPlanInWorker,
    getRegexExecutionPlan,
    type RegexExecutionPlan,
} from './regexExecutionPlan'
import {
    DEFAULT_REGEX_WORKER_TIMEOUT_MS,
    RegexExecutionTimeoutError,
    RegexWorkerResetError,
    RegexWorkerUnavailableError,
    REGEX_WORKER_READY_TIMEOUT_MS,
    RegexWorkerClient,
    type RegexWorkerRequest,
    type RegexWorkerResponse,
} from './regexWorkerClient'
import { FakeWorker, InMemoryWorker, StallingWorker } from '../../../tests/support/regexWorker'
import { createRegexWorkerMessageHandler } from './regexWorker'
import { RegexExecutor, RegexTimeoutMemory } from './regexExecutor'
import { executeRegexOperation, replaceRegexMatches } from './regexOperations'
import { createRegexTimeoutScope } from './regexTimeoutScope'
import { executeRegexPlanSync } from './regexExecutionPlan'

function plan(revision = 1): RegexExecutionPlan {
    return {
        revision,
        mode: 'editoutput',
        workerEligible: true,
        requiresHostExecution: false,
        entries: [{
            sourceIndex: 0,
            script: {
                comment: '',
                in: 'a',
                out: 'b',
                type: 'editoutput',
                flag: 'g',
                ableFlag: true,
            },
            pattern: 'a',
            replacement: 'b',
            flags: 'g',
            order: 0,
            actions: [],
            dynamicPattern: false,
            compiledRegex: /a/g,
            requiresHostExecution: false,
        }],
    }
}

function plannedScript(output = 'b', flag = 'g'): customscript {
    return {
        comment: '',
        in: 'a',
        out: output,
        type: 'editoutput',
        flag,
        ableFlag: true,
    }
}

function continuationPlan(patterns: [string, string, string?][], mode = 'editdisplay') {
    return getRegexExecutionPlan(patterns.map(([pattern, replacement, flags]) => ({
        ...plannedScript(replacement, flags ?? 'g'), in: pattern, type: mode,
    })), mode)
}

function executePlan(executor: RegexExecutor, plan: RegexExecutionPlan, input: string, parse = (value: string) => value) {
    return executor.execute(plan, input, parse, async (entry, value, runner) => {
        const result = await runner.scriptOperation({ kind: 'replace', pattern: entry.dynamicPattern ? parse(entry.pattern) : entry.pattern,
            input: value, flags: entry.flags, replacement: entry.replacement }, JSON.stringify(entry.script))
        return parse(result)
    })
}

describe('bounded regex execution through the real Worker handler', () => {
    it('keeps plain replacements resident, stops at generated CBS, and returns the final plain tail', async () => {
        const worker = new InMemoryWorker()
        const executor = new RegexExecutor({ skipPlainCbs: true, worker: true, client: new RegexWorkerClient(() => worker) })
        const plan = continuationPlan([['a', '<b>b</b>'], ['b', 'c'], ['c', '{{value}}'], ['resolved', 'tail']])
        const parsed: string[] = []
        const result = await executePlan(executor, plan, 'a', value => {
            parsed.push(value)
            return value.replaceAll('{{value}}', 'resolved')
        })
        expect(result.data).toBe('<tail>tail</tail>\n')
        expect(parsed).toEqual(['<{{value}}>{{value}}</{{value}}>\n'])
    })

    it('checks remembered timeouts after preceding plain replacements change the input', async () => {
        const memory = new RegexTimeoutMemory()
        const plan = continuationPlan([['a', 'b'], ['b', 'wrong'], ['b', 'done']])
        const { regexRuleKey } = await import('./regexExecutor')
        memory.remember(regexRuleKey(plan.entries[1]), 'b')
        const result = await executePlan(new RegexExecutor({ skipPlainCbs: true, memory, worker: true,
            client: new RegexWorkerClient(() => new InMemoryWorker()) }), plan, 'a')
        expect(result.data).toBe('done')
    })
    it('bounds remembered inputs by both byte size and entry count', () => {
        const memory = new RegexTimeoutMemory(100, 2)
        memory.remember('a', 'first')
        memory.remember('b', 'next')
        memory.remember('c', 'last')
        expect(memory.has('a', 'first')).toBe(false)
        expect(memory.has('c', 'last')).toBe(true)
        memory.remember('large', 'x'.repeat(100))
        expect(memory.has('large', 'x'.repeat(100))).toBe(false)
        memory.remember('d', 'more')
        expect(memory.has('b', 'next')).toBe(false)
    })

    it('cancels a queued request immediately without waiting for another row to time out', async () => {
        vi.useFakeTimers()
        try {
            const client = new RegexWorkerClient(() => new StallingWorker())
            const active = new RegexExecutor({ client, worker: true, requestTimeoutMs: 10 }).operation({ kind: 'test', pattern: 'SLOW-queued', flags: '', input: 'a' })
            const controller = new AbortController()
            const pending = new RegexExecutor({ client, worker: true, signal: controller.signal }).operation({ kind: 'test', pattern: 'a', flags: '', input: 'a' })
            const rejected = expect(pending).rejects.toMatchObject({ name: 'AbortError' })
            controller.abort()
            await rejected
            await vi.advanceTimersByTimeAsync(10)
            expect(await active).toBe(false)
        } finally { vi.useRealTimers() }
    })

    it.each(['editinput', 'editoutput', 'editprocess', 'editdisplay'])('replays parsing and replacement semantics in %s', async (mode) => {
        const plan = continuationPlan([
            ['missing', 'unused'],
            ['x', '{{change}}'],
            ['y', 'z', 'g<unknown>'],
            ['z', '$&$`$\''],
            ['(?<last>z)', '$<last>!'],
        ], mode)
        const inputs: string[] = []
        const parse = (value: string) => { inputs.push(value); return value.replace('{{change}}', 'y') }
        const expected = executeRegexPlanSync(plan, '<x>', parse)
        const expectedInputs = inputs.splice(0)
        const actual = await executePlan(new RegexExecutor({ worker: true, client: new RegexWorkerClient(() => new InMemoryWorker()) }), plan, '<x>', parse)
        expect(actual.data).toBe(expected.data)
        expect(inputs).toEqual(expectedInputs)
    })

    it('discards speculative replacement and errors when an earlier parse changes the input', async () => {
        const plan = continuationPlan([['absent', 'unused'], ['x', 'wrong'], ['[', 'invalid'], ['y', 'correct']])
        const parse = (value: string) => value === 'x' ? 'y' : value
        const executor = new RegexExecutor({ worker: true, client: new RegexWorkerClient(() => new InMemoryWorker()) })
        const result = await executePlan(executor, plan, 'x', parse)
        expect(result.data).toBe('correct')
        expect(result.errors.map((error) => error.sourceIndex)).toEqual([2])
    })

    it('parses every plain rule and only matching conditional rules, including unchanged replacements', async () => {
        const plan = continuationPlan([['missing', 'x'], ['missing', 'x', 'g<unknown>'], ['a', '$&', 'g<unknown>'], ['a', 'b']])
        let calls = 0
        const executor = new RegexExecutor({ worker: true, client: new RegexWorkerClient(() => new InMemoryWorker()) })
        const result = await executePlan(executor, plan, 'a', (value) => { calls++; return value })
        expect(result.data).toBe('b')
        expect(calls).toBe(3)
    })

    it.each(['g', 'y', 'gy', 'i'])('avoids a separate match search for ordinary %s replacements', async flags => {
        const plan = continuationPlan([['a(?:b)', '$&!', flags]], 'editinput')
        const original = RegExp.prototype.test
        let probes = 0
        const probe = vi.spyOn(RegExp.prototype, 'test').mockImplementation(function (input) {
            if (this.source === 'a(?:b)') probes++
            return original.call(this, input)
        })
        try {
            for (const input of ['abab', 'missing']) {
                const expected = executeRegexPlanSync(plan, input, value => value)
                const executor = new RegexExecutor({ worker: true, client: new RegexWorkerClient(() => new InMemoryWorker()) })
                expect((await executePlan(executor, plan, input)).data).toBe(expected.data)
            }
            expect(probes).toBe(0)
        } finally { probe.mockRestore() }
    })

    it('stops at host rules and resumes using their changed input', async () => {
        const plan = continuationPlan([['none', 'unused'], ['dynamic', 'B', 'g<cbs>'], ['B', 'C'], ['C', '@@host'], ['D', 'E']])
        const executor = new RegexExecutor({ worker: true, client: new RegexWorkerClient(() => new InMemoryWorker()) })
        const result = await executor.execute(plan, 'A', (value) => value, async (entry, input, runner) => {
            if (entry.dynamicPattern) return runner.scriptOperation({ kind: 'replace', input, pattern: 'A', flags: entry.flags, replacement: 'B' }, 'dynamic')
            return (await runner.scriptOperation({ kind: 'test', input, pattern: 'C', flags: '' }, 'directive')) ? 'D' : input
        })
        expect(result.data).toBe('E')
    })

    it('isolates once for thirty queued rows, preserves following rules, and remembers the same text across batches', async () => {
        vi.useFakeTimers()
        try {
            const client = new RegexWorkerClient(() => new StallingWorker())
            const scope = createRegexTimeoutScope()
            const notices = vi.fn()
            const plan = continuationPlan([['missing', 'unused'], ['SLOW-display', 'bad'], ['row', 'rendered']])
            const run = (input: string, batch = scope) => executePlan(new RegexExecutor({ client, worker: true, scope: batch,
                requestTimeoutMs: 10, invocationTimeoutMs: 30, onTimeout: notices }), plan, input)
            let timerFired = false
            setTimeout(() => { timerFired = true }, 1)
            const start = Date.now()
            const rows = Promise.all(Array.from({ length: 30 }, (_, i) => run(`<row ${i} SLOW-display>`)))
            await vi.advanceTimersByTimeAsync(1)
            expect(timerFired).toBe(true)
            await vi.advanceTimersByTimeAsync(19)
            expect((await rows).map((row) => row.data)).toEqual(Array.from({ length: 30 }, (_, i) => `<rendered ${i} SLOW-display>`))
            expect(Date.now() - start).toBe(20)
            expect(notices).toHaveBeenCalledOnce()
            // This exact input timed out; a new batch must not wait for it again.
            expect((await run('<row 0 SLOW-display>', createRegexTimeoutScope())).data).toBe('<rendered 0 SLOW-display>')
            expect(notices).toHaveBeenCalledOnce()
        } finally { vi.useRealTimers() }
    })

    it('skips a timed-out rule on later stream updates and final output containing markup', async () => {
        vi.useFakeTimers()
        try {
            const client = new RegexWorkerClient(() => new StallingWorker())
            const scope = createRegexTimeoutScope()
            const plan = continuationPlan([['SLOW-stream', 'bad'], ['tail', 'done']], 'editoutput')
            const run = (input: string) => executePlan(new RegexExecutor({ client, worker: true, scope, requestTimeoutMs: 10 }), plan, input)
            const first = run('<tail SLOW-stream>')
            await vi.advanceTimersByTimeAsync(20)
            expect((await first).data).toBe('<done SLOW-stream>')
            expect((await run('<tail SLOW-stream> longer')).data).toBe('<done SLOW-stream> longer')
            expect((await run('<tail SLOW-stream> final')).data).toBe('<done SLOW-stream> final')
        } finally { vi.useRealTimers() }
    })

    it('caps timeout time including isolation and skips the remaining rules', async () => {
        vi.useFakeTimers()
        try {
            const plan = continuationPlan([['SLOW-budget-1', 'bad'], ['SLOW-budget-2', 'bad'], ['tail', 'changed']])
            const executor = new RegexExecutor({ client: new RegexWorkerClient(() => new StallingWorker()), worker: true,
                requestTimeoutMs: 10, invocationTimeoutMs: 25 })
            const result = executePlan(executor, plan, 'tail')
            await vi.advanceTimersByTimeAsync(25)
            expect((await result).data).toBe('tail')
        } finally { vi.useRealTimers() }
    })

    it('returns no match for a stalled trigger operation and continues with subsequent operations', async () => {
        vi.useFakeTimers()
        try {
            const executor = new RegexExecutor({ client: new RegexWorkerClient(() => new StallingWorker()), worker: true, requestTimeoutMs: 10 })
            const test = executor.operation({ kind: 'test', pattern: 'SLOW-trigger|text', flags: 's', input: '<text>' })
            await vi.advanceTimersByTimeAsync(10)
            expect(await test).toBe(false)
            const replacement = executor.operation({ kind: 'replacementMatches', pattern: 'SLOW-replace|text', flags: 'g', input: '<text>' })
            await vi.advanceTimersByTimeAsync(10)
            expect(replaceRegexMatches('<text>', await replacement, () => 'bad')).toBe('<text>')
            expect(await executor.operation({ kind: 'replace', pattern: 'text', flags: '', input: '<text>', replacement: 'next' })).toBe('<next>')
        } finally { vi.useRealTimers() }
    })

    it('preserves callback captures, optional named groups, zero-width matches, and split semantics', async () => {
        const executor = new RegexExecutor({ client: new RegexWorkerClient(() => new InMemoryWorker()), worker: true })
        const input = '😀ab a'
        const pattern = '(?<first>a)(b)?|(?=$)'
        const replace = (...args: any[]) => JSON.stringify(args)
        const matches = await executor.operation({ kind: 'replacementMatches', input, pattern, flags: 'gu' })
        expect(replaceRegexMatches(input, matches, replace)).toBe(input.replace(new RegExp(pattern, 'gu'), replace))
        for (const kind of ['test', 'match', 'exec', 'matches', 'split'] as const) {
            const operation = { kind, input, pattern, flags: 'gu' }
            expect(await executor.operation(operation)).toEqual(executeRegexOperation(operation))
        }
    })

    it('falls back only when the Worker cannot start, and preserves cancellation', async () => {
        const executor = new RegexExecutor({ worker: true, client: new RegexWorkerClient(() => { throw new Error('boot failed') }) })
        expect(await executor.operation({ kind: 'replace', pattern: 'a', flags: 'g', input: 'aa', replacement: 'b' })).toBe('bb')
        const controller = new AbortController()
        controller.abort()
        const cancelled = new RegexExecutor({ worker: true, signal: controller.signal, client: new RegexWorkerClient(() => new InMemoryWorker()) })
        await expect(cancelled.operation({ kind: 'test', pattern: 'a', flags: '', input: 'a' })).rejects.toMatchObject({ name: 'AbortError' })
    })
})

describe('RegexWorkerClient', () => {
    it('reuses unchanged continuation input and releases its retained message at the end', async () => {
        const plan = continuationPlan([['a', 'b'], ['b', 'c']])
        const worker = new InMemoryWorker()
        const client = new RegexWorkerClient(() => worker)
        const first = await client.execute(plan, 'a', { continuation: { start: 0, end: 2 } })
        const second = await client.execute(plan, first.data, { continuation: { start: first.nextIndex!, end: 2 } })
        expect(second.data).toBe('c')
        const request = worker.requests.at(-1)
        expect(request).toMatchObject({ type: 'execute', input: '', reuseInput: true })
        client.releaseContinuation()
        const responses: unknown[] = []
        const handle = createRegexWorkerMessageHandler((response) => responses.push(response))
        for (const message of worker.requests) handle(message)
        handle({ type: 'execute', id: 100, revision: plan.revision, input: '', reuseInput: true, continuation: { start: 1, end: 2 } })
        expect(responses.at(-1)).toMatchObject({ type: 'error', id: 100 })
    })
    it('registers a plan once per compact revision', async () => {
        const worker = new FakeWorker()
        const client = new RegexWorkerClient(() => worker)
        const executionPlan = plan()

        const first = client.execute(executionPlan, 'a')
        const firstExecute = worker.requests.find((message) => message.type === 'execute')
        expect(worker.requests.map((message) => message.type)).toEqual(['register', 'execute'])
        expect(firstExecute).toMatchObject({ revision: executionPlan.revision, input: 'a' })
        expect(firstExecute).not.toHaveProperty('entries')
        worker.respond({ type: 'result', id: firstExecute!.id, data: 'b', errors: [] })
        await expect(first).resolves.toEqual({ data: 'b', errors: [] })

        const second = client.execute(executionPlan, 'aa')
        const executeRequests = worker.requests.filter((message) => message.type === 'execute')
        expect(worker.requests.map((message) => message.type)).toEqual(['register', 'execute', 'execute'])
        worker.respond({ type: 'result', id: executeRequests[1].id, data: 'bb', errors: [] })
        await expect(second).resolves.toEqual({ data: 'bb', errors: [] })
    })

    it('retains only the current plan and re-registers after switching back', async () => {
        const worker = new FakeWorker()
        const client = new RegexWorkerClient(() => worker)
        const firstPlan = plan(20)
        const secondPlan = plan(21)

        for (const executionPlan of [firstPlan, secondPlan, firstPlan]) {
            const result = client.execute(executionPlan, 'a')
            const execute = worker.requests.at(-1)
            if (execute?.type !== 'execute') {
                throw new Error('Expected an execute request')
            }
            worker.respond({ type: 'result', id: execute.id, data: 'b', errors: [] })
            await result
        }

        expect(worker.requests
            .filter((message) => message.type === 'register')
            .map((message) => message.revision)).toEqual([20, 21, 20])
    })

    it('executes replacements in order and isolates invalid rules', async () => {
        const worker = new InMemoryWorker()
        const client = new RegexWorkerClient(() => worker)
        const executionPlan = plan()
        executionPlan.entries = [
            { ...executionPlan.entries[0], sourceIndex: 4, pattern: '[', replacement: 'broken' },
            { ...executionPlan.entries[0], sourceIndex: 5, pattern: 'a', replacement: 'b' },
            { ...executionPlan.entries[0], sourceIndex: 6, pattern: 'b', replacement: 'c' },
        ]

        const result = await client.execute(executionPlan, 'aa')

        expect(result.data).toBe('cc')
        expect(result.errors).toHaveLength(1)
        expect(result.errors[0].sourceIndex).toBe(4)
    })

    it('starts each execution deadline after the preceding request completes', async () => {
        vi.useFakeTimers()
        try {
            const worker = new FakeWorker()
            const client = new RegexWorkerClient(() => worker)
            const first = client.execute(plan(), 'first')
            const second = client.execute(plan(), 'second')
            expect(worker.requests.filter((message) => message.type === 'execute')).toHaveLength(1)
            await vi.advanceTimersByTimeAsync(1_900)
            const firstRequest = worker.requests.at(-1) as Extract<RegexWorkerRequest, { type: 'execute' }>
            worker.respond({ type: 'result', id: firstRequest.id, data: 'first-result', errors: [] })
            await first
            await vi.advanceTimersByTimeAsync(1_900)
            expect(worker.terminated).toBe(false)
            const secondRequest = worker.requests.at(-1) as Extract<RegexWorkerRequest, { type: 'execute' }>
            worker.respond({ type: 'result', id: secondRequest.id, data: 'second-result', errors: [] })
            await expect(second).resolves.toMatchObject({ data: 'second-result' })
        } finally { vi.useRealTimers() }
    })

    it('bounds startup separately without posting execution before readiness', async () => {
        vi.useFakeTimers()
        try {
            const worker = new FakeWorker(false)
            const client = new RegexWorkerClient(() => worker)
            const pending = client.execute(plan(), 'first').catch((error) => error)
            await vi.advanceTimersByTimeAsync(REGEX_WORKER_READY_TIMEOUT_MS - 1)
            expect(worker.requests).toEqual([])
            expect(worker.terminated).toBe(false)
            await vi.advanceTimersByTimeAsync(REGEX_WORKER_READY_TIMEOUT_MS)
            expect(await pending).toBeInstanceOf(RegexWorkerUnavailableError)
            expect(worker.terminated).toBe(true)
        } finally { vi.useRealTimers() }
    })

    it('distinguishes an active abort from a queued victim', async () => {
        const worker = new FakeWorker()
        const client = new RegexWorkerClient(() => worker)
        const controller = new AbortController()
        const active = client.execute(plan(), 'first', { signal: controller.signal }).catch((error) => error)
        const victim = client.execute(plan(), 'second').catch((error) => error)
        controller.abort()
        expect(await active).toMatchObject({ name: 'AbortError' })
        expect(await victim).toBeInstanceOf(RegexWorkerResetError)
    })

    it('cleans listeners and terminates an in-flight Worker on abort', async () => {
        const worker = new FakeWorker()
        const client = new RegexWorkerClient(() => worker)
        const controller = new AbortController()
        const removeListener = vi.spyOn(controller.signal, 'removeEventListener')
        const pending = client.execute(plan(), 'blocked', { signal: controller.signal })
        const outcome = pending.catch((error) => error)

        controller.abort()

        await expect(outcome).resolves.toMatchObject({ name: 'AbortError' })
        expect(removeListener).toHaveBeenCalledWith('abort', expect.any(Function))
        expect(worker.terminated).toBe(true)
        expect(worker.listenerCount).toBe(0)
    })

    it('times out at 10,000 ms, terminates without retry, and rejects stale requests', async () => {
        vi.useFakeTimers()
        try {
            const workers: FakeWorker[] = []
            const client = new RegexWorkerClient(() => {
                const worker = new FakeWorker()
                workers.push(worker)
                return worker
            })
            const timedOutPlan = plan(10)
            const timedOut = client.execute(timedOutPlan, 'blocked')
            const stale = client.execute(plan(11), 'queued')
            const timedOutOutcome = timedOut.catch((error) => error)
            const staleOutcome = stale.catch((error) => error)

            await vi.advanceTimersByTimeAsync(DEFAULT_REGEX_WORKER_TIMEOUT_MS - 1)
            expect(workers[0].terminated).toBe(false)
            await vi.advanceTimersByTimeAsync(1)

            const timeoutError = await timedOutOutcome
            expect(timeoutError).toBeInstanceOf(RegexExecutionTimeoutError)
            expect(timeoutError).toMatchObject({ category: 'regex_timeout', revision: timedOutPlan.revision })
            await expect(staleOutcome).resolves.toBeInstanceOf(RegexWorkerResetError)
            expect(workers[0].terminated).toBe(true)
            expect(workers[0].listenerCount).toBe(0)
            expect(workers).toHaveLength(1)
            expect(workers[0].requests.filter((message) => message.type === 'execute')).toHaveLength(1)

            const replacement = client.execute(timedOutPlan, 'fresh')
            expect(workers).toHaveLength(2)
            const replacementExecute = workers[1].requests.find((message) => message.type === 'execute')!
            workers[0].respond({ type: 'result', id: replacementExecute.id, data: 'stale-result', errors: [] })
            workers[1].respond({ type: 'result', id: replacementExecute.id, data: 'fresh-result', errors: [] })
            await expect(replacement).resolves.toEqual({ data: 'fresh-result', errors: [] })
        }
        finally {
            vi.useRealTimers()
        }
    })

    it('allows only parser-independent plain replacement plans and inputs', () => {
        const ordered = getRegexExecutionPlan([plannedScript('b', 'g<order 1>')], 'editoutput')
        expect(canExecuteRegexPlanInWorker(ordered, 'plain input')).toBe(true)
        expect(canExecuteRegexPlanInWorker(
            getRegexExecutionPlan([], 'editoutput'),
            'plain input',
        )).toBe(false)
        expect(canExecuteRegexPlanInWorker(
            getRegexExecutionPlan([{ ...plannedScript(), in: '' }], 'editoutput'),
            'plain input',
        )).toBe(false)

        for (const script of [
            plannedScript('@@custom directive'),
            plannedScript('b', 'g<inject>'),
            plannedScript('b', 'g<unknown_action>'),
            plannedScript('b', 'g<cbs>'),
        ]) {
            expect(canExecuteRegexPlanInWorker(
                getRegexExecutionPlan([script], 'editoutput'),
                'plain input',
            )).toBe(false)
        }

        for (const parserOpening of ['{{value}}', '{#each}', '<user>', '<CHAR>', '<BoT>']) {
            const replacementPlan = getRegexExecutionPlan([
                plannedScript(`before ${parserOpening} after`),
            ], 'editoutput')
            expect(canExecuteRegexPlanInWorker(replacementPlan, 'plain input')).toBe(false)
            expect(canExecuteRegexPlanInWorker(ordered, `before ${parserOpening} after`)).toBe(false)
        }
    })

    it('rejects parser openings that a replacement could synthesize across a boundary', () => {
        const bracePlan = getRegexExecutionPlan([
            { ...plannedScript('{'), in: 'x' },
        ], 'editoutput')
        const anglePlan = getRegexExecutionPlan([
            { ...plannedScript('<'), in: 'x' },
        ], 'editoutput')

        expect(canExecuteRegexPlanInWorker(bracePlan, 'x{user}}')).toBe(false)
        expect(canExecuteRegexPlanInWorker(anglePlan, 'xuser>')).toBe(false)

        for (const risk of ['isolated { fragment', 'isolated < fragment']) {
            expect(canExecuteRegexPlanInWorker(
                getRegexExecutionPlan([plannedScript('safe')], 'editoutput'),
                risk,
            )).toBe(false)
        }
        for (const replacement of ['isolated { fragment', 'isolated < fragment']) {
            expect(canExecuteRegexPlanInWorker(
                getRegexExecutionPlan([plannedScript(replacement)], 'editoutput'),
                'safe input',
            )).toBe(false)
        }
    })
})

describe('compiled display checkpoints through the real Worker', () => {
    it('reuses syntax with fresh request variables and discards programs on plan changes', async () => {
        const worker = new InMemoryWorker()
        const client = new RegexWorkerClient(() => worker)
        let value = 'first'
        const reads: string[] = []
        const executor = new RegexExecutor({ worker: true, skipPlainCbs: true, client,
            displayCbs: { project: () => ({ variables: { value } }), record: (keys) => reads.push(...keys) },
        })
        const first = continuationPlan([['x', '<b>{{getvar::value}}</b>']])
        const second = continuationPlan([['x', '{{equal::{{getvar::value}}::second}}']])
        const host = () => { throw new Error('Admitted checkpoints must execute in the Worker') }
        expect((await executePlan(executor, first, 'x', host)).data).toBe('<b>first</b>\n')
        value = 'second'
        expect((await executePlan(executor, first, 'x', host)).data).toBe('<b>second</b>\n')
        expect((await executePlan(executor, second, 'x', host)).data).toBe('1')
        expect((await executePlan(executor, first, 'x', host)).data).toBe('<b>second</b>\n')
        expect(reads).toEqual(['value', 'value', 'value', 'value'])
        expect(worker.requests.filter((request) => request.type === 'register')).toHaveLength(3)
        expect(worker.requests.at(-1)).toEqual({ type: 'release' })
    })

    it.each(['hello {{unknownmacro}}', 'hello <user>', '```\n{{ x }}\n```'])('batches remaining unchanged unsupported checkpoints for %s', async (input) => {
        const worker = new InMemoryWorker()
        let projections = 0
        const executor = new RegexExecutor({ worker: true, skipPlainCbs: true, client: new RegexWorkerClient(() => worker),
            displayCbs: { project: () => { projections++; return { variables: { value: 'unused' } } }, record() {} },
        })
        const plan = continuationPlan([
            ['missing', '{{getvar::value}}'],
            ...Array.from({ length: 7 }, (_, index): [string, string] => ['missing' + index, 'unused']),
        ])
        let calls = 0
        const result = await executePlan(executor, plan, input, source => { calls++; return source })
        expect(result.data).toBe(input)
        expect(calls).toBe(8)
        expect(worker.requests.filter(request => request.type === 'execute')).toHaveLength(2)
        expect(projections).toBe(1)
    })

    it('keeps compilation after generated unsupported CBS is removed by a later rule', async () => {
        const worker = new InMemoryWorker()
        const variables = { value: 'first', tail: 'done' }
        const executor = new RegexExecutor({ worker: true, skipPlainCbs: true, client: new RegexWorkerClient(() => worker),
            displayCbs: { project: () => ({ variables }), record() {} },
        })
        const plan = continuationPlan([
            ['x', '{{unknownmacro}}'], ['\\{\\{unknownmacro\\}\\}', 'clean'],
            ['clean', '{{getvar::value}}'], ['first', '{{getvar::tail}}'],
        ])
        const parse = (source: string) => source.replaceAll('{{getvar::value}}', variables.value).replaceAll('{{getvar::tail}}', variables.tail)
        const result = await executePlan(executor, plan, 'x', parse)
        expect(result.data).toBe('done')
        expect(result.data).toBe(executeRegexPlanSync(plan, 'x', parse).data)
        expect(result.errors).toEqual([])
        expect(worker.requests.filter(request => request.type === 'execute')).toHaveLength(2)
    })

    it.each([false, true])('omits redundant checkpoint input with compiled CBS %s, including retained continuations', async compiled => {
        const worker = new InMemoryWorker()
        const responses = vi.spyOn(worker, 'respond')
        const client = new RegexWorkerClient(() => worker)
        const plan = continuationPlan([['x', '{{unknown1}}'], ['unknown1', 'unknown2'], ['absent', '{{getvar::unused}}']])
        expect(plan.displayCbs).toBeDefined()
        const context = compiled ? { cbsContext: { variables: {} } } : {}
        try {
            const first = await client.execute(plan, 'x', { continuation: { start: 0, end: 2 }, ...context })
            expect(first.data).toBe('{{unknown1}}')
            const second = await client.execute(plan, first.data, { continuation: { start: first.nextIndex!, end: 2 }, ...context })
            expect(second.data).toBe('{{unknown2}}')
            const requests = worker.requests.filter(request => request.type === 'execute')
            expect(requests).toHaveLength(2)
            expect(requests[1]).toMatchObject({ reuseInput: true, input: '' })
            const results = responses.mock.calls.map(([response]) => response).filter(response => response.type === 'result')
            expect(results).toHaveLength(2)
            for (const result of results) expect(result).not.toHaveProperty('checkpointInput')
        } finally {
            client.releaseContinuation()
            responses.mockRestore()
        }
    })

    it.each([
        { name: 'plain prefix', value: 'unused', rules: [['a', 'b'], ['b', 'c{{boom}}'], ['b', 'done']], expected: 'done', hostInput: undefined },
        { name: 'empty prefix', value: 'unused', rules: [['a', ''], ['^$', '{{boom}}'], ['^$', 'done']], expected: 'done', hostInput: undefined },
        { name: 'compiled prefix', value: 'b', rules: [['a', '{{getvar::value}}'], ['b', 'c{{boom}}'], ['b', 'done']], expected: 'done', hostInput: undefined },
        { name: 'unchanged checkpoint after compiled output', value: '{{boom}}', rules: [['a', '{{getvar::value}}'], ['absent', 'unused'], ['boom', 'done']], expected: '{{done}}', hostInput: undefined },
        { name: 'host boundary after compiled output', value: 'b', rules: [['a', '{{getvar::value}}'], ['b', '@@host'], ['c', 'd{{boom}}'], ['c', 'done']], expected: 'done', hostInput: 'b' },
    ])('restores only the failing rule input after a $name', async ({ value, rules, expected, hostInput }) => {
        const plan = continuationPlan(rules as [string, string][])
        const executor = new RegexExecutor({ worker: true, skipPlainCbs: true,
            client: new RegexWorkerClient(() => new InMemoryWorker()),
            displayCbs: { project: () => ({ variables: { value } }), record() {} },
        })
        const failures: string[] = []
        const hostInputs: string[] = []
        const result = await executor.execute(plan, 'a', (source, ruleInput) => {
            if (source.includes('{{boom}}')) { failures.push(source); return ruleInput }
            return source
        }, async (_entry, input) => { hostInputs.push(input); return 'c' })
        expect(result.data).toBe(expected)
        expect(failures).toHaveLength(1)
        expect(hostInputs).toEqual(hostInput ? [hostInput] : [])
    })

    it('keeps fallback effects, conditional parsing, live reads and later host rules in order', async () => {
        let value = '{{unknown}}'
        let unchanged = 0
        const reads: string[] = []
        const events: string[] = []
        const worker = new InMemoryWorker()
        const executor = new RegexExecutor({ worker: true, skipPlainCbs: true, client: new RegexWorkerClient(() => worker),
            displayCbs: { project: () => ({ variables: { value } }), record: keys => reads.push(...keys) },
        })
        const plan = continuationPlan([
            ['x', '{{getvar::value}}'], ['absent', 'unused'], ['absent', 'unused', 'g<unknown>'],
            ['unknown', '$&', 'g<unknown>'], ['unknown', 'wrong'], ['[', 'invalid'], ['live', '@@host'], ['after', '{{getvar::value}}'],
        ])
        const result = await executor.execute(plan, 'x', source => {
            if (source === '{{unknown}}') { events.push('effect'); value = 'fresh'; return ++unchanged === 1 ? source : 'live' }
            if (source === '{{getvar::value}}') { reads.push('value'); return value }
            return source
        }, async (_entry, input) => { events.push('host:' + input); return 'after' })
        expect(result.data).toBe('fresh')
        expect(result.errors.map(error => error.sourceIndex)).toEqual([5])
        expect(events).toEqual(['effect', 'effect', 'host:live'])
        expect(reads).toEqual(['value', 'value'])
        const next = await executePlan(executor, continuationPlan([['x', '{{getvar::value}}']]), 'x', () => { throw new Error('Compilation must reset per invocation') })
        expect(next.data).toBe('fresh')
    })

    it('refreshes context after an unsupported checkpoint and never retries completed host effects', async () => {
        const worker = new InMemoryWorker()
        let value = 'before'
        let effects = 0
        const executor = new RegexExecutor({ worker: true, skipPlainCbs: true,
            client: new RegexWorkerClient(() => worker),
            displayCbs: { project: () => ({ variables: { value } }), record() {} },
        })
        const plan = continuationPlan([
            ['x', '{{getvar::value}}'],
            ['before', '{{effect}}'],
            ['next', '{{getvar::value}}'],
        ])
        const result = await executePlan(executor, plan, 'x', (source) => {
            expect(source).toBe('{{effect}}')
            effects++
            value = 'after'
            return 'next'
        })
        expect(result.data).toBe('after')
        expect(effects).toBe(1)
    })

    it('stops unchanged unsupported checkpoints before a later replacement can erase their effects', async () => {
        const plan = continuationPlan([
            ['x', '{{getvar::value}}'],
            ['absent', 'unused'],
            ['effect', 'erased'],
        ])
        const executor = new RegexExecutor({ worker: true, skipPlainCbs: true,
            client: new RegexWorkerClient(() => new InMemoryWorker()),
            displayCbs: { project: () => ({ variables: { value: '{{effect}}' } }), record() {} },
        })
        let effects = 0
        const result = await executePlan(executor, plan, 'x', (source) => {
            expect(source).toBe('{{effect}}')
            effects++
            return 'effect'
        })
        expect(result.data).toBe('erased')
        expect(effects).toBe(1)
    })
    it('discards stale variable snapshots without replaying preceding host effects', async () => {
        let value = 'old'
        let effects = 0
        let reached!: () => void
        const paused = new Promise<void>((resolve) => { reached = resolve })
        class PausingWorker extends InMemoryWorker {
            private held?: RegexWorkerResponse
            private pause = true
            override respond(response: RegexWorkerResponse): void {
                if (this.pause && response.type === 'result' && response.cbsReads?.length) {
                    this.pause = false
                    this.held = response
                    reached()
                } else super.respond(response)
            }
            resume(): void { super.respond(this.held!) }
        }
        const worker = new PausingWorker()
        const executor = new RegexExecutor({ worker: true, skipPlainCbs: true,
            client: new RegexWorkerClient(() => worker),
            displayCbs: { project: () => ({ variables: { value } }), record() {} },
        })
        const pending = executePlan(executor, continuationPlan([
            ['x', '{{effect}}'], ['next', '{{getvar::value}}'],
        ]), 'x', (source) => {
            if (source === '{{effect}}') { effects++; return 'next' }
            expect(source).toBe('{{getvar::value}}')
            return value
        })
        await paused
        value = 'fresh'
        worker.resume()
        expect((await pending).data).toBe('fresh')
        expect(effects).toBe(1)
    })

})
