import { afterEach, describe, expect, it, vi } from 'vitest'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'

vi.mock('../parser/parser.svelte', async () => (await import('../parser/parser.testSupport')).parserStubModule())
import { selectedCharID } from '../stores.svelte'
import { captureNativeGenerationMessageEdit, doingChat, reserveGeneration } from '../process/generationState'
import type { Database, Message } from './database.svelte'
import { getDatabase, normalizeDatabaseDefaults, setDatabaseLite } from './database.svelte'
import type { ConversationHistoryMapping } from './conversationJournalRebase'
import type { NativeGenerationGuardPhase, NativeGenerationGuardTicket } from './nativeGenerationGuard'
import { NativeGenerationUnknownTicketError, type NativeGenerationPort, type NativeGenerationProof, type NativeGenerationRouteStatus } from './nativeGenerationPort'
import { IndexedDbPersistentDataStore } from './indexedDbPersistentDataStore'
import type { ContentChangeKey, DataRevision, PersistentDataStore, WorkingSetCommit } from './persistentDataStore'
import { createPersistentDataRuntime, type PersistentDataRuntime } from './persistentDataRuntime'
import { registerGeneratingConversation } from './generatingConversationRegistry'
import { createProductionStateAdapter } from './persistentDataRuntime.svelte'
import { PersistentMutationFencedError } from './saveCoordinator'
import { projectCompleteScalableWorkingSet } from './workingSetCatalog'
import { workingSetResidency } from './workingSetResidency'

afterEach(() => {
    vi.restoreAllMocks()
    workingSetResidency.clear()
    workingSetResidency.setEvictionAllowed(true)
    doingChat.set(false)
})

const TARGET = { characterId: 'char-b', conversationId: 'b-chat-1' }
const MESSAGES_KEY = JSON.stringify(['messages', 'char-b', 'b-chat-1'])
const VARIABLES_KEY = JSON.stringify(['conversation', 'char-b', 'b-chat-1', 'scriptstate'])
const EFFECT_KEYS = [
    JSON.stringify(['root', 'statics']),
    JSON.stringify(['character', 'char-b', 'lastInteraction']),
    JSON.stringify(['conversation', 'char-b', 'b-chat-1', 'lastMemory']),
]
const row = (chatId: string, data = `${chatId} row`): Message => ({ role: 'char', data, chatId })
const NATIVE_ROW = row('native-out', 'native reply')
const chat = (id: string, rows: Message[]) => ({ id, name: id, note: '', localLore: [], message: rows })

const ticket = (sequence = '1'): NativeGenerationGuardTicket => ({ epoch: '7', sequence })
const routeStatus = (
    phase: NativeGenerationGuardPhase,
    output: NativeGenerationRouteStatus['output'],
    committedRevision: DataRevision | null = null,
    sequence = '1',
): NativeGenerationRouteStatus => ({ ticket: ticket(sequence), phase, pendingCommit: false, output, ...TARGET, committedRevision })
const proofAt = (revision: DataRevision, at: NativeGenerationProof['at'] = 'completed', affectedUnitKeys = [MESSAGES_KEY]): NativeGenerationProof =>
    ({ ticket: ticket(), revision, at, affectedUnitKeys })

/** The native generation commands as the invoke boundary answers them. */
function createNativePort(active: readonly NativeGenerationRouteStatus[]) {
    const cancels: string[] = []
    const proofs = new Map<string, NativeGenerationProof>()
    const statuses = new Map(active.map((status) => [status.ticket.sequence, status]))
    // Commands that fail, as a broken command channel would, until the test lets them answer.
    const failing = { active: false, status: false }
    let polls: (() => void)[] = []
    const port: NativeGenerationPort = {
        async active() {
            if (failing.active) throw new Error('Synthetic active list failure')
            return [...active]
        },
        async status(value, waitMs) {
            if (failing.status) throw new Error('Synthetic status failure')
            // A long poll answers once the status changes.
            if (waitMs !== undefined) await new Promise<void>((resolve) => polls.push(resolve))
            const status = statuses.get(value.sequence)
            if (!status) throw new NativeGenerationUnknownTicketError()
            return status
        },
        async proof(value) {
            const proof = proofs.get(value.sequence)
            if (!proof) throw new Error('Synthetic lost proof response')
            return proof
        },
        async cancel(value, reason) {
            cancels.push(`${value.sequence}:${reason}`)
        },
    }
    return {
        port, cancels, proofs, failing,
        forget(sequence: string) {
            statuses.delete(sequence)
        },
        publish(status: NativeGenerationRouteStatus) {
            statuses.set(status.ticket.sequence, status)
            const woken = polls
            polls = []
            for (const wake of woken) wake()
        },
    }
}

async function nativeRuntime(options: {
    windowed?: boolean
    /** A recreated renderer can discover native tickets before any character is selected. */
    initialSelection?: boolean
    active?: readonly NativeGenerationRouteStatus[]
    store?: PersistentDataStore
    /** Runs before the working set is loaded. */
    prepare?(native: ReturnType<typeof createNativePort>): void
    /** Whether loading ends with an idle save; a fenced load cannot save. */
    settle?: boolean
} = {}) {
    const windowed = options.windowed ?? false
    workingSetResidency.setEvictionAllowed(windowed)
    let store = options.store
    let initial: Database
    if (store) {
        initial = await store.materializeDatabase()
    } else {
        initial = normalizeDatabaseDefaults({} as Database)
        initial.botPresets[0].id = 'preset'
        initial.personas[0].id = 'persona'
        initial.characters = [
            { type: 'character', chaId: 'char-a', name: 'char-a', chatPage: 0, chatFolders: [], globalLore: [], chats: [chat('a-chat', [row('a1')])] },
            { type: 'character', chaId: 'char-b', name: 'char-b', chatPage: 0, chatFolders: [], globalLore: [],
                chats: [chat('b-chat-1', [row('m1'), row('m2')]), chat('b-chat-2', [row('n1')])] },
        ] as unknown as Database['characters']
        store = new IndexedDbPersistentDataStore(`native-generation-${crypto.randomUUID()}`, new IDBFactory(), IDBKeyRange) as PersistentDataStore
        await store.open()
        await store.replaceFromDatabase(initial)
    }
    store.readLibraryIdentity ??= async () => 'synthetic-native-library'
    const { revision } = await store.readRoot()
    const selected = options.initialSelection ?? true
    setDatabaseLite(projectCompleteScalableWorkingSet(initial, selected ? 'char-b' : null, revision, new Set(selected ? ['char-b'] : [])))
    selectedCharID.set(selected ? 1 : -1)
    const native = createNativePort(options.active ?? [])
    options.prepare?.(native)
    const saveFailures: unknown[] = []
    const backgroundErrors: unknown[] = []
    const runtime = createPersistentDataRuntime({
        store, state: createProductionStateAdapter(), prepareDatabase: async (value) => value,
        onLocalSaveFailure: (error) => { if (error !== null) saveFailures.push(error) },
        onBackgroundError: (error) => backgroundErrors.push(error),
        getNativeGenerationPort: () => native.port,
        nativeGenerationRetry: { attempts: 2, delayMs: 0 },
    })
    await runtime.initializeActiveWorkingSet(getDatabase())
    if (options.settle ?? true) await runtime.flushPendingDataLocally('native-generation-settle')
    expect(runtime.getSelectedConversationMode()).toBe(selected ? (windowed ? 'windowed' : 'complete') : null)
    return { store, runtime, native, saveFailures, backgroundErrors }
}

const rows = (messages: readonly Message[]) => messages.map((message) => [message.chatId, message.data])
const storedRows = async (store: PersistentDataStore, conversationId = 'b-chat-1', characterId = 'char-b') =>
    rows((await store.readConversation(characterId, conversationId))!.value.message)
const liveConversation = (conversationId = 'b-chat-1', characterId = 'char-b') =>
    getDatabase().characters.find((value) => value.chaId === characterId)!.chats.find((value) => value.id === conversationId)!
const liveRows = (conversationId = 'b-chat-1') => rows(liveConversation(conversationId).message)
/** The selected conversation's rows as the chat screen reads them, in either mode. */
async function selectedRows(runtime: PersistentDataRuntime) {
    const source = runtime.getActiveConversationViewportSource()!
    const total = source.snapshot().totalMessages
    await source.ensureRange({ startIndex: 0, limit: total, reason: 'viewport' })
    const snapshot = source.snapshot()
    return Array.from({ length: total }, (_, index) => {
        const message = snapshot.rowAt(index)!.message
        return [message.chatId, message.data]
    })
}
/** Native's output commit: one row added to the conversation, at its end unless `start` says otherwise. */
async function commitNativeRow(store: PersistentDataStore, options: {
    commit?: (input: WorkingSetCommit) => Promise<{ revision: DataRevision }>
    expectedRevision?: DataRevision
    start?: number
} = {}) {
    const commit = options.commit ?? ((input: WorkingSetCommit) => store.commit(input))
    const start = options.start ?? (await store.readConversationMetadata('char-b', 'b-chat-1'))!.value.totalMessages
    return (await commit({
        expectedRevision: options.expectedRevision ?? (await store.readRoot()).revision,
        conversations: [{ type: 'replace-range', characterId: 'char-b', conversationId: 'b-chat-1', start, deleteCount: 0, messages: [structuredClone(NATIVE_ROW)] }],
    })).revision
}
const editFirstRow = () => {
    const conversation = liveConversation()
    conversation.message[0] = { ...conversation.message[0], data: 'edited m1' }
}
/** The target once native's output commit landed on the fixture conversation. */
const OUTPUT_ROWS = [['m1', 'm1 row'], ['m2', 'm2 row'], ['native-out', 'native reply']]

describe('native generation guard in the save path', () => {
    it('rejects changed, foreign and expired preparation tokens while preserving edits', async () => {
        const { runtime, store } = await nativeRuntime()
        let expired: Parameters<typeof runtime.assertNativeGenerationPreparationCurrent>[0] | undefined
        await runtime.withPausedPersistentWrites('native-prepare-check', async token => {
            runtime.assertNativeGenerationPreparationCurrent(token)
            expect(() => runtime.assertNativeGenerationPreparationCurrent({ ...token })).toThrow(PersistentMutationFencedError)
            getDatabase().username = 'Edited during native reservation'
            runtime.markPersistentDataDirty(1)
            expect(() => runtime.assertNativeGenerationPreparationCurrent(token)).toThrow(PersistentMutationFencedError)
            expired = token
        })
        expect(() => runtime.assertNativeGenerationPreparationCurrent(expired!)).toThrow(PersistentMutationFencedError)
        await runtime.flushPendingDataLocally('retained-reservation-edit')
        expect((await store.readRoot()).value.username).toBe('Edited during native reservation')
        await runtime.withPausedPersistentWrites('native-prepare-fenced', async token => {
            runtime.markCommittedWorkingSetRefreshRequired(runtime.revision, new Error('Synthetic permission fence'))
            expect(() => runtime.assertNativeGenerationPreparationCurrent(token)).toThrow(PersistentMutationFencedError)
        })
    })

    it('EDIT: holds an edit of the target, cancels once, and saves it onto the adopted output', async () => {
        const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime()
        runtime.installNativeGenerationGuard(ticket(), TARGET)
        editFirstRow()
        runtime.markPersistentDataDirty(1)
        let flushed = false
        const flush = runtime.flushPendingDataLocally('native-edit').then(() => { flushed = true })
        await vi.waitFor(() => expect(native.cancels).toEqual(['1:edit']))
        expect(flushed).toBe(false)
        expect(await storedRows(store)).toEqual([['m1', 'm1 row'], ['m2', 'm2 row']])

        const revision = await commitNativeRow(store)
        native.proofs.set('1', proofAt(revision))
        await expect(runtime.settleNativeGeneration(routeStatus('completed', 'committed', revision))).resolves.toBe('released')
        await flush
        const expected = [['m1', 'edited m1'], ['m2', 'm2 row'], ['native-out', 'native reply']]
        expect(await storedRows(store)).toEqual(expected)
        expect(liveRows()).toEqual(expected)
        expect(native.cancels).toEqual(['1:edit'])
        expect(runtime.getNativeGenerationGuards()).toEqual([])
        expect(saveFailures).toEqual([])
        expect(backgroundErrors).toEqual([])
    })

    it.each([
        'before the output commit',
        'across the output commit',
        'after the target is selected again',
    ] as const)('EDIT: rebases a windowed journal edit held %s from the baseline it was made against', async (order) => {
        const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime({ windowed: true })
        // The native store maps the recorded baseline onto the history with the reply placed first.
        store.rebaseConversationHistory = async (request) => {
            expect([request.characterId, request.conversationId]).toEqual(['char-b', 'b-chat-1'])
            return { kind: 'mapped', originalCount: 2, currentCount: 3, spans: [{ originalStart: 0, currentStart: 1, count: 2 }] } satisfies ConversationHistoryMapping
        }
        runtime.installNativeGenerationGuard(ticket(), TARGET)
        // The send keeps its target registered until the reply is adopted.
        const unregister = registerGeneratingConversation(TARGET)
        try {
            if (order === 'after the target is selected again') {
                await expect(runtime.activateConversation('b-chat-2')).resolves.toBe(true)
                await expect(runtime.activateConversation('b-chat-1')).resolves.toBe(true)
            }
            const controller = await runtime.captureWindowedMessageMutation(runtime.captureSelectedConversationTarget()!, 0, row('m1'))
            // The output can land while the edit is open; the baseline it was opened on is superseded then.
            let revision = order === 'across the output commit' ? await commitNativeRow(store, { start: 0 }) : null
            expect(controller!.applyRange(0, 1, [row('m1', 'edited m1')], 'edit')).toBe(true)
            controller!.release()
            expect(native.cancels).toEqual(['1:edit'])
            const flush = runtime.flushPendingDataLocally('native-windowed-edit')

            revision ??= await commitNativeRow(store, { start: 0 })
            native.proofs.set('1', proofAt(revision))
            await expect(runtime.settleNativeGeneration(routeStatus('completed', 'committed', revision))).resolves.toBe('released')
            await flush
        } finally {
            unregister()
        }
        const expected = [['native-out', 'native reply'], ['m1', 'edited m1'], ['m2', 'm2 row']]
        expect(await storedRows(store)).toEqual(expected)
        expect(runtime.getSelectedConversationMode()).toBe('windowed')
        expect(await selectedRows(runtime)).toEqual(expected)
        expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(native.cancels).toEqual(['1:edit'])
        expect(saveFailures).toEqual([])
        expect(backgroundErrors).toEqual([])
    })

    it('PLUG: holds a plugin write to the target while plugin storage and other conversations save', async () => {
        const { store, runtime, native } = await nativeRuntime()
        runtime.installNativeGenerationGuard(ticket(), TARGET)
        let pluginSaved = false
        const plugin = runtime.commitPersistentUnitIntent('plugin-setChatToIndex', [], [], [{ ...TARGET, messages: [row('p1', 'plugin row')] }])
            .then(() => { pluginSaved = true })
        await vi.waitFor(() => expect(native.cancels).toEqual(['1:plugin']))

        await runtime.mutatePersistentPluginStorage('plugin-storage', [{ type: 'set', owner: 'synthetic-plugin', key: 'counter', value: 3 }])
        await runtime.commitPersistentUnitIntent('plugin-setChatToIndex', [], [], [{ characterId: 'char-b', conversationId: 'b-chat-2', messages: [row('q1', 'other plugin row')] }])
        expect(await store.readPluginStorage('synthetic-plugin', 'counter')).toMatchObject({ value: 3 })
        expect(await storedRows(store, 'b-chat-2')).toEqual([['q1', 'other plugin row']])
        expect(await storedRows(store)).toEqual([['m1', 'm1 row'], ['m2', 'm2 row']])
        expect(pluginSaved).toBe(false)

        native.proofs.set('1', proofAt((await store.readRoot()).revision, 'cancelled', []))
        await expect(runtime.settleNativeGeneration(routeStatus('cancelled', 'none'))).resolves.toBe('released')
        await plugin
        expect(await storedRows(store)).toEqual([['p1', 'plugin row']])
        expect(native.cancels).toEqual(['1:plugin'])
    })

    it.each([
        { windowed: false, terminal: 'failed' }, { windowed: true, terminal: 'failed' },
        { windowed: false, terminal: 'cancelled' }, { windowed: true, terminal: 'cancelled' },
    ] as const)('preserves effects and later foreign variables through saves and $terminal settlement (windowed=$windowed)', async ({ windowed, terminal }) => {
        const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime({ windowed })
        const statics = { messages: 41, imports: 3 }
        const effectKeys = [...EFFECT_KEYS, VARIABLES_KEY]
        const nativeVariables = { $__internal_ka_synthetic: 'true', other: 'preserved' }
        const issuedVariables = terminal === 'cancelled'
            ? { ...nativeVariables, $__internal_ka_synthetic: 'foreign before issued', $unrelated: 'foreign' } : nativeVariables
        await runtime.withPausedPersistentWrites('native-send', async (token) => {
            runtime.installNativeGenerationGuard(ticket(), TARGET, effectKeys)
            const { revision } = await store.commit({
                expectedRevision: token.revision,
                rootMutations: [{ type: 'set', key: 'statics', value: statics }],
                unitMutations: [{ type: 'set', key: EFFECT_KEYS[1], value: 1700 }, { type: 'set', key: EFFECT_KEYS[2], value: 'native memory' },
                    { type: 'set', key: VARIABLES_KEY, value: nativeVariables }],
            })
            if (terminal === 'cancelled') {
                await store.commit({ expectedRevision: revision, unitMutations: [{ type: 'set', key: VARIABLES_KEY, value: issuedVariables }] })
            }
            await runtime.adoptNativeGenerationCommitInQueue(token, proofAt(revision, 'issued', effectKeys))
        })
        expect(getDatabase().statics).toEqual(statics)
        expect(getDatabase().characters[1].lastInteraction).toBe(1700)
        expect(liveConversation().lastMemory).toBe('native memory')
        expect(liveConversation().scriptstate).toEqual(issuedVariables)

        // A character import during the wait counts itself in the root record the send also wrote.
        getDatabase().statics.imports += 1
        getDatabase().username = 'Edited during the wait'
        runtime.markPersistentDataDirty(1)
        let flushed = false
        const flush = runtime.flushPendingDataLocally('unrelated-root-edit').then(() => { flushed = true })
        await vi.waitFor(() => expect(flushed).toBe(true))
        await flush
        expect((await store.readRoot()).value).toMatchObject({ username: 'Edited during the wait', statics: { messages: 41, imports: 4 } })
        expect((await store.readConversation(TARGET.characterId, TARGET.conversationId))!.value.scriptstate).toEqual(issuedVariables)
        expect(runtime.getNativeGenerationGuards()).toMatchObject([{ ...TARGET, state: 'holding', cancelled: false }])

        const proofRevision = (await store.readRoot()).revision
        const terminalVariables = { ...issuedVariables, $__internal_ka_synthetic: 'foreign before terminal', $later: 'retained' }
        await store.commit({ expectedRevision: proofRevision, unitMutations: [{ type: 'set', key: VARIABLES_KEY, value: terminalVariables }] })
        native.proofs.set('1', proofAt(proofRevision, terminal, effectKeys))
        await expect(runtime.settleNativeGeneration(routeStatus(terminal, 'none', proofRevision))).resolves.toBe('released')
        expect(liveConversation().scriptstate).toEqual(terminalVariables)
        expect(runtime.getNativeGenerationGuards()).toEqual([])
        const afterAdoptionVariables = { ...terminalVariables, $__internal_ka_synthetic: 'foreign after terminal adoption', $latest: 'retained' }
        await store.commit({ expectedRevision: (await store.readRoot()).revision,
            unitMutations: [{ type: 'set', key: VARIABLES_KEY, value: afterAdoptionVariables }] })
        getDatabase().username = 'Edited after settlement'
        runtime.markPersistentDataDirty(1)
        await runtime.flushPendingDataLocally('after-variable-settlement')
        expect((await store.readConversation(TARGET.characterId, TARGET.conversationId))!.value.scriptstate).toEqual(afterAdoptionVariables)
        expect((await store.readRoot()).value.username).toBe('Edited after settlement')
        expect(native.cancels).toEqual([])
        expect(saveFailures).toEqual([])
        expect(backgroundErrors).toEqual([])
    })

    describe('race loser', () => {
        /** A native commit that lands between a write's capture and its commit. */
        type Racer = (store: PersistentDataStore, commit: PersistentDataStore['commit'], expectedRevision: DataRevision) =>
            Promise<{ revision: DataRevision; key: ContentChangeKey }>
        const outputRacer: Racer = async (store, commit, expectedRevision) => ({
            revision: await commitNativeRow(store, { commit, expectedRevision }),
            key: { kind: 'conversation', key1: 'char-b', key2: 'b-chat-1' },
        })
        /** Lands the next racer before each raced commit attempt, and records the target rows each attempt saw. */
        function raceCommits(store: PersistentDataStore, raced: (input: WorkingSetCommit) => boolean, racers: readonly Racer[]) {
            const changes: { revision: DataRevision; key: ContentChangeKey }[] = []
            const acquire = store.acquireRevision.bind(store)
            // The change index the native store keeps; this store has none.
            vi.spyOn(store, 'acquireRevision').mockImplementation(async (revision) => {
                const lease = await acquire(revision)
                lease.readWorkingSetChangePage = async (after, afterKey) =>
                    afterKey ? [] : changes.filter((change) => change.revision > after && change.revision <= revision).map((change) => change.key)
                return lease
            })
            const commit = store.commit.bind(store)
            const race = { revisions: [] as DataRevision[], attempts: [] as string[][] }
            vi.spyOn(store, 'commit').mockImplementation(async (input) => {
                if (raced(input)) {
                    race.attempts.push(liveConversation().message.map((message) => message.chatId!))
                    const racer = racers[race.attempts.length - 1]
                    if (racer) {
                        const change = await racer(store, commit, input.expectedRevision)
                        changes.push(change)
                        race.revisions.push(change.revision)
                    }
                }
                return commit(input)
            })
            return race
        }
        const rootCommit = (input: WorkingSetCommit) => input.root !== undefined || !!input.rootMutations?.length
        /** Native's output commit lands between a root flush's capture and its commit. */
        const raceRootFlush = (store: PersistentDataStore) => raceCommits(store, rootCommit, [outputRacer])

        it('parks a flush that lost the race to the output commit and retries it once after adoption', async () => {
            const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime()
            runtime.installNativeGenerationGuard(ticket(), TARGET)
            const race = raceRootFlush(store)
            getDatabase().username = 'Raced root edit'
            runtime.markPersistentDataDirty(1)
            const flush = runtime.flushPendingDataLocally('race-loser')
            await vi.waitFor(() => expect(race.revisions).toHaveLength(1))
            native.proofs.set('1', proofAt(race.revisions[0]))
            await expect(runtime.settleNativeGeneration(routeStatus('completed', 'committed', race.revisions[0]))).resolves.toBe('released')
            await flush
            // The retry is sent only once the output row is in the working set.
            expect(race.attempts).toEqual([['m1', 'm2'], ['m1', 'm2', 'native-out']])
            expect((await store.readRoot()).value.username).toBe('Raced root edit')
            expect(await storedRows(store)).toEqual(OUTPUT_ROWS)
            expect(native.cancels).toEqual([])
            expect(saveFailures).toEqual([])
            expect(backgroundErrors).toEqual([])
        })

        it('fences a flush that loses the race again, and saves it once the refresh adopts what it lost to', async () => {
            const { store, runtime, native, saveFailures } = await nativeRuntime()
            const other = { characterId: 'char-a', conversationId: 'a-chat' }
            runtime.installNativeGenerationGuard(ticket(), TARGET)
            runtime.installNativeGenerationGuard(ticket('2'), other)
            const otherRacer: Racer = async (_store, commit, expectedRevision) => ({
                revision: (await commit({ expectedRevision, conversations: [{ type: 'replace-range', ...other, start: 1, deleteCount: 0, messages: [row('native-a', 'other reply')] }] })).revision,
                key: { kind: 'conversation', key1: 'char-a', key2: 'a-chat' },
            })
            const race = raceCommits(store, rootCommit, [outputRacer, otherRacer])
            getDatabase().username = 'Raced root edit'
            runtime.markPersistentDataDirty(1)
            const flush = runtime.flushPendingDataLocally('race-loser-twice')
            await vi.waitFor(() => expect(race.revisions).toHaveLength(1))
            native.proofs.set('1', proofAt(race.revisions[0]))
            await expect(runtime.settleNativeGeneration(routeStatus('completed', 'committed', race.revisions[0]))).resolves.toBe('released')
            await expect(flush).rejects.toBeInstanceOf(PersistentMutationFencedError)
            expect(race.attempts).toEqual([['m1', 'm2'], ['m1', 'm2', 'native-out']])
            expect(runtime.pendingWorkingSetRefreshRevision).not.toBeNull()
            expect(() => runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
            expect((await store.readRoot()).value.username).not.toBe('Raced root edit')
            expect(runtime.getNativeGenerationGuards()).toMatchObject([{ ...other, state: 'holding', cancelled: false }])

            // While the other ticket still runs, a refresh keeps the fence and the raced edit.
            native.publish({ ...routeStatus('running', 'none', null, '2'), ...other })
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'refresh-required' })
            expect(getDatabase().username).toBe('Raced root edit')
            expect(runtime.getNativeGenerationGuards()).toMatchObject([{ ...other, state: 'holding' }])

            const otherRevision = race.revisions[1]
            native.proofs.set('2', { ticket: ticket('2'), revision: otherRevision, at: 'completed', affectedUnitKeys: [JSON.stringify(['messages', 'char-a', 'a-chat'])] })
            native.publish({ ...routeStatus('completed', 'committed', otherRevision, '2'), ...other })
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
            await runtime.flushPendingDataLocally('race-loser-twice-recovered')
            expect((await store.readRoot()).value.username).toBe('Raced root edit')
            expect(await storedRows(store, 'a-chat', 'char-a')).toEqual([['a1', 'a1 row'], ['native-a', 'other reply']])
            expect(await storedRows(store)).toEqual(OUTPUT_ROWS)
            expect(runtime.getNativeGenerationGuards()).toEqual([])
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            expect(native.cancels).toEqual([])
            expect(saveFailures).toEqual([])
        })

        it('parks a unit intent that lost the race to the output commit until the adoption', async () => {
            const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime()
            runtime.installNativeGenerationGuard(ticket(), TARGET)
            const race = raceCommits(store, (input) => input.conversations?.some((change) => change.type !== 'reorder' && change.conversationId === 'b-chat-2') ?? false, [outputRacer])
            const intent = runtime.commitPersistentUnitIntent('plugin-setChatToIndex', [], [], [{ characterId: 'char-b', conversationId: 'b-chat-2', messages: [row('q1', 'other plugin row')] }])
            await vi.waitFor(() => expect(race.revisions).toHaveLength(1))
            native.proofs.set('1', proofAt(race.revisions[0]))
            await expect(runtime.settleNativeGeneration(routeStatus('completed', 'committed', race.revisions[0]))).resolves.toBe('released')
            await intent
            // The retry is sent only once the output row is in the working set.
            expect(race.attempts).toEqual([['m1', 'm2'], ['m1', 'm2', 'native-out']])
            expect(await storedRows(store, 'b-chat-2')).toEqual([['q1', 'other plugin row']])
            expect(await storedRows(store)).toEqual(OUTPUT_ROWS)
            expect(liveRows()).toEqual(OUTPUT_ROWS)
            expect(native.cancels).toEqual([])
            expect(saveFailures).toEqual([])
            expect(backgroundErrors).toEqual([])
        })

        it('fences the parked flush when the adoption fails, keeping the edit for the retry', async () => {
            const { store, runtime, native } = await nativeRuntime()
            runtime.installNativeGenerationGuard(ticket(), TARGET)
            const race = raceRootFlush(store)
            getDatabase().username = 'Raced root edit'
            runtime.markPersistentDataDirty(1)
            const flush = runtime.flushPendingDataLocally('race-loser-fenced')
            await vi.waitFor(() => expect(race.revisions).toHaveLength(1))
            const settled = routeStatus('completed', 'committed', race.revisions[0])
            await expect(runtime.settleNativeGeneration(settled)).resolves.toBe('fenced')
            await expect(flush).rejects.toBeInstanceOf(PersistentMutationFencedError)
            expect(runtime.pendingWorkingSetRefreshRevision).not.toBeNull()
            expect(getDatabase().username).toBe('Raced root edit')
            expect((await store.readRoot()).value.username).not.toBe('Raced root edit')

            native.publish(settled)
            native.proofs.set('1', proofAt(race.revisions[0]))
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
            expect(liveRows()).toEqual([['m1', 'm1 row'], ['m2', 'm2 row'], ['native-out', 'native reply']])
            await runtime.flushPendingDataLocally('race-loser-recovered')
            expect((await store.readRoot()).value.username).toBe('Raced root edit')
            expect(runtime.getNativeGenerationGuards()).toEqual([])
        })
    })

    describe('LOST', () => {
        it.each([false, true])('recovers preparation effects and normalized IDs without an output row, preserving held edits (reload=%s)', async reload => {
            const first = await nativeRuntime()
            await first.store.commit({ expectedRevision: (await first.store.readRoot()).revision,
                conversations: [{ type: 'replace-range', ...TARGET, start: 1, deleteCount: 1,
                    messages: [{ role: 'char', data: 'needs an ID' }] }] })
            const { store, runtime, native } = await nativeRuntime({ store: first.store,
                active: reload ? [routeStatus('running', 'none')] : [] })
            if (!reload) runtime.installNativeGenerationGuard(ticket(), TARGET, EFFECT_KEYS)
            editFirstRow()
            runtime.markPersistentDataDirty(1)
            const flush = runtime.flushPendingDataLocally('preparation-held-edit')
            const failedFlush = expect(flush).rejects.toBeInstanceOf(PersistentMutationFencedError)
            void failedFlush.catch(() => {})
            await vi.waitFor(() => expect(native.cancels).toEqual(['1:edit']))
            const { revision } = await store.commit({ expectedRevision: (await store.readRoot()).revision,
                unitMutations: [{ type: 'set', key: EFFECT_KEYS[0], value: { ...getDatabase().statics, messages: 73 } }],
                conversations: [{ type: 'replace-range', ...TARGET, start: 1, deleteCount: 1,
                    messages: [row('normalized-m2', 'needs an ID')] }] })
            const settled = routeStatus('cancelled', 'none', revision)
            native.publish(settled)
            if (!reload) await expect(runtime.settleNativeGeneration(settled)).resolves.toBe('fenced')
            await failedFlush
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'refresh-required' })
            expect(runtime.getNativeGenerationGuards()).toMatchObject([{ state: 'fenced' }])
            expect(liveConversation().message[0].data).toBe('edited m1')
            native.proofs.set('1', proofAt(revision, 'cancelled', [MESSAGES_KEY, EFFECT_KEYS[0]]))
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
            expect(getDatabase().statics.messages).toBe(73)
            expect(liveRows()).toEqual([['m1', 'edited m1'], ['normalized-m2', 'needs an ID']])
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            expect(runtime.getNativeGenerationGuards()).toEqual([])
            await runtime.flushPendingDataLocally('recovered-preparation')
            expect(await storedRows(store)).toEqual([['m1', 'edited m1'], ['normalized-m2', 'needs an ID']])
        })

        it('fences a lost adoption without saving stale target rows, keeps it fenced while the proof stays lost, then adopts', async () => {
            const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime()
            runtime.installNativeGenerationGuard(ticket(), TARGET)
            editFirstRow()
            runtime.markPersistentDataDirty(1)
            const flush = runtime.flushPendingDataLocally('lost-adoption')
            await vi.waitFor(() => expect(native.cancels).toEqual(['1:edit']))
            const revision = await commitNativeRow(store)
            const settled = routeStatus('completed', 'committed', revision)
            await expect(runtime.settleNativeGeneration(settled)).resolves.toBe('fenced')
            await expect(flush).rejects.toBeInstanceOf(PersistentMutationFencedError)
            await expect(runtime.flushPendingDataLocally('lost-adoption-again')).rejects.toBeInstanceOf(PersistentMutationFencedError)
            const committed = [['m1', 'm1 row'], ['m2', 'm2 row'], ['native-out', 'native reply']]
            expect(await storedRows(store)).toEqual(committed)
            expect(liveRows()).toEqual([['m1', 'edited m1'], ['m2', 'm2 row']])

            native.publish(settled)
            // A refresh while the proof cannot be read keeps the fence and the held edit.
            for (const click of [1, 2]) {
                await expect(runtime.retryCommittedWorkingSetRefresh(), `refresh ${click}`).resolves.toMatchObject({ projection: 'refresh-required' })
                expect(runtime.getNativeGenerationGuards()).toMatchObject([{ ...TARGET, state: 'fenced' }])
                expect(liveRows()).toEqual([['m1', 'edited m1'], ['m2', 'm2 row']])
                expect(await storedRows(store)).toEqual(committed)
            }
            expect(() => runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
            native.proofs.set('1', proofAt(revision))
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
            expect(native.cancels).toEqual(['1:edit'])
            await runtime.flushPendingDataLocally('lost-adoption-recovered')
            expect(await storedRows(store)).toEqual([['m1', 'edited m1'], ['m2', 'm2 row'], ['native-out', 'native reply']])
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            expect(saveFailures).toEqual([])
            // The lost response is reported once, with the fence.
            expect(backgroundErrors).toEqual([expect.objectContaining({ message: 'Synthetic lost proof response' })])
        })

        it('adopts a recovered native reply after windowed selection creates pending activation changes', async () => {
            const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime({
                windowed: true, initialSelection: false, active: [routeStatus('running', 'none')], settle: false,
            })
            expect(runtime.getSelectedConversationMode()).toBeNull()
            expect(runtime.getNativeGenerationGuards()).toMatchObject([{ ...TARGET, state: 'holding' }])
            const selectionRevision = runtime.revision
            const nativeInteraction = 1700
            const nativeMemory = 'recovered native memory'
            const nativeStatics = { messages: 73, imports: 3 }
            let outputRevision: DataRevision | undefined
            // The native boundary maps only this exact append, checked against real immutable leases.
            store.rebaseConversationHistory = async request => {
                expect([request.characterId, request.conversationId]).toEqual([TARGET.characterId, TARGET.conversationId])
                expect(request.baseline.revision).toBe(selectionRevision)
                expect(request.current.revision).toBe(outputRevision)
                const original = await request.baseline.readConversation(TARGET.characterId, TARGET.conversationId)
                const current = await request.current.readConversation(TARGET.characterId, TARGET.conversationId)
                expect(original?.revision).toBe(selectionRevision)
                expect(current?.revision).toBe(outputRevision)
                expect(original?.value.id).toBe(TARGET.conversationId)
                expect(current?.value.id).toBe(TARGET.conversationId)
                expect(rows(original!.value.message)).toEqual([['m1', 'm1 row'], ['m2', 'm2 row']])
                expect(rows(current!.value.message)).toEqual(OUTPUT_ROWS)
                expect((await request.baseline.readCharacter(TARGET.characterId))!.value.lastInteraction).not.toBe(nativeInteraction)
                expect((await request.current.readCharacter(TARGET.characterId))!.value.lastInteraction).toBe(nativeInteraction)
                expect(current!.value.lastMemory).toBe(nativeMemory)
                return { kind: 'mapped', originalCount: original!.value.message.length,
                    currentCount: current!.value.message.length,
                    spans: [{ originalStart: 0, currentStart: 0, count: original!.value.message.length }] } satisfies ConversationHistoryMapping
            }
            const { characterFormatUpdate } = await import('../characters')
            await expect(runtime.activateCharacter(TARGET.characterId, {
                normalize: candidate => characterFormatUpdate(candidate, { updateInteraction: true }),
            })).resolves.toBe(true)
            expect(runtime.getSelectedConversationMode()).toBe('windowed')
            const activationInteraction = getDatabase().characters.find(character => character.chaId === TARGET.characterId)!.lastInteraction
            expect(activationInteraction).toBeDefined()
            expect((await store.readCharacter(TARGET.characterId))!.value.lastInteraction).not.toBe(activationInteraction)
            expect(await selectedRows(runtime)).toEqual([['m1', 'm1 row'], ['m2', 'm2 row']])

            const revision = await commitNativeRow(store, {
                commit: input => store.commit({ ...input, unitMutations: [
                    { type: 'set', key: EFFECT_KEYS[0], value: nativeStatics },
                    { type: 'set', key: EFFECT_KEYS[1], value: nativeInteraction },
                    { type: 'set', key: EFFECT_KEYS[2], value: nativeMemory },
                ] }),
            })
            outputRevision = revision
            native.proofs.set('1', proofAt(revision, 'completed', [MESSAGES_KEY, ...EFFECT_KEYS]))
            native.publish(routeStatus('completed', 'committed', revision))
            await vi.waitFor(() => {
                expect(backgroundErrors).toEqual([])
                expect(runtime.getNativeGenerationGuards()).toEqual([])
            })
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            expect(runtime.getSelectedConversationMode()).toBe('windowed')
            expect(await selectedRows(runtime)).toEqual(OUTPUT_ROWS)
            expect(await storedRows(store)).toEqual(OUTPUT_ROWS)
            expect(getDatabase().statics).toEqual(nativeStatics)
            expect(liveConversation().lastMemory).toBe(nativeMemory)
            expect(getDatabase().characters.find(character => character.chaId === TARGET.characterId)!.lastInteraction).toBe(activationInteraction)
            await runtime.flushPendingDataLocally('recovered-windowed-activation')
            expect((await store.readRoot()).value.statics).toEqual(nativeStatics)
            expect((await store.readConversation(TARGET.characterId, TARGET.conversationId))!.value.lastMemory).toBe(nativeMemory)
            expect((await store.readCharacter(TARGET.characterId))!.value.lastInteraction).toBe(activationInteraction)
            expect(await storedRows(store)).toEqual(OUTPUT_ROWS)
            expect(native.cancels).toEqual([])
            expect(saveFailures).toEqual([])
            expect(backgroundErrors).toEqual([])
        })

        it('guards a ticket native still runs after a reload and adopts it when the status settles', async () => {
            const first = await nativeRuntime()
            const running = routeStatus('running', 'none')
            const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime({ store: first.store, active: [running] })
            expect(runtime.getNativeGenerationGuards()).toMatchObject([{ ...TARGET, state: 'holding' }])
            expect(captureNativeGenerationMessageEdit(TARGET)).toBeNull()
            const unrelatedSend = reserveGeneration()!
            try {
                // Recovering a native ticket never grants an unrelated local send permission to edit it.
                expect(captureNativeGenerationMessageEdit(TARGET)).toBeNull()
            } finally { unrelatedSend.release() }
            editFirstRow()
            runtime.markPersistentDataDirty(1)
            const flush = runtime.flushPendingDataLocally('reloaded-edit')
            await vi.waitFor(() => expect(native.cancels).toEqual(['1:edit']))

            const revision = await commitNativeRow(store)
            native.proofs.set('1', proofAt(revision))
            native.publish(routeStatus('cancelled', 'committed', revision))
            await flush
            expect(await storedRows(store)).toEqual([['m1', 'edited m1'], ['m2', 'm2 row'], ['native-out', 'native reply']])
            expect(runtime.getNativeGenerationGuards()).toEqual([])
            expect(saveFailures).toEqual([])
            expect(backgroundErrors).toEqual([])
        })

        it('fences a reloaded ticket whose status stays unreadable, and reloads its target once native no longer knows it', async () => {
            const first = await nativeRuntime()
            const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime({
                store: first.store, active: [routeStatus('running', 'none')], settle: false,
                prepare: (value) => { value.failing.status = true },
            })
            await vi.waitFor(() => expect(runtime.getNativeGenerationGuards()).toMatchObject([{ ...TARGET, state: 'fenced' }]))
            expect(runtime.pendingWorkingSetRefreshRevision).not.toBeNull()
            expect(() => runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
            // The fence reports why the status could not be read, once.
            expect(backgroundErrors).toEqual([expect.objectContaining({ message: 'Synthetic status failure' })])
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'refresh-required' })
            expect(runtime.getNativeGenerationGuards()).toMatchObject([{ ...TARGET, state: 'fenced' }])

            // Native committed the reply and then dropped the ticket.
            await commitNativeRow(store)
            native.forget('1')
            native.failing.status = false
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
            expect(liveRows()).toEqual(OUTPUT_ROWS)
            expect(runtime.getNativeGenerationGuards()).toEqual([])
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            expect(native.cancels).toEqual([])
            editFirstRow()
            runtime.markPersistentDataDirty(1)
            await runtime.flushPendingDataLocally('after-unknown-ticket')
            expect(await storedRows(store)).toEqual([['m1', 'edited m1'], ['m2', 'm2 row'], ['native-out', 'native reply']])
            expect(saveFailures).toEqual([])
        })

        it.each([
            { windowed: false, withOutput: false }, { windowed: false, withOutput: true },
            { windowed: true, withOutput: false }, { windowed: true, withOutput: true },
        ])("pauses editing while native's ticket list cannot be read after a reload, then guards the ticket it lists (windowed=$windowed, output=$withOutput)", async ({ windowed, withOutput }) => {
            const first = await nativeRuntime()
            const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime({
                store: first.store, windowed, active: [routeStatus('running', 'none')], settle: false,
                prepare: (value) => { value.failing.active = true },
            })
            expect(runtime.getNativeGenerationGuards()).toEqual([])
            expect(runtime.pendingWorkingSetRefreshRevision).not.toBeNull()
            expect(() => runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
            expect(backgroundErrors).toEqual([expect.objectContaining({ message: 'Synthetic active list failure' })])
            // A refresh while the list still cannot be read keeps editing paused.
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'refresh-required' })
            expect(() => runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)

            native.failing.active = false
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            expect(runtime.getNativeGenerationGuards()).toMatchObject([{ ...TARGET, state: 'holding' }])
            if (windowed) {
                store.rebaseConversationHistory = async () => ({ kind: 'mapped', originalCount: 2,
                    currentCount: withOutput ? 3 : 2, spans: [{ originalStart: 0, currentStart: 0, count: 2 }] })
                const controller = await runtime.captureWindowedMessageMutation(runtime.captureSelectedConversationTarget()!, 0, row('m1'))
                expect(controller!.applyRange(0, 1, [row('m1', 'edited m1')], 'edit')).toBe(true)
                controller!.release()
            } else {
                editFirstRow()
                runtime.markPersistentDataDirty(1)
            }
            const flush = runtime.flushPendingDataLocally('reinstalled-edit')
            await vi.waitFor(() => expect(native.cancels).toEqual(['1:edit']))
            expect(await storedRows(store)).toEqual([['m1', 'm1 row'], ['m2', 'm2 row']])
            const revision = withOutput ? await commitNativeRow(store) : (await store.readRoot()).revision
            native.proofs.set('1', proofAt(revision, 'cancelled', withOutput ? [MESSAGES_KEY] : []))
            native.publish(routeStatus('cancelled', withOutput ? 'committed' : 'none', revision))
            await flush
            const expected = [['m1', 'edited m1'], ['m2', 'm2 row'], ...(withOutput ? [['native-out', 'native reply']] : [])]
            expect(await storedRows(store)).toEqual(expected)
            expect(await selectedRows(runtime)).toEqual(expected)
            if (windowed) expect((await store.readConversationMetadata(TARGET.characterId, TARGET.conversationId))?.value.conversation.isStreaming).toBe(false)
            expect(runtime.getNativeGenerationGuards()).toEqual([])
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            expect(saveFailures).toEqual([])
        })

        it('persists windowed reload normalization without a message edit or native output', async () => {
            const first = await nativeRuntime()
            const { store, runtime, native, saveFailures } = await nativeRuntime({
                store: first.store, windowed: true, active: [routeStatus('running', 'none')], settle: false,
                prepare: (value) => { value.failing.active = true },
            })
            expect((await store.readConversationMetadata(TARGET.characterId, TARGET.conversationId))?.value.conversation.isStreaming).toBeUndefined()
            native.failing.active = false
            await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
            expect(liveConversation().isStreaming).toBe(false)
            store.rebaseConversationHistory = async () => ({ kind: 'mapped', originalCount: 2,
                currentCount: 2, spans: [{ originalStart: 0, currentStart: 0, count: 2 }] })
            const revision = (await store.readRoot()).revision
            native.proofs.set('1', proofAt(revision, 'cancelled', []))
            const status = routeStatus('cancelled', 'none', revision)
            native.publish(status)
            await expect(runtime.settleNativeGeneration(status)).resolves.toBe('released')
            await runtime.flushPendingDataLocally('reload-normalization-without-edit')
            expect((await store.readConversationMetadata(TARGET.characterId, TARGET.conversationId))?.value.conversation.isStreaming).toBe(false)
            const expected = [['m1', 'm1 row'], ['m2', 'm2 row']]
            expect(await storedRows(store)).toEqual(expected)
            expect(await selectedRows(runtime)).toEqual(expected)
            expect(runtime.getNativeGenerationGuards()).toEqual([])
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            expect(saveFailures).toEqual([])
        })
    })

    it.each(['library-change', 'replacement'] as const)('does not bless an old native ticket through a refresh after %s', async kind => {
        const { store, runtime, native } = await nativeRuntime()
        runtime.installNativeGenerationGuard(ticket(), TARGET)
        const original = await store.materializeDatabase()
        if (kind === 'library-change') {
            vi.spyOn(store, 'readLibraryIdentity').mockResolvedValue('different-synthetic-library')
        } else {
            original.username = 'Authoritative replacement'
            await expect(runtime.replacePersistentDatabase(original, 'replacement-with-old-ticket', { authoritative: true }))
                .resolves.toMatchObject({ projection: 'applied' })
        }
        const revision = (await store.readRoot()).revision
        await expect(runtime.refreshActiveWorkingSetFromStore(revision)).resolves.toMatchObject({ projection: 'applied' })
        native.proofs.set('1', proofAt(revision, 'cancelled', []))
        await expect(runtime.settleNativeGeneration(routeStatus('cancelled', 'none', revision))).resolves.toBe('fenced')
        expect(runtime.getNativeGenerationGuards()).toMatchObject([{ state: 'fenced' }])
        expect(runtime.pendingWorkingSetRefreshRevision).not.toBeNull()
        expect(await storedRows(store)).toEqual([['m1', 'm1 row'], ['m2', 'm2 row']])
        expect(getDatabase().username).toBe(original.username)
    })

    it('reloads the committed target when the held edits cannot be placed on the output, and saving resumes', async () => {
        const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime()
        runtime.installNativeGenerationGuard(ticket(), TARGET)
        // A row added during the wait has no chatId to replay it by.
        liveConversation().message.push({ role: 'user', data: 'typed during the wait' })
        runtime.markPersistentDataDirty(1)
        const flush = runtime.flushPendingDataLocally('unplaceable-edit')
        await vi.waitFor(() => expect(native.cancels).toEqual(['1:edit']))
        const revision = await commitNativeRow(store)
        const settled = routeStatus('completed', 'committed', revision)
        native.proofs.set('1', proofAt(revision))
        await expect(runtime.settleNativeGeneration(settled)).resolves.toBe('fenced')
        await expect(flush).rejects.toBeInstanceOf(PersistentMutationFencedError)
        // The refresh prompt is up and the failure reported while the held row stays on screen.
        expect(runtime.pendingWorkingSetRefreshRevision).not.toBeNull()
        expect(backgroundErrors).toEqual([expect.objectContaining({ name: 'NativeGenerationAdoptionError', reason: 'unplaceable-edit' })])
        expect(liveRows()).toEqual([['m1', 'm1 row'], ['m2', 'm2 row'], [undefined, 'typed during the wait']])

        native.publish(settled)
        await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
        // Like any committed refresh, the retry loads the stored conversation: the held row is not saved.
        expect(liveRows()).toEqual(OUTPUT_ROWS)
        expect(await storedRows(store)).toEqual(OUTPUT_ROWS)
        expect(runtime.getNativeGenerationGuards()).toEqual([])
        expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
        // A settled ticket is not asked to stop again.
        expect(native.cancels).toEqual(['1:edit'])
        editFirstRow()
        runtime.markPersistentDataDirty(1)
        await runtime.flushPendingDataLocally('after-reload')
        expect(await storedRows(store)).toEqual([['m1', 'edited m1'], ['m2', 'm2 row'], ['native-out', 'native reply']])
        expect(saveFailures).toEqual([])
    })

    it('fences a ticket that does not settle within the close bound, and a refresh stops it and reloads the target', async () => {
        const { store, runtime, native } = await nativeRuntime()
        runtime.installNativeGenerationGuard(ticket(), TARGET)
        await expect(runtime.closeNativeGenerations(20)).resolves.toEqual({ released: [], fenced: [ticket()] })
        expect(native.cancels).toEqual(['1:lifecycle'])
        expect(runtime.pendingWorkingSetRefreshRevision).not.toBeNull()
        expect(runtime.getNativeGenerationGuards()).toMatchObject([{ ...TARGET, state: 'fenced', cancelled: true }])
        getDatabase().username = 'Edited after close'
        expect(() => runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
        await expect(runtime.flushPendingDataLocally('after-close')).rejects.toBeInstanceOf(PersistentMutationFencedError)
        expect((await store.readRoot()).value.username).not.toBe('Edited after close')
        expect(getDatabase().username).toBe('Edited after close')

        // Native still runs the ticket after the retry bound.
        native.publish(routeStatus('running', 'none'))
        await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
        expect(native.cancels).toEqual(['1:lifecycle', '1:authority'])
        expect(runtime.getNativeGenerationGuards()).toEqual([])
        expect(getDatabase().username).toBe((await store.readRoot()).value.username)
        getDatabase().username = 'Edited after refresh'
        runtime.markPersistentDataDirty(1)
        await runtime.flushPendingDataLocally('after-refresh')
        expect((await store.readRoot()).value.username).toBe('Edited after refresh')
    })

    it('releases a ticket that settles within the close bound', async () => {
        const { store, runtime, native, saveFailures } = await nativeRuntime()
        runtime.installNativeGenerationGuard(ticket(), TARGET)
        const closing = runtime.closeNativeGenerations(60_000)
        expect(native.cancels).toEqual(['1:lifecycle'])
        const revision = await commitNativeRow(store)
        native.proofs.set('1', proofAt(revision))
        await expect(runtime.settleNativeGeneration(routeStatus('cancelled', 'committed', revision))).resolves.toBe('released')
        await expect(closing).resolves.toEqual({ released: [ticket()], fenced: [] })
        expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(liveRows()).toEqual(OUTPUT_ROWS)
        getDatabase().username = 'Edited after close'
        runtime.markPersistentDataDirty(1)
        await runtime.flushPendingDataLocally('after-close')
        expect((await store.readRoot()).value.username).toBe('Edited after close')
        expect(saveFailures).toEqual([])
    })

    it.each([true, false])('NAV: navigates during the wait and adopts the reply into the old conversation only (windowed=%s)', async (windowed) => {
        const { store, runtime, native, saveFailures, backgroundErrors } = await nativeRuntime({ windowed })
        await runtime.withPausedPersistentWrites('native-send', async (token) => {
            runtime.installNativeGenerationGuard(ticket(), TARGET, EFFECT_KEYS)
            const { revision } = await store.commit({
                expectedRevision: token.revision,
                unitMutations: [{ type: 'set', key: EFFECT_KEYS[1], value: 1700 }],
            })
            await runtime.adoptNativeGenerationCommitInQueue(token, proofAt(revision, 'issued', EFFECT_KEYS))
        })
        await expect(runtime.activateConversation('b-chat-2')).resolves.toBe(true)
        await runtime.flushPendingDataLocally('navigation')
        expect(native.cancels).toEqual([])
        expect((await store.readCharacter('char-b'))!.value.chatPage).toBe(1)

        const revision = await commitNativeRow(store)
        native.proofs.set('1', proofAt(revision))
        await expect(runtime.settleNativeGeneration(routeStatus('completed', 'committed', revision))).resolves.toBe('released')
        expect(runtime.captureSelectedConversationTarget()).toMatchObject({ characterId: 'char-b', conversationId: 'b-chat-2' })
        expect(await selectedRows(runtime)).toEqual([['n1', 'n1 row']])
        expect(await storedRows(store, 'b-chat-2')).toEqual([['n1', 'n1 row']])
        expect(await storedRows(store)).toEqual([['m1', 'm1 row'], ['m2', 'm2 row'], ['native-out', 'native reply']])
        await expect(runtime.activateConversation('b-chat-1')).resolves.toBe(true)
        expect(await selectedRows(runtime)).toEqual([['m1', 'm1 row'], ['m2', 'm2 row'], ['native-out', 'native reply']])
        expect(native.cancels).toEqual([])
        expect(saveFailures).toEqual([])
        expect(backgroundErrors).toEqual([])
    })
})
