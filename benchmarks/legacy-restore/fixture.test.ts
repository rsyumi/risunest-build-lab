import { describe, expect, it, vi } from 'vitest'
// The isolated harness runs in a WebView, without Node's internal Buffer API.
vi.hoisted(() => { vi.stubGlobal('Buffer', undefined) })
import { unpack } from 'msgpackr/index-no-eval'
import { fixturePackets, fixtureCharacter, messageProjection, planFixture } from './fixture'

describe('bounded synthetic legacy measurement fixture', () => {
    it.each([true, false])('emits an exact decoded size with charactersFirst=%s', (charactersFirst) => {
        const plan = planFixture(2_500_000)
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
            expect(messageProjection(decoded.characters[index].chats[0].message))
                .toBe(messageProjection(fixtureCharacter(plan, index).chats[0].message))
        }
        expect(decoded.botPresets).toEqual([])
        expect(decoded.pluginCustomStorage).toEqual({})
    })
    it.each([100, 300, 600])('plans exactly %s decimal MB without building the database tree', megabytes => {
        expect(planFixture(megabytes * 1_000_000).decodedBytes).toBe(megabytes * 1_000_000)
    })
})
