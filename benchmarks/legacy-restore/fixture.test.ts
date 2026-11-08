import { describe, expect, it, vi } from 'vitest'
// The isolated harness runs in a WebView, without Node's internal Buffer API.
vi.hoisted(() => { vi.stubGlobal('Buffer', undefined) })
import { unpack } from 'msgpackr/index-no-eval'
import { fixturePackets, fixtureCharacter, messageProjection, planFixture } from './fixture'

describe('bounded synthetic legacy measurement fixture', () => {
    it.each([
        [true, 'synthetic'], [false, 'synthetic'],
        [true, 'd085c8c9-1b60-4933-b957-b335b6077158'], [false, 'd085c8c9-1b60-4933-b957-b335b6077158'],
    ] as const)('emits an exact decoded size with charactersFirst=%s and prefix=%s', (charactersFirst, prefix) => {
        const plan = planFixture(2_500_000, prefix)
        const packets = [...fixturePackets(plan, charactersFirst)]
        expect(Math.max(...packets.map(bytes => bytes.length))).toBeLessThan(3_000_000)
        expect(packets.reduce((sum, bytes) => sum + bytes.length, 0)).toBe(plan.decodedBytes)
        const bytes = new Uint8Array(plan.decodedBytes)
        let offset = 0
        for (const packet of packets) { bytes.set(packet, offset); offset += packet.length }
        const decoded = unpack(bytes)
        expect(decoded.characters).toHaveLength(plan.characterCount)
        expect(decoded.characters.reduce((sum: number, character: any) => sum + character.chats[0].message.length, 0)).toBe(plan.messageCount)
        for (let index = 0; index < plan.characterCount; index++) {
            expect(decoded.characters[index].chaId).toBe(`${prefix}-${String(index).padStart(6, '0')}`)
            expect(messageProjection(decoded.characters[index].chats[0].message))
                .toBe(messageProjection(fixtureCharacter(plan, index).chats[0].message))
        }
        expect(decoded.botPresets).toEqual([])
        expect(decoded.pluginCustomStorage).toEqual({})
    })
    it.each([100, 300, 600])('plans exactly %s decimal MB without building the database tree', megabytes => {
        const original = planFixture(megabytes * 1_000_000)
        for (const prefix of ['synthetic', 'd085c8c9-1b60-4933-b957-b335b6077158']) {
            const plan = planFixture(megabytes * 1_000_000, prefix)
            expect(plan.decodedBytes).toBe(megabytes * 1_000_000)
            expect(plan.characterCount).toBe(original.characterCount)
            expect(plan.messageCount).toBe(original.messageCount)
            const fixedBytes = [...fixturePackets({ ...plan, characterCount: 0 })]
                .reduce((sum, bytes) => sum + bytes.length, 0)
            const firstBytes = [...fixturePackets({ ...plan, characterCount: 1 })]
                .reduce((sum, bytes) => sum + bytes.length, 0) - fixedBytes
            const tailBytes = [...fixturePackets({ ...plan, fullCharacterCount: 0, characterCount: 1 })]
                .reduce((sum, bytes) => sum + bytes.length, 0) - fixedBytes
            expect(fixedBytes + firstBytes * plan.fullCharacterCount + tailBytes).toBe(plan.decodedBytes)
        }
    })
    it('keeps message identities and references while assigning a fresh parent identity', () => {
        const first = fixtureCharacter(planFixture(2_500_000, 'first-run'), 0)
        const second = fixtureCharacter(planFixture(2_500_000, 'second-run'), 0)
        expect(second.chaId).not.toBe(first.chaId)
        expect(second.chats).toEqual(first.chats)
    })
})
