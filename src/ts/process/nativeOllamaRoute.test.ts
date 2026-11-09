// @vitest-environment happy-dom
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { bootChatInteractionApp, interactionCharacterId as characterId, interactionConversationId as conversationId } from '../../lib/ChatScreens/chatInteractionApp.testSupport'
import type { NativeGenerationStatus, NativeGenerationTicket } from '../storage/nativeGeneration'
import type { PersistentRevisionLease } from '../storage/persistentDataStore'
import type { ConversationHistoryMapping } from '../storage/conversationJournalRebase'
import type { GenerationEndRecord } from './generationEnd'
import { caseUnicode, localTimeZone, regexUnicode, tailoredCase } from './nativeHostCapture'
import { nativeTokenizeAnswer } from '../tokenizer.testSupport'

const boundary = vi.hoisted(() => ({ native: false, mobile: false, invoke: vi.fn() }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule(() => ({
    runtime: boundary.native ? 'tauri' : 'web', os: boundary.mobile ? 'android' : 'windows',
})))
vi.mock('@tauri-apps/api/core', async original => ({
    ...await original<typeof import('@tauri-apps/api/core')>(), invoke: boundary.invoke,
}))

let app: Awaited<ReturnType<typeof bootChatInteractionApp>> | undefined
let mounted: Record<string, any> | undefined
let popupMounted: Record<string, any> | undefined
beforeAll(async () => {
    const warm = await bootChatInteractionApp({ realChat: true })
    await import('./index.svelte')
    await import('../../lib/ChatScreens/Chat.svelte')
    await import('../../lib/ChatScreens/DefaultChatScreen.svelte')
    warm.restore()
}, 180_000)
beforeEach(() => { boundary.native = false; boundary.mobile = false; boundary.invoke.mockReset() })
afterEach(async () => {
    const generation = await import('./generationState')
    const navigationAllowed = generation.isNativeGenerationNavigationAllowed()
    const unmounts = await Promise.allSettled([
        ...(mounted && app ? [app.svelte.unmount(mounted)] : []),
        ...(popupMounted && app ? [app.svelte.unmount(popupMounted)] : []),
    ])
    try {
        app?.restore()
    } finally {
        mounted = undefined; popupMounted = undefined; app = undefined
        document.body.replaceChildren()
        boundary.native = false
        vi.restoreAllMocks()
    }
    expect(navigationAllowed).toBe(false)
    for (const result of unmounts) if (result.status === 'rejected') throw result.reason
})

async function fixture(seed: Parameters<typeof bootChatInteractionApp>[0] = {}) {
    app = await bootChatInteractionApp(seed)
    const current = app
    const runtime = current.runtime
    const database = current.database.getDatabase()
    database.aiModel = 'ollama-hosted'
    database.notification = false
    database.playMessage = false
    database.theme = 'cardboard'
    database.risunestChatEditPopup = false
    const device = await import('../storage/deviceSettings')
    const markers = new Map<string, string>([['risuNestNativeOllamaGeneration', 'true']])
    device.loadDeviceSettings({ getItem: key => markers.get(key) ?? null,
        setItem: (key, value) => { markers.set(key, value) }, removeItem: key => { markers.delete(key) }, flush: async () => {} })
    await runtime.flushPendingDataLocally('native-route-fixture')
    const facade = await import('../storage/nativeGeneration')
    const leases = await import('../storage/nativePersistentExport')
    const production = await import('../storage/persistentDataRuntime.svelte')
    const { SqlitePersistentDataStore } = await import('../storage/sqlitePersistentDataStore')
    Object.assign(runtime.store, { readLibraryIdentity: async () => 'synthetic-library',
        lwwBindingState: async () => ({ targetAuthority: '1' }),
        rebaseConversationHistory: SqlitePersistentDataStore.prototype.rebaseConversationHistory })
    const acquire = vi.mocked(runtime.store.acquireRevision).getMockImplementation()!
    let leaseId = 0
    const fixtureStore = current.raw
    const leaseRegistry = new Map<string, { lease: PersistentRevisionLease; revision: number; store: typeof fixtureStore }>()
    vi.spyOn(runtime.store, 'acquireRevision').mockImplementation(async revision => {
        const lease = await acquire(revision)
        const token = `synthetic-lease-${++leaseId}`
        const release = lease.release.bind(lease)
        Object.assign(lease, { [leases.nativePersistentRevisionLease]: token, release: async () => {
            leaseRegistry.delete(token)
            await release()
        } })
        leaseRegistry.set(token, { lease, revision, store: fixtureStore })
        return lease
    })
    production.configurePersistentDataRuntime({ nativeGeneration: facade.createNativeGenerationPort() })
    const ticket: NativeGenerationTicket = { epoch: '1', sequence: '1' }
    let status: NativeGenerationStatus = {
        ...{ characterId, conversationId }, ticket, phase: 'reserved', messageId: 'native-answer',
        checkpoint: '0', committedRevision: null, pendingCommit: false, preparationReady: false,
        preparationError: null, dispatch: 'none', output: 'none', cancelDecision: null,
        failure: null, diagnosticsAvailable: false,
    }
    let effectsCommitted = false
    let outputCommitted = false
    const commands: string[] = []
    const cancelReasons: string[] = []
    const reserveRequests: Record<string, any>[] = []
    const wake: (() => void)[] = []
    const cancellationWaiters: (() => void)[] = []
    let executionFinished = false
    const options = { holdCancellation: false, unsupported: '', lostExecuteReply: false, proofFailure: false, rejectExecute: false, preparationFailure: false,
        onReserve: undefined as undefined | (() => void), onExecute: undefined as undefined | (() => void),
        onProof: undefined as undefined | (() => Promise<void>) }
    const publish = () => { for (const resolve of wake.splice(0)) resolve() }
    boundary.invoke.mockImplementation(async (command: string, args: Record<string, any> = {}) => {
        // The tokenizer's own command, answered as the device answers it and kept out of the route's log.
        if (command === 'tokenize_batch') return nativeTokenizeAnswer(args.request)
        commands.push(command)
        switch (command) {
            case 'pds_rebase_conversation_history': {
                const request = args.request
                const baseline = leaseRegistry.get(request?.baselineLease)
                const latest = leaseRegistry.get(request?.currentLease)
                const live = (entry: typeof baseline, token: string) => entry && leaseRegistry.get(token) === entry
                    && entry.store === fixtureStore && entry.lease.revision === entry.revision
                    && leases.hasNativePersistentRevisionLease(entry.lease) && entry.lease[leases.nativePersistentRevisionLease] === token
                if (!baseline || !latest || !live(baseline, request.baselineLease) || !live(latest, request.currentLease)
                    || request.characterId !== characterId || request.conversationId !== conversationId
                    || request.baselineRevision !== baseline.revision || request.currentRevision !== latest.revision
                    || baseline.revision !== latest.revision) throw new Error('Synthetic native history requires live leases at the same revision')
                const [original, currentMetadata] = await Promise.all([
                    baseline.lease.readConversationMetadata(characterId, conversationId),
                    latest.lease.readConversationMetadata(characterId, conversationId),
                ])
                if (!live(baseline, request.baselineLease) || !live(latest, request.currentLease)
                    || !original || !currentMetadata || original.revision !== baseline.revision || currentMetadata.revision !== latest.revision
                    || original.value.characterId !== characterId || original.value.conversationId !== conversationId
                    || currentMetadata.value.characterId !== characterId || currentMetadata.value.conversationId !== conversationId
                    || original.value.conversation.id !== conversationId || currentMetadata.value.conversation.id !== conversationId
                    || !Number.isSafeInteger(original.value.totalMessages) || original.value.totalMessages < 0
                    || original.value.totalMessages !== currentMetadata.value.totalMessages
                    || JSON.stringify(original.value) !== JSON.stringify(currentMetadata.value)) throw new Error('Synthetic native history metadata mismatch')
                // Equal revisions of the same retained store prove unchanged history, not just equal counts.
                const count = original.value.totalMessages
                return { kind: 'mapped', originalCount: count, currentCount: count,
                    spans: count ? [{ originalStart: 0, currentStart: 0, count }] : [] } satisfies ConversationHistoryMapping
            }
            case 'native_generation_host_init': return
            case 'native_generation_active': return []
            case 'native_generation_reserve_ollama':
                reserveRequests.push(args.request)
                options.onReserve?.()
                return options.unsupported ? { unsupported: options.unsupported } : { ticket }
            case 'native_generation_capture_witness': {
                const original = leaseRegistry.get(args.original.token)
                if (!original || original.store !== fixtureStore || original.revision !== args.original.revision
                    || original.lease.revision !== original.revision || !leases.hasNativePersistentRevisionLease(original.lease)
                    || original.lease[leases.nativePersistentRevisionLease] !== args.original.token) {
                    throw new Error('Synthetic native witness requires its live captured lease')
                }
                const metadata = await original.lease.readConversationMetadata(characterId, conversationId)
                if (leaseRegistry.get(args.original.token) !== original || !metadata || metadata.revision !== original.revision
                    || metadata.value.characterId !== characterId || metadata.value.conversationId !== conversationId
                    || metadata.value.conversation.id !== conversationId || !Number.isSafeInteger(metadata.value.totalMessages)
                    || metadata.value.totalMessages < 0) throw new Error('Synthetic native witness metadata is inconsistent')
                return { ticket, originalRevision: original.revision, originalCount: metadata.value.totalMessages, rows: [] }
            }
            case 'native_generation_execute': {
                try {
                    options.onExecute?.()
                    if (options.rejectExecute) throw new Error('Synthetic execute not delivered')
                    const { revision } = await current.raw.commit({ expectedRevision: (await current.raw.readRoot()).revision,
                        unitMutations: [{ type: 'set', key: '["root","statics"]', value: { ...database.statics, messages: 73 } }] })
                    effectsCommitted = true
                    status = { ...status, committedRevision: revision }
                    // Preparation finishing cannot reopen a cancellation accepted while storage was pending.
                    if ((status.phase !== 'reserved' && status.phase !== 'running') || status.cancelDecision !== null) return status
                    status = { ...status, phase: 'running', dispatch: 'issued', preparationReady: true }
                    if (options.preparationFailure) status = { ...status, phase: 'failed', dispatch: 'none',
                        failure: { category: 'sdk', stage: 'prepare', field: null, httpStatus: null, sdkMessage: 'Synthetic preparation failure' } }
                    if (options.lostExecuteReply) throw new Error('Synthetic lost execute response')
                    return status
                } finally {
                    executionFinished = true
                }
            }
            case 'native_generation_status':
                if (args.after && status.phase === 'running') await new Promise<void>(resolve => wake.push(resolve))
                return status
            case 'native_generation_cancel':
                cancelReasons.push(args.reason)
                if (options.holdCancellation) await new Promise<void>(resolve => { cancellationWaiters.push(resolve) })
                status = { ...status, phase: 'cancelled', cancelDecision: args.reason }
                publish()
                return status
            case 'native_generation_proof':
                await options.onProof?.()
                if (options.proofFailure) throw new Error('Synthetic unreadable proof')
                return { ticket, revision: (await current.raw.readRoot()).revision,
                    at: status.phase === 'running' ? 'issued' : status.phase,
                    affectedUnitKeys: [...(effectsCommitted ? ['["root","statics"]'] : []),
                        ...(outputCommitted ? [JSON.stringify(['messages', characterId, conversationId])] : [])],
                    outputMessageId: outputCommitted ? status.messageId : null,
                    receipts: { normalization: null, effects: [], output: null },
                    idDeltas: [], mappingUnavailable: false, failedWithOutput: status.phase === 'failed' && outputCommitted }
            default: throw new Error(`Unexpected boundary command: ${command}`)
        }
    })
    boundary.native = true
    const { sendChat } = await import('./index.svelte')
    return {
        current, runtime, database, sendChat, commands, cancelReasons, reserveRequests, options, ticket,
        executionFinished: () => executionFinished,
        status: () => status,
        releaseCancellation: () => { for (const release of cancellationWaiters.splice(0)) release() },
        pending: () => vi.waitFor(() => expect(wake.length).toBeGreaterThan(0)),
        async finish(failed = false, withOutput = true) {
            if (withOutput) {
                const metadata = await current.raw.readConversationMetadata(characterId, conversationId)
                const { revision } = await current.raw.commit({ expectedRevision: (await current.raw.readRoot()).revision,
                    conversations: [{ type: 'replace-range', characterId, conversationId,
                        start: metadata!.value.totalMessages, deleteCount: 0,
                        messages: [{ role: 'char', data: 'Synthetic native reply', chatId: status.messageId }] }] })
                outputCommitted = true
                status = { ...status, output: 'committed', committedRevision: revision }
            }
            status = { ...status, phase: failed ? 'failed' : 'completed',
                failure: failed ? { category: 'http', stage: 'body', field: null, httpStatus: 503, sdkMessage: 'Synthetic provider failure' } : null }
            publish()
        },
        async visibleReply() {
            const source = runtime.getActiveConversationViewportSource()!
            const index = source.snapshot().totalMessages - 1
            await source.ensureRange({ startIndex: index, limit: 1, reason: 'viewport' })
            const row = source.snapshot().rowAt(index)?.message
            return row?.chatId === 'native-answer' ? row.data : undefined
        },
        async storedReply() {
            return (await current.raw.readConversationWindow({ characterId, conversationId,
                anchorMessageId: 'native-answer', before: 0, after: 0, limit: 1 }))?.value.messages
                .find(row => row.chatId === 'native-answer')?.data
        },
        /** Answers a JS send with one Ollama reply. */
        stubProvider() {
            return vi.spyOn(globalThis, 'fetch').mockImplementation(async () => new Response(
                JSON.stringify({ model: 'synthetic-model', done: true, message: { role: 'assistant', content: 'Synthetic TS reply' } }) + '\n',
                { status: 200, headers: { 'Content-Type': 'application/json' } }))
        },
        async lastStoredMessage() {
            await runtime.flushPendingDataLocally('native-route-last-message')
            const total = (await current.raw.readConversationMetadata(characterId, conversationId))!.value.totalMessages
            return (await current.raw.readConversationWindow({ characterId, conversationId,
                startIndex: total - 1, limit: 1 }))!.value.messages[0]
        },
    }
}

async function openMessageEditor(h: Awaited<ReturnType<typeof fixture>>, popup = false) {
    const { current, runtime } = h
    current.database.getDatabase().risunestChatEditPopup = popup
    const { createSelectedConversationOperations } = await import('../selectedConversationOperations')
    const { default: Chat } = await import('../../lib/ChatScreens/Chat.svelte')
    const source = runtime.getActiveConversationViewportSource()!
    await source.ensureRange({ startIndex: 4999, limit: 1, reason: 'viewport' })
    const snapshot = source.snapshot()
    const row = snapshot.rowAt(4999)!
    expect(row).toBeDefined()
    const operations = createSelectedConversationOperations({
        captureSelectedConversationTarget: () => runtime.captureSelectedConversationTarget(),
        acquireCompleteConversation: (reason, target) => runtime.acquireCompleteConversation(reason, target),
        captureWindowedMessageMutation: (target, index, evidence) => runtime.captureWindowedMessageMutation(target, index, evidence),
        getCurrentSession: () => runtime.getActiveConversationSession(),
        getCurrentViewportSource: () => runtime.getActiveConversationViewportSource(),
        captureCurrent: () => {
            const character = current.database.getDatabase().characters[get(current.stores.selectedCharID)]
            const conversation = character?.chats[character.chatPage]
            return character && conversation ? { character, conversation } : null
        },
    })
    const target = document.createElement('div')
    document.body.append(target)
    mounted = current.svelte.mount(Chat, { target, props: {
        message: row.message.data, name: 'Synthetic owner', character: current.stores.createSimpleCharacter(current.selected()), role: row.message.role,
        idx: row.absoluteIndex, totalLength: snapshot.totalMessages, isLastMemory: false,
        viewportRow: row, viewportSourceToken: snapshot.sourceToken, selectedConversationOperations: operations,
        captureViewportTarget: () => null,
    } })
    const editButton = await vi.waitFor(() => {
        const button = target.querySelector<HTMLButtonElement>('.button-icon-edit')
        expect(button).not.toBeNull()
        return button!
    })
    editButton.click()
    let editorTarget = target
    let save = () => editButton.click()
    if (popup) {
        boundary.mobile = true
        const { textEditorPopup } = await import('../gui/textEditorPopup.svelte')
        const { default: Popup } = await import('../../lib/Others/TextEditorPopup.svelte')
        expect(textEditorPopup.request).not.toBeNull()
        editorTarget = document.createElement('div')
        document.body.append(editorTarget)
        popupMounted = current.svelte.mount(Popup, { target: editorTarget, props: { request: textEditorPopup.request! } })
        await current.svelte.tick()
        const { language } = await import('../../lang')
        save = () => [...editorTarget.querySelectorAll('button')].find(button => button.textContent?.trim() === language.risuNest.textEditor.save)!.click()
    }
    const editor = await vi.waitFor(() => {
        const element = editorTarget.querySelector<HTMLTextAreaElement>('textarea')
        expect(element).not.toBeNull()
        return element!
    })
    const input = (value: string) => {
        editor.value = value
        editor.dispatchEvent(new Event('input', { bubbles: true }))
    }
    return { target: editorTarget, editor, input, save }
}

async function storedEditedRow(h: Awaited<ReturnType<typeof fixture>>) {
    return (await h.current.raw.readConversationWindow({ characterId, conversationId,
        startIndex: 4999, limit: 1 }))!.value.messages[0].data
}

describe('ordinary native Ollama Send', () => {
    it('saves the screen message editor during native generation started by its Send button', async () => {
        const h = await fixture({ realChat: true })
        h.options.holdCancellation = true
        const { default: DefaultChatScreen } = await import('../../lib/ChatScreens/DefaultChatScreen.svelte')
        const generation = await import('./generationState')
        const target = document.createElement('div')
        document.body.append(target)
        mounted = h.current.svelte.mount(DefaultChatScreen, { target })
        let stage = 'composer'
        let cancelOwnedGeneration: (() => void) | null = null
        const failures: unknown[] = []
        const failureStates: unknown[] = []
        const captureFailureState = () => {
            try {
                const status = h.status()
                const guards = h.runtime.getNativeGenerationGuards()
                return {
                    stage, phase: status.phase, dispatch: status.dispatch, pendingCommit: status.pendingCommit,
                    cancelReasons: h.cancelReasons.slice(-8), omittedCancelReasons: Math.max(0, h.cancelReasons.length - 8),
                    commands: h.commands.slice(-32), omittedCommands: Math.max(0, h.commands.length - 32),
                    busy: get(generation.doingChat), cancellationAvailable: get(generation.generationCancellation) !== null,
                    cancelButtons: target.querySelectorAll('button[aria-labelledby="cancel"]').length,
                    rows: target.querySelectorAll('[data-chat-id="interaction-4999"]').length,
                    editors: target.querySelectorAll('[data-chat-id="interaction-4999"] textarea').length,
                    guards: guards.slice(0, 8).map(guard => ({ state: guard.state, cancelled: guard.cancelled })),
                    omittedGuards: Math.max(0, guards.length - 8),
                }
            } catch {
                return { stage, diagnosticCaptureFailed: true }
            }
        }
        try {
            const input = await vi.waitFor(() => {
                const element = target.querySelector<HTMLTextAreaElement>('textarea.input-text')
                expect(element).not.toBeNull()
                return element!
            })
            input.value = 'Synthetic screen Send question'
            input.dispatchEvent(new Event('input', { bubbles: true }))
            await h.current.svelte.tick()
            stage = 'native-pending'
            target.querySelector<HTMLButtonElement>('.button-icon-send')!.click()
            cancelOwnedGeneration = get(generation.generationCancellation)
            await vi.waitFor(() => expect(h.executionFinished()).toBe(true), { timeout: 10_000 })
            await h.pending()
            stage = 'message-row'
            const row = await vi.waitFor(() => {
                const element = target.querySelector<HTMLElement>('[data-chat-id="interaction-4999"]')
                expect(element).not.toBeNull()
                return element!
            })
            stage = 'open-editor'
            row.querySelector<HTMLButtonElement>('.button-icon-edit')!.click()
            const editor = await vi.waitFor(() => {
                const element = row.querySelector<HTMLTextAreaElement>('textarea')
                expect(element).not.toBeNull()
                return element!
            })
            stage = 'save-editor'
            editor.value = 'Edited after the screen Send action'
            editor.dispatchEvent(new Event('input', { bubbles: true }))
            row.querySelector<HTMLButtonElement>('.button-icon-edit')!.click()
            await vi.waitFor(() => expect(h.cancelReasons).toEqual(['edit']))
            expect(await storedEditedRow(h)).toBe('Synthetic interaction row 4999')
            stage = 'settlement'
            const flushing = h.runtime.flushPendingDataLocally('screen-native-editor-save')
            h.releaseCancellation()
            await vi.waitFor(() => expect(get(generation.doingChat)).toBe(false), { timeout: 10_000 })
            await flushing
            expect(await storedEditedRow(h)).toBe('Edited after the screen Send action')
            expect(await h.storedReply()).toBeUndefined()
            expect(h.runtime.getNativeGenerationGuards()).toEqual([])
            boundary.native = false
            const { createPersistentDataStore } = await import('../storage/persistentDataStoreFactory')
            const reopened = createPersistentDataStore()
            boundary.native = true
            await reopened.open()
            stage = 'durable-reload'
            const persisted = await reopened.readConversationWindow({ characterId, conversationId, startIndex: 4999, limit: 2 })
            expect(persisted!.value.messages.map(message => message.data)).toEqual([
                'Edited after the screen Send action', 'Synthetic screen Send question',
            ])
            expect((await reopened.readConversationMetadata(characterId, conversationId))!.value.totalMessages).toBe(5001)
        } catch (error) {
            failures.push(error)
            failureStates.push(captureFailureState())
        } finally {
            // Cleanup cancellation never contributes to the Save acceptance assertions.
            h.options.holdCancellation = false
            try {
                if (get(generation.doingChat)) {
                    target.querySelector<HTMLButtonElement>('button[aria-labelledby="cancel"]')?.click()
                    if (get(generation.doingChat)) cancelOwnedGeneration?.()
                }
            } catch (error) {
                failures.push(error)
                failureStates.push({ cleanup: true, ...captureFailureState() })
            } finally {
                h.releaseCancellation()
            }
            try {
                await vi.waitFor(() => expect(get(generation.doingChat)).toBe(false))
            } catch (error) {
                failures.push(error)
                failureStates.push({ cleanup: true, ...captureFailureState() })
            }
        }
        if (failures.length) throw new AggregateError([
            ...failures,
            new Error('Screen Send/editor diagnostic: ' + JSON.stringify(failureStates)),
        ], 'Screen Send/editor failed')
    }, 30_000)

    it.each([false, true])('saves the actual windowed editor through edit cancellation, held settlement, and durable reload (popup=%s)', async popup => {
        const h = await fixture({ realChat: true })
        h.options.holdCancellation = true
        h.database.risunestChatEditPopup = popup
        await h.runtime.flushPendingDataLocally('editor-mode')
        const controller = new AbortController()
        const sending = h.sendChat({ nativeIntent: 'ordinary', signal: controller.signal })
        try {
            await h.pending()
            const generation = await import('./generationState')
            expect(generation.captureNativeGenerationMessageEdit({ characterId, conversationId })).not.toBeNull()
            const editor = await openMessageEditor(h, popup)
            editor.input('Edited through the native Save action')
            editor.save()
            await vi.waitFor(() => expect(h.cancelReasons).toEqual(['edit']))
            expect(generation.captureNativeGenerationMessageEdit({ characterId, conversationId })).toBeNull()
            expect(generation.isNativeGenerationNavigationAllowed()).toBe(true)
            expect(await storedEditedRow(h)).toBe('Synthetic interaction row 4999')
            const flushing = h.runtime.flushPendingDataLocally('native-editor-save')
            h.releaseCancellation()
            await expect(sending).resolves.toBe(false)
            await flushing
            expect(generation.isNativeGenerationNavigationAllowed()).toBe(false)
            expect(await storedEditedRow(h)).toBe('Edited through the native Save action')
            expect(await h.storedReply()).toBeUndefined()
            expect(h.runtime.getNativeGenerationGuards()).toEqual([])
            expect(h.current.completeLeases).not.toHaveBeenCalled()
            expect(h.runtime.getSelectedConversationMode()).toBe('windowed')
            boundary.native = false
            const { createPersistentDataStore } = await import('../storage/persistentDataStoreFactory')
            const reopened = createPersistentDataStore()
            boundary.native = true
            await reopened.open()
            expect((await reopened.readConversationWindow({ characterId, conversationId, startIndex: 4999, limit: 1 }))!.value.messages[0].data)
                .toBe('Edited through the native Save action')
            expect((await reopened.readConversationMetadata(characterId, conversationId))!.value.totalMessages).toBe(5000)
        } finally {
            // Failure cleanup also drains the old-blocker witness, whose Save never cancelled.
            h.options.holdCancellation = false
            controller.abort()
            h.releaseCancellation()
            await sending.catch(() => {})
        }
    })

    it.each([false, true])('keeps a newer draft when real storage acquisition resumes after more typing (popup=%s)', async popup => {
        const h = await fixture({ realChat: true })
        h.database.risunestChatEditPopup = popup
        await h.runtime.flushPendingDataLocally('editor-mode')
        const sending = h.sendChat({ nativeIntent: 'ordinary' })
        await h.pending()
        const editor = await openMessageEditor(h, popup)
        editor.input('Earlier draft')
        let entered!: () => void
        let resume!: () => void
        const acquired = new Promise<void>(resolve => { entered = resolve })
        const wait = new Promise<void>(resolve => { resume = resolve })
        const acquire = h.runtime.store.acquireRevision.bind(h.runtime.store)
        vi.spyOn(h.runtime.store, 'acquireRevision').mockImplementationOnce(async revision => {
            entered(); await wait; return acquire(revision)
        })
        editor.save()
        await acquired
        editor.input('Newer draft stays open')
        resume()
        const { language } = await import('../../lang')
        await vi.waitFor(() => expect(get(h.current.stores.alertStore)).toMatchObject({ type: 'toast', msg: language.chatMessageActionFailed }))
        expect(editor.target.querySelector('textarea')?.value).toBe('Newer draft stays open')
        expect(h.cancelReasons).toEqual([])
        expect(await storedEditedRow(h)).toBe('Synthetic interaction row 4999')
        await h.current.svelte.tick()
        editor.save()
        await expect(sending).resolves.toBe(false)
        await h.runtime.flushPendingDataLocally('newer-native-editor-draft')
        expect(h.cancelReasons).toEqual(['edit'])
        expect(await storedEditedRow(h)).toBe('Newer draft stays open')
    })

    it.each(['cancel', 'reroll'] as const)('refuses Save when %s invalidates its admission during real storage acquisition', async invalidation => {
        const h = await fixture({ realChat: true })
        const controller = new AbortController()
        h.options.holdCancellation = true
        const sending = h.sendChat({ nativeIntent: 'ordinary', signal: controller.signal })
        await h.pending()
        const editor = await openMessageEditor(h)
        const { activeRerollConversations } = await import('../durableReroll')
        const generation = await import('./generationState')
        const capability = generation.captureNativeGenerationMessageEdit({ characterId, conversationId })!
        expect(capability).not.toBeNull()
        editor.input('Retained draft after admission expires')
        let entered!: () => void
        let resume!: () => void
        const acquired = new Promise<void>(resolve => { entered = resolve })
        const wait = new Promise<void>(resolve => { resume = resolve })
        const acquire = h.runtime.store.acquireRevision.bind(h.runtime.store)
        vi.spyOn(h.runtime.store, 'acquireRevision').mockImplementationOnce(async revision => {
            entered(); await wait; return acquire(revision)
        })
        try {
            editor.save()
            await acquired
            if (invalidation === 'cancel') {
                controller.abort()
                expect(capability.isCurrent({ characterId, conversationId })).toBe(false)
            } else {
                activeRerollConversations.set([conversationId])
                await h.current.svelte.tick()
            }
            resume()
            const { language } = await import('../../lang')
            await vi.waitFor(() => expect(get(h.current.stores.alertStore)).toMatchObject({ type: 'toast', msg: language.chatMessageActionFailed }))
            expect(editor.target.querySelector('textarea')?.value).toBe('Retained draft after admission expires')
            expect(await storedEditedRow(h)).toBe('Synthetic interaction row 4999')
            expect(h.cancelReasons).toEqual(invalidation === 'cancel' ? ['user'] : [])
            expect(h.runtime.getNativeGenerationGuards()).toMatchObject([{ state: 'holding', cancelled: false }])
            if (invalidation === 'reroll') {
                h.current.stores.alertStore.set({ type: 'none', msg: '' })
                editor.save()
                await vi.waitFor(() => expect(get(h.current.stores.alertStore)).toMatchObject({ type: 'toast', msg: language.navigationBlockedWhileGenerating }))
                expect(h.cancelReasons).toEqual([])
            }
        } finally {
            activeRerollConversations.set([])
            resume()
            h.options.holdCancellation = false
            controller.abort()
            h.releaseCancellation()
            await sending
        }
    })

    it('does not admit an editor while its real native guard is adopting or fenced', async () => {
        const h = await fixture({ realChat: true })
        const generation = await import('./generationState')
        const reservation = generation.reserveGeneration()!
        const sending = h.sendChat({ nativeIntent: 'ordinary' }, reservation)
        const failed = expect(sending).rejects.toThrow('requires recovery')
        let adoptingReached = false
        let releaseProof!: () => void
        const proofWait = new Promise<void>(resolve => { releaseProof = resolve })
        try {
            await h.pending()
            const preparedStatus = h.status()
            const editor = await openMessageEditor(h)
            editor.input('Draft retained during adoption')
            h.options.onProof = () => proofWait
            h.options.proofFailure = true
            await h.finish()
            await vi.waitFor(() => expect(h.runtime.getNativeGenerationGuards()).toMatchObject([{ state: 'adopting' }]))
            adoptingReached = true
            expect(generation.isNativeGenerationNavigationAllowed()).toBe(true)
            const { language } = await import('../../lang')
            for (const state of ['adopting', 'fenced'] as const) {
                expect(h.runtime.getNativeGenerationGuards()).toMatchObject([{ state }])
                const release = generation.allowNativeGenerationMessageEdit(reservation, {
                    runtime: h.runtime, ticket: h.ticket, status: preparedStatus, target: { characterId, conversationId },
                    authorityEpoch: h.runtime.getStorageAuthorityEpoch(),
                })
                expect(generation.captureNativeGenerationMessageEdit({ characterId, conversationId })).toBeNull()
                release()
                h.current.stores.alertStore.set({ type: 'none', msg: '' })
                editor.save()
                await vi.waitFor(() => expect(get(h.current.stores.alertStore)).toMatchObject({ type: 'toast', msg: language.navigationBlockedWhileGenerating }))
                expect(editor.target.querySelector('textarea')?.value).toBe('Draft retained during adoption')
                expect(await storedEditedRow(h)).toBe('Synthetic interaction row 4999')
                expect(h.cancelReasons).toEqual([])
                if (state === 'adopting') {
                    releaseProof()
                    await failed
                }
            }
        } finally {
            releaseProof()
            if (adoptingReached) await failed.catch(() => {})
            reservation.release()
            h.options.proofFailure = false
            await h.runtime.retryCommittedWorkingSetRefresh()
        }
    })

    it('keeps the draft and stored message when JS generation owns the busy state', async () => {
        const h = await fixture({ realChat: true })
        const generation = await import('./generationState')
        const reservation = generation.reserveGeneration()!
        const editor = await openMessageEditor(h)
        try {
            editor.input('Draft during JS generation')
            editor.save()
            const { language } = await import('../../lang')
            await vi.waitFor(() => expect(get(h.current.stores.alertStore)).toMatchObject({ type: 'toast', msg: language.navigationBlockedWhileGenerating }))
            expect(editor.target.querySelector('textarea')?.value).toBe('Draft during JS generation')
            expect(await storedEditedRow(h)).toBe('Synthetic interaction row 4999')
            expect(h.cancelReasons).toEqual([])
        } finally { reservation.release() }
    })

    it('rejects a retained editor after selection changes away from the native owner', async () => {
        const h = await fixture({ realChat: true, extraCharacters: [{ characterId: 'other-editor-owner', conversationId: 'other-editor-chat' }] })
        const sending = h.sendChat({ nativeIntent: 'ordinary' })
        await h.pending()
        const editor = await openMessageEditor(h)
        editor.input('Draft for the original owner')
        await h.current.characters.changeChar(1)
        editor.save()
        const { language } = await import('../../lang')
        await vi.waitFor(() => expect(get(h.current.stores.alertStore)).toMatchObject({ type: 'toast', msg: language.chatMessageActionFailed }))
        expect(editor.target.querySelector('textarea')?.value).toBe('Draft for the original owner')
        expect(await storedEditedRow(h)).toBe('Synthetic interaction row 4999')
        expect(h.cancelReasons).toEqual([])
        const failed = expect(sending).rejects.toThrow('without an attached output')
        await h.finish(false, false)
        await failed
    })

    it('scopes message editing to the current reservation, prepared status, live guard and authority', async () => {
        const h = await fixture()
        const generation = await import('./generationState')
        const reservation = generation.reserveGeneration()!
        const target = { characterId, conversationId }
        const capture = () => generation.captureNativeGenerationMessageEdit(target)
        const controller = new AbortController()
        h.options.onReserve = () => expect(capture()).toBeNull()
        h.options.onExecute = () => expect(capture()).toBeNull()
        const sending = h.sendChat({ nativeIntent: 'ordinary', signal: controller.signal }, reservation)
        try {
            await h.pending()
            const first = capture()!
            expect(first).not.toBeNull()
            expect(generation.captureNativeGenerationMessageEdit({ ...target, characterId: 'wrong-character' })).toBeNull()
            expect(generation.captureNativeGenerationMessageEdit({ ...target, conversationId: 'wrong-conversation' })).toBeNull()
            const options = { runtime: h.runtime, ticket: h.ticket, status: h.status(), target,
                authorityEpoch: h.runtime.getStorageAuthorityEpoch(), signal: controller.signal }
            const staleRelease = generation.allowNativeGenerationMessageEdit(reservation, options)
            const release = generation.allowNativeGenerationMessageEdit(reservation, options)
            const current = capture()!
            staleRelease()
            expect(capture()).toBe(current)
            expect(first.isCurrent(target)).toBe(false)
            release()
            expect(current.isCurrent(target)).toBe(false)
            const invalidStatuses: Partial<NativeGenerationStatus>[] = [
                { phase: 'reserved' }, { phase: 'completed' }, { phase: 'cancelled' }, { phase: 'failed' },
                { phase: 'stale' }, { phase: 'interrupted' }, { dispatch: 'none' }, { dispatch: 'uncertain' },
                { pendingCommit: true }, { preparationReady: false }, { cancelDecision: 'user' },
                { ticket: { epoch: 'wrong', sequence: '1' } }, { characterId: 'wrong' }, { conversationId: 'wrong' },
            ]
            for (const status of invalidStatuses) {
                const releaseInvalid = generation.allowNativeGenerationMessageEdit(reservation, { ...options, status: { ...options.status, ...status } })
                expect(capture()).toBeNull()
                releaseInvalid()
            }
            generation.allowNativeGenerationMessageEdit(reservation, { ...options, authorityEpoch: options.authorityEpoch + 1 })
            expect(capture()).toBeNull()
            generation.allowNativeGenerationMessageEdit({ ...reservation }, options)
            expect(capture()).toBeNull()
            const mismatchedTicket = { epoch: 'other-guard', sequence: '1' }
            generation.allowNativeGenerationMessageEdit(reservation, {
                ...options, ticket: mismatchedTicket, status: { ...options.status, ticket: mismatchedTicket },
            })
            expect(capture()).toBeNull()
            generation.allowNativeGenerationMessageEdit(reservation, options)
            const beforeAbort = capture()!
            controller.abort()
            expect(beforeAbort.isCurrent(target)).toBe(false)
            await expect(sending).resolves.toBe(false)
            expect(capture()).toBeNull()
            // A released guard cannot be recreated as a local edit permission from old status.
            generation.allowNativeGenerationMessageEdit(reservation, { ...options, signal: undefined })
            expect(capture()).toBeNull()
            reservation.release()
            const next = generation.reserveGeneration()!
            try {
                generation.allowNativeGenerationMessageEdit(reservation, { ...options, signal: undefined })
                expect(capture()).toBeNull()
                staleRelease()
            } finally { next.release() }
        } finally {
            controller.abort()
            reservation.release()
        }
    })

    it.each([false, true])('adopts effects before waiting and the committed reply without JS history loading (lost execute reply=%s)', async lost => {
        const h = await fixture()
        h.options.lostExecuteReply = lost
        const sending = h.sendChat({ nativeIntent: 'ordinary' })
        await h.pending()
        expect(h.current.database.getDatabase().statics.messages).toBe(73)
        expect(h.current.completeLeases).not.toHaveBeenCalled()
        expect(h.commands.filter(command => command === 'native_generation_execute')).toHaveLength(1)
        expect(h.commands.indexOf('native_generation_host_init')).toBeLessThan(h.commands.indexOf('native_generation_reserve_ollama'))
        // The reservation carries this engine's case mapping and RegExp tables, the local offsets around the send, and
        // whether this start excludes regex.
        const { host } = h.reserveRequests[0]
        expect(host).toEqual({ tailoredCase: tailoredCase(), caseUnicode: caseUnicode(), timeZone: localTimeZone(host.timeZone.end - 30 * 86_400_000),
            regexUnicode: regexUnicode(), regexExcluded: false })
        expect(host.timeZone.end).toBeGreaterThan(Date.now())
        await h.finish()
        await expect(sending).resolves.toBe(true)
        expect(await h.storedReply()).toBe('Synthetic native reply')
        expect(await h.visibleReply()).toBe('Synthetic native reply')
        expect(h.runtime.getNativeGenerationGuards()).toEqual([])
        h.options.unsupported = 'syncBinding'
        const provider = h.stubProvider()
        const before = h.commands.length
        await expect(h.sendChat({ nativeIntent: 'ordinary' })).resolves.toBe(true)
        expect(h.commands.slice(before)).toEqual(['native_generation_reserve_ollama'])
        expect(provider).toHaveBeenCalledTimes(1)
        expect((await h.lastStoredMessage()).data).toBe('Synthetic TS reply')
        expect(h.runtime.getNativeGenerationGuards()).toEqual([])
    }, 30_000)

    it.each([false, true])('reports native failure and retains a committed fallback row (with output=%s)', async withOutput => {
        const h = await fixture()
        const sending = h.sendChat({ nativeIntent: 'ordinary' })
        const failed = expect(sending).rejects.toThrow('Synthetic provider failure')
        await h.pending()
        await h.finish(true, withOutput)
        await failed
        expect(await h.storedReply()).toBe(withOutput ? 'Synthetic native reply' : undefined)
        expect(await h.visibleReply()).toBe(withOutput ? 'Synthetic native reply' : undefined)
        expect(h.runtime.getNativeGenerationGuards()).toEqual([])
    })

    it.each([true, false])('Stop cancels without fallback (before execute=%s)', async before => {
        const h = await fixture()
        const controller = new AbortController()
        if (before) h.options.onReserve = () => controller.abort()
        const sending = h.sendChat({ nativeIntent: 'ordinary', signal: controller.signal })
        if (!before) { await h.pending(); controller.abort() }
        await expect(sending).resolves.toBe(false)
        expect(h.cancelReasons).toContain('user')
        expect(h.commands.filter(command => command === 'native_generation_execute')).toHaveLength(before ? 0 : 1)
        expect(await h.storedReply()).toBeUndefined()
    })

    it('closes a running native owner and clears its navigation allowance after settlement', async () => {
        const h = await fixture()
        const sending = h.sendChat({ nativeIntent: 'ordinary' })
        await h.pending()
        const closing = h.runtime.closeNativeGenerations(2_000)
        await expect(sending).resolves.toBe(false)
        await expect(closing).resolves.toMatchObject({ fenced: [] })
        expect(h.cancelReasons).toEqual(['lifecycle'])
        expect(h.runtime.getNativeGenerationGuards()).toEqual([])
    })

    it('refuses unsupported intent before reserve and sends on the TS route when the reservation refuses', async () => {
        const h = await fixture()
        for (const args of [{}, { nativeIntent: 'ordinary' as const, continue: true },
            { nativeIntent: 'ordinary' as const, preview: true }, { nativeIntent: 'ordinary' as const, previewPrompt: true }]) {
            await expect(h.sendChat(args)).rejects.toThrow('ordinary Send only')
        }
        expect(h.commands).toEqual([])
        h.options.unsupported = 'syncBinding'
        const provider = h.stubProvider()
        const end = await import('./generationEnd')
        const { generatingConversations } = await import('../storage/generatingConversationRegistry')
        const records: GenerationEndRecord[] = []
        const stop = end.subscribeGenerationEnd(record => { records.push(record) })
        try {
            const run = end.beginGenerationEndRun({ characterId, conversationId })
            await expect(run.capture(() => h.sendChat({ nativeIntent: 'ordinary' }))).resolves.toBe(true)
            run.finish('completed')
            expect(h.commands).toEqual(['native_generation_host_init', 'native_generation_reserve_ollama'])
            expect(provider).toHaveBeenCalledTimes(1)
            expect((await h.lastStoredMessage()).data).toBe('Synthetic TS reply')
            expect(h.runtime.getNativeGenerationGuards()).toEqual([])
            expect(generatingConversations.snapshot()).toEqual([])
            expect(records.map(record => record.status)).toEqual(['completed'])
            // A refused send that the TS route then drops never entered generation.
            const { SelectedConversationPromotionStaleError } = await import('../storage/activeWorkingSet.svelte')
            h.current.completeLeases.mockRejectedValueOnce(new SelectedConversationPromotionStaleError())
            const dropped = end.beginGenerationEndRun({ characterId, conversationId })
            await expect(dropped.capture(() => h.sendChat({ nativeIntent: 'ordinary' }))).resolves.toBe(false)
            dropped.finish('failed')
            expect(records.map(record => record.status)).toEqual(['completed'])
            expect(provider).toHaveBeenCalledTimes(1)
        } finally { stop() }
    })

    // The native route reads the whole stored conversation, so a send whose history window applies takes the TS route.
    it.each([true, false])('sends on the TS route while the history window applies (window=%s)', async windowed => {
        const h = await fixture()
        const device = await import('../storage/deviceSettings')
        device.updateDeviceSettings({ generationHistoryLimitEnabled: windowed })
        const provider = h.stubProvider()
        const sending = h.sendChat({ nativeIntent: 'ordinary', historyLimit: true })
        if (windowed) {
            await vi.waitFor(() => expect(provider).toHaveBeenCalledTimes(1))
            await expect(sending).resolves.toBe(true)
            expect(h.commands).toEqual([])
            const source = h.runtime.getActiveConversationViewportSource()!
            const index = source.snapshot().totalMessages - 1
            await source.ensureRange({ startIndex: index, limit: 1, reason: 'viewport' })
            expect(source.snapshot().rowAt(index)?.message.data).toBe('Synthetic TS reply')
        } else {
            await h.pending()
            expect(h.commands).toContain('native_generation_reserve_ollama')
            await h.finish()
            await expect(sending).resolves.toBe(true)
            expect(provider).not.toHaveBeenCalled()
            expect(await h.storedReply()).toBe('Synthetic native reply')
        }
    })

    it('cancels a reserved ticket after an edit and preserves the edit without executing', async () => {
        const h = await fixture()
        h.options.onReserve = () => {
            h.current.database.getDatabase().username = 'Edited during reserve'
            h.runtime.markPersistentDataDirty(1)
        }
        await expect(h.sendChat({ nativeIntent: 'ordinary' })).rejects.toThrow()
        expect(h.cancelReasons).toEqual(['authority'])
        expect(h.commands).not.toContain('native_generation_execute')
        await h.runtime.flushPendingDataLocally('after-reservation-edit')
        expect((await h.current.raw.readRoot()).value.username).toBe('Edited during reserve')
    })

    it('keeps the original reply owner through real conversation and character navigation', async () => {
        const otherCharacter = 'synthetic-navigation-owner'
        const h = await fixture({ extraCharacters: [{ characterId: otherCharacter, conversationId: 'synthetic-navigation-chat' }] })
        const navigation = await import('../globalApi.svelte')
        await expect(h.current.characters.addNewChat(h.current.selected())).resolves.toBe(true)
        const otherConversation = h.runtime.captureSelectedConversationTarget()!.conversationId
        await expect(navigation.changeChatTo(conversationId)).resolves.toBe(true)
        const originalIndex = h.current.database.getDatabase().characters.findIndex(value => value.chaId === characterId)
        await h.current.characters.changeChar(originalIndex)
        await navigation.changeChatTo(conversationId)
        const sending = h.sendChat({ nativeIntent: 'ordinary' })
        await h.pending()
        await expect(navigation.changeChatTo(otherConversation)).resolves.toBe(true)
        const otherIndex = h.current.database.getDatabase().characters.findIndex(value => value.chaId === otherCharacter)
        await expect(h.current.characters.changeChar(otherIndex)).resolves.toBe(true)
        expect(h.cancelReasons).toEqual([])
        const selected = h.runtime.captureSelectedConversationTarget()
        await h.finish()
        await expect(sending).resolves.toBe(true)
        expect(h.runtime.captureSelectedConversationTarget()).toMatchObject({ characterId: selected!.characterId, conversationId: selected!.conversationId })
        expect(await h.storedReply()).toBe('Synthetic native reply')
        expect((await h.current.raw.readConversationMetadata(characterId, otherConversation))!.value.totalMessages).toBe(0)
        expect((await h.current.raw.readConversationMetadata(otherCharacter, 'synthetic-navigation-chat'))!.value.totalMessages).toBe(0)
    })

    it('settles a rejected execute that remains Reserved without holding the save queue', async () => {
        const h = await fixture()
        h.options.rejectExecute = true
        h.options.onExecute = () => {
            h.current.database.getDatabase().username = 'Held during rejected execute'
            h.runtime.markPersistentDataDirty(1)
        }
        await expect(h.sendChat({ nativeIntent: 'ordinary' })).rejects.toThrow('Synthetic execute not delivered')
        expect(h.cancelReasons).toEqual(['authority'])
        expect(h.commands.filter(command => command === 'native_generation_execute')).toHaveLength(1)
        expect(h.runtime.getNativeGenerationGuards()).toEqual([])
        await h.runtime.flushPendingDataLocally('after-rejected-execute')
        expect((await h.current.raw.readRoot()).value.username).toBe('Held during rejected execute')
        expect(await h.storedReply()).toBeUndefined()
        const { doingChat } = await import('./generationState')
        const { get } = await import('svelte/store')
        expect(get(doingChat)).toBe(false)
    })

    it('adopts preparation effects when failure happens before Issued', async () => {
        const h = await fixture()
        h.options.preparationFailure = true
        await expect(h.sendChat({ nativeIntent: 'ordinary' })).rejects.toThrow('Synthetic preparation failure')
        expect(h.current.database.getDatabase().statics.messages).toBe(73)
        expect(h.runtime.getNativeGenerationGuards()).toEqual([])
        expect(await h.storedReply()).toBeUndefined()
        expect(h.commands.filter(command => command === 'native_generation_execute')).toHaveLength(1)
    })

    it('retains a recovery fence when preparation proof cannot be read', async () => {
        const h = await fixture()
        h.options.proofFailure = true
        h.options.onExecute = () => {
            h.current.database.getDatabase().username = 'Held while proof is unavailable'
            h.runtime.markPersistentDataDirty(1)
        }
        await expect(h.sendChat({ nativeIntent: 'ordinary' })).rejects.toThrow('Synthetic unreadable proof')
        expect(h.runtime.pendingWorkingSetRefreshRevision).not.toBeNull()
        expect(h.runtime.getNativeGenerationGuards()).toMatchObject([{ state: 'fenced' }])
        await expect(h.runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'refresh-required' })
        expect(h.runtime.getNativeGenerationGuards()).toMatchObject([{ state: 'fenced' }])
        h.options.proofFailure = false
        await expect(h.runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
        expect(h.current.database.getDatabase().statics.messages).toBe(73)
        expect(h.current.database.getDatabase().username).toBe('Held while proof is unavailable')
        expect(h.runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(h.runtime.getNativeGenerationGuards()).toEqual([])
        await h.runtime.flushPendingDataLocally('after-proof-recovery')
        expect((await h.current.raw.readRoot()).value.username).toBe('Held while proof is unavailable')
        expect(h.commands.filter(command => command === 'native_generation_execute')).toHaveLength(1)
    })
})
