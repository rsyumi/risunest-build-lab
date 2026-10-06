import { describe, expect, it } from 'vitest'
import type { Chat } from '../../src/ts/storage/database.svelte'
import { captureSyntheticReroll } from './rerollCapture'

function fixture() {
    const chat: Chat = {
        id: 'synthetic-chat', name: '', note: '', localLore: [],
        message: [
            { role: 'user', data: 'synthetic question private marker', chatId: 'm-0' },
            { role: 'char', data: 'synthetic original private marker', chatId: 'm-1' },
        ],
    }
    let serial = 0
    return {
        chat, session: () => null, isCurrent: () => true,
        createId: () => `attempt-${++serial}`, aborted: () => false,
        flush: async () => {},
        generate: async () => {
            chat.message.push({ role: 'char', data: 'synthetic reply private marker', chatId: 'm-1' })
            return true
        },
    }
}

describe('synthetic reroll capture', () => {
    it('records the real reroll checkpoints and provider completion without content', async () => {
        const options = fixture()
        const capture = captureSyntheticReroll(options, 'synthetic-owner')
        const generate = options.generate
        options.generate = async () => { capture.providerCompleted(); return generate() }
        expect(await capture.run()).toBe(true)
        expect(capture.snapshots()).toEqual(expect.arrayContaining([
            expect.objectContaining({ phase: 'checkpoint', pendingSave: true, recoveryPhase: 'prepared' }),
            expect.objectContaining({ phase: 'generation', messageCount: 1, recoveryPhase: 'generating' }),
            expect.objectContaining({ phase: 'settled', providerCompleted: true, result: true, messageCount: 2, recoveryPhase: null }),
        ]))
        expect(JSON.stringify(capture.snapshots())).not.toContain('private marker')
    })

    it('captures an unsettled save and stale owner without settling or retrying it', async () => {
        const options = fixture()
        let release!: () => void
        let valid = true
        options.isCurrent = () => valid
        options.flush = () => new Promise<void>(resolve => { release = resolve })
        const capture = captureSyntheticReroll(options, 'synthetic-owner')
        const run = capture.run()
        valid = false
        expect(capture.observe()).toMatchObject({ pendingSave: true, controllerValid: false, providerCompleted: false })
        release()
        expect(await run).toBe(false)
        expect(capture.observe()).toMatchObject({ generationCompleted: false, providerCompleted: false })
    })

    it('bounds retained snapshots and omits arbitrary error messages and renderer state after exit', async () => {
        const options = fixture()
        const failure = new TypeError('synthetic secret error text')
        options.flush = async () => { throw failure }
        const emitted: unknown[] = []
        const capture = captureSyntheticReroll(options, 'invalid owner private marker', snapshot => emitted.push(snapshot))
        await expect(capture.run()).rejects.toBe(failure)
        for (let index = 0; index < 100; index++) capture.observe()
        expect(capture.snapshots()).toHaveLength(64)
        expect(capture.rendererExited(true)).toMatchObject({ rendererExit: 'crashed', controllerValid: null, messageCount: null, errorCategory: 'type' })
        expect(capture.observe()).toMatchObject({ controllerValid: null, messageCount: null })
        expect(JSON.stringify(emitted)).not.toMatch(/private marker|secret error text/)
    })

    it('separates a pending provider from a completed provider awaiting its final save', async () => {
        const options = fixture()
        let releaseProvider!: () => void
        const provider = new Promise<void>(resolve => { releaseProvider = resolve })
        let releaseSave!: () => void
        let saveStarted!: () => void
        const saving = new Promise<void>(resolve => { saveStarted = resolve })
        let checkpoint = 0
        options.flush = async () => {
            if (++checkpoint === 2) {
                saveStarted()
                await new Promise<void>(resolve => { releaseSave = resolve })
            }
        }
        const generate = options.generate
        let providerStarted!: () => void
        const started = new Promise<void>(resolve => { providerStarted = resolve })
        const capture = captureSyntheticReroll(options, 'synthetic-owner')
        options.generate = async () => {
            providerStarted()
            await provider
            capture.providerCompleted()
            return generate()
        }
        const run = capture.run()
        await started
        expect(capture.observe()).toMatchObject({ providerCompleted: false, pendingSave: false, recoveryPhase: 'generating' })
        releaseProvider()
        await saving
        expect(capture.observe()).toMatchObject({ providerCompleted: true, pendingSave: true, recoveryPhase: null, result: null })
        releaseSave()
        expect(await run).toBe(true)
    })
})
