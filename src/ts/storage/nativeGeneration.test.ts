import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    invoke: vi.fn(),
    isTauri: true,
}))

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule(() => ({ runtime: mocks.isTauri ? 'tauri' : 'web', os: 'windows' })))

import {
    createNativeGenerationPort,
    nativeGenerationActive,
    nativeGenerationCancel,
    nativeGenerationExecute,
    nativeGenerationFailureMessage,
    nativeGenerationHostInit,
    nativeGenerationProof,
    nativeGenerationReserveOllama,
    nativeGenerationStatus,
    type NativeGenerationStatus,
} from './nativeGeneration'

import { NativeGenerationUnknownTicketError } from './nativeGenerationPort'

const ticket = { epoch: '18446744073709551615', sequence: '9007199254740993' }

function routeStatus(overrides: Partial<NativeGenerationStatus> = {}): NativeGenerationStatus {
    return {
        ticket, characterId: 'c', conversationId: 'v', phase: 'running', messageId: 'm', checkpoint: '9007199254740993',
        committedRevision: 7, pendingCommit: false, preparationReady: true, preparationError: null,
        dispatch: 'issued', output: 'none', cancelDecision: null, failure: null, diagnosticsAvailable: false,
        ...overrides,
    }
}

describe('native generation facade', () => {
    beforeEach(() => {
        mocks.invoke.mockReset()
        mocks.isTauri = true
    })

    it('hands decimal-string epochs, sequences and checkpoints back exactly as the command returned them', async () => {
        const status = {
            ticket, characterId: 'c', conversationId: 'v', phase: 'running', messageId: 'm', checkpoint: '9007199254740993',
            committedRevision: 7, pendingCommit: false, preparationReady: true, preparationError: null,
            dispatch: 'issued', output: 'none', cancelDecision: null, failure: null, diagnosticsAvailable: false,
        } satisfies NativeGenerationStatus
        mocks.invoke.mockResolvedValue(status)

        const result = await nativeGenerationExecute(ticket)

        expect(result.ticket).toEqual(ticket)
        expect(result).toMatchObject({ characterId: 'c', conversationId: 'v' })
        expect(result.checkpoint).toBe('9007199254740993')
        expect(typeof result.ticket.sequence).toBe('string')
        expect(mocks.invoke).toHaveBeenCalledWith('native_generation_execute', { ticket })
    })

    it('sends each operation to its command with decimal tickets unchanged', async () => {
        mocks.invoke.mockResolvedValue(undefined)
        const request = {
            characterId: 'c', conversationId: 'v', expectedRevision: 3,
            libraryIdentity: 'lib', bindingAuthority: '18446744073709551614', effectivePresetId: 'p',
        }

        await nativeGenerationHostInit('agent')
        await nativeGenerationReserveOllama(request)
        await nativeGenerationCancel(ticket, 'user')
        await nativeGenerationActive()
        await nativeGenerationProof(ticket)

        expect(mocks.invoke.mock.calls).toEqual([
            ['native_generation_host_init', { userAgent: 'agent' }],
            ['native_generation_reserve_ollama', { request }],
            ['native_generation_cancel', { ticket, reason: 'user' }],
            ['native_generation_active'],
            ['native_generation_proof', { ticket, normalization: null }],
        ])
    })

    it('caps the status wait at 25 seconds and never sends a negative wait', async () => {
        mocks.invoke.mockResolvedValue(undefined)

        await nativeGenerationStatus(ticket)
        await nativeGenerationStatus(ticket, 60_000)
        await nativeGenerationStatus(ticket, -5)

        expect(mocks.invoke.mock.calls.map(([, args]) => args.waitMs)).toEqual([null, 25_000, 0])
    })

    it('never calls the native commands outside the native app', async () => {
        mocks.isTauri = false

        await expect(nativeGenerationReserveOllama({
            characterId: 'c', conversationId: 'v', expectedRevision: 1,
            libraryIdentity: 'lib', bindingAuthority: '1', effectivePresetId: 'p',
        })).rejects.toThrow('requires Tauri')
        await expect(nativeGenerationStatus(ticket, 10)).rejects.toThrow('requires Tauri')
        await expect(nativeGenerationHostInit('agent')).rejects.toThrow('requires Tauri')

        expect(mocks.invoke).not.toHaveBeenCalled()
    })

    describe('storage port adapter', () => {
        it('forwards active targets, proofs and cancellation with decimal tickets unchanged', async () => {
            const status = routeStatus()
            const proof = { ticket, revision: 7, at: 'issued', affectedUnitKeys: ['["messages","c","v"]'] }
            mocks.invoke.mockImplementation(async (command, args) => {
                if (command === 'native_generation_active') return [status]
                if (command === 'native_generation_proof') {
                    expect(args).toEqual({ ticket, normalization: null })
                    return proof
                }
                expect(command).toBe('native_generation_cancel')
                expect(args.ticket).toEqual(ticket)
                return { ...status, cancelDecision: args.reason }
            })
            const port = createNativeGenerationPort()

            expect(await port.active()).toEqual([status])
            expect(await port.proof(ticket)).toBe(proof)
            for (const reason of ['user', 'edit', 'plugin', 'authority', 'lifecycle'] as const) {
                expect(await port.cancel(ticket, reason)).toMatchObject({ ticket, cancelDecision: reason })
            }
        })

        it('returns a newly observed or settled ticket immediately before polling again', async () => {
            const running = routeStatus()
            const completed = routeStatus({ phase: 'completed', output: 'committed' })
            const waits: unknown[] = []
            mocks.invoke.mockImplementation(async (_command, args) => {
                waits.push(args.waitMs)
                return waits.length === 1 ? running : completed
            })
            const port = createNativeGenerationPort()

            expect(await port.status(ticket, 25_000)).toBe(running)
            expect(await port.status(ticket, 25_000)).toBe(completed)
            expect(waits).toEqual([null, null])
        })

        it('returns a dispatch change since the active listing without waiting for another change', async () => {
            const preparing = routeStatus({ preparationReady: false, dispatch: 'none' })
            const issued = routeStatus()
            mocks.invoke.mockResolvedValueOnce([preparing]).mockResolvedValueOnce(issued)
            const port = createNativeGenerationPort()
            await port.active()

            expect(await port.status(ticket, 25_000)).toBe(issued)
            expect(mocks.invoke.mock.calls.at(-1)).toEqual(['native_generation_status', { ticket, waitMs: null }])
        })

        it('long polls an unchanged running ticket after checking its current status', async () => {
            const running = routeStatus()
            const completed = routeStatus({ phase: 'completed', output: 'committed' })
            mocks.invoke.mockResolvedValueOnce([running]).mockResolvedValueOnce(running).mockResolvedValueOnce(completed)
            const port = createNativeGenerationPort()
            await port.active()

            expect(await port.status(ticket, 60_000)).toBe(completed)
            expect(mocks.invoke.mock.calls.slice(1)).toEqual([
                ['native_generation_status', { ticket, waitMs: null }],
                ['native_generation_status', { ticket, waitMs: 25_000, after: {
                    phase: 'running', dispatch: 'issued', checkpoint: '9007199254740993', committedRevision: 7,
                    pendingCommit: false, preparationReady: true, output: 'none', cancelDecision: null, diagnosticsAvailable: false,
                } }],
            ])
        })

        it('observes dispatch that changes between its immediate read and the long poll', async () => {
            const preparing = routeStatus({ preparationReady: false, dispatch: 'none' })
            const issued = routeStatus()
            mocks.invoke.mockImplementation(async (command, args) => {
                if (command === 'native_generation_active') return [preparing]
                if (args.waitMs === null) return preparing
                // Native has issued the request since the no-wait read. The observation
                // lets it return that change without waiting for the held provider reply.
                if (args.after?.dispatch === 'none' && args.after.preparationReady === false) return issued
                throw new Error('The status request missed the dispatch change')
            })
            const port = createNativeGenerationPort()
            await port.active()

            expect(await port.status(ticket, 25_000)).toBe(issued)
        })

        it.each([
            ['unknown-ticket', false], ['unknown-ticket', true],
            ['expired-ticket', false], ['expired-ticket', true],
        ] as const)('maps a gone ticket (%s) to the recovery error, long poll: %s', async (code, poll) => {
            const unknown = { code: 'validation', message: 'native-generation:' + code }
            const port = createNativeGenerationPort()
            if (poll) {
                const running = routeStatus()
                mocks.invoke.mockResolvedValueOnce([running]).mockResolvedValueOnce(running)
                await port.active()
            }
            mocks.invoke.mockRejectedValueOnce(unknown)

            await expect(port.status(ticket, 25_000)).rejects.toBeInstanceOf(NativeGenerationUnknownTicketError)
        })

        it('preserves other failures and leaves the raw facade rejection unchanged', async () => {
            const port = createNativeGenerationPort()
            for (const code of ['unknown-ticket', 'expired-ticket']) {
                const gone = { code: 'validation', message: 'native-generation:' + code }
                const other = { ...gone, code: 'store' }
                mocks.invoke.mockRejectedValueOnce(gone).mockRejectedValueOnce(other)

                await expect(nativeGenerationStatus(ticket)).rejects.toBe(gone)
                await expect(port.status(ticket)).rejects.toBe(other)
            }
            const transient = { code: 'validation', message: 'native-generation:store-unavailable' }
            mocks.invoke.mockRejectedValueOnce(transient)
            await expect(port.status(ticket)).rejects.toBe(transient)
        })
    })

    it.each(['http', 'sdk', 'network', 'type', 'decode'] as const)('shows the trimmed thrown-error message for a %s failure', (category) => {
        const failure = { field: null, stage: 'body', httpStatus: null, sdkMessage: '  model x not found\n' }

        expect(nativeGenerationFailureMessage({ ...failure, category })).toBe('model x not found')
        expect(nativeGenerationFailureMessage({ ...failure, category, sdkMessage: null })).toBeNull()
    })

    it.each(['unsupported', 'invalid', 'tokenizer', 'context-limit', 'limit', 'compat', 'consumer', 'unsupported-output', 'stale', 'interrupted'] as const)(
        'has no TS wording for a %s failure even when a message is present', (category) => {
            expect(nativeGenerationFailureMessage({ category, field: null, stage: null, httpStatus: null, sdkMessage: 'text' })).toBeNull()
        })
})
