import { Packr } from 'msgpackr/index-no-eval'

const pack = new Packr({ useRecords: false })
export const MEASUREMENT_MB = [100, 300, 600] as const
export type LegacyEncoding = 'raw' | 'gzip'
export type LegacyMessage = { role: string; data: string; chatId: string }
const fullMessages = 128
const messageBytes = 8192
const rootFields = { username: 'Synthetic memory fixture', botPresets: [], botPresetsId: 0,
    pluginCustomStorage: {}, modules: [], loadouts: [], plugins: [] }
const encodedRoot = Object.entries(rootFields).flatMap(([key, value]) => [pack.encode(key).slice(), pack.encode(value).slice()])
const charactersKey = pack.encode('characters').slice()
const rootBytes = 1 + charactersKey.length + 5 + encodedRoot.reduce((sum, bytes) => sum + bytes.length, 0)

function text(bytes: number, seed: number): string {
    const words = ['synthetic', 'conversation', 'message', 'fixture', 'chapter', 'window', 'river', 'forest', 'response', 'question', 'morning', 'evening']
    const parts: string[] = []
    let length = 0
    while (length < bytes) {
        seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
        const word = words[seed % words.length] + ' '
        parts.push(word)
        length += word.length
    }
    return parts.join('').slice(0, bytes)
}
function character(index: number, tailBytes?: number, characterPrefix = 'synthetic') {
    const id = String(index).padStart(6, '0')
    const message = Array.from({ length: tailBytes === undefined ? fullMessages : 1 }, (_, slot) => ({
        role: slot % 2 === 0 ? 'user' : 'char',
        chatId: `synthetic-message-${id}-${String(slot).padStart(3, '0')}`,
        data: text(tailBytes ?? messageBytes, (index + 1) * 131 + slot),
    }))
    return { type: 'character', chaId: `${characterPrefix}-${id}`, name: `Synthetic ${id}`, chatPage: 0,
        chats: [{ id: `chat-${id}`, name: 'Synthetic chat', note: '', localLore: [], message }] }
}
export interface FixturePlan {
    characterPrefix: string
    decodedBytes: number
    characterCount: number
    messageCount: number
    fullCharacterCount: number
    tailTextBytes: number
}
export function planFixture(decodedBytes: number, characterPrefix = 'synthetic'): FixturePlan {
    if (!Number.isSafeInteger(decodedBytes) || decodedBytes < 2_500_000 || decodedBytes > 600_000_000)
        throw new Error('Synthetic decoded size must be between 2.5 and 600 decimal MB')
    const packetBytes = pack.encode(character(0, undefined, characterPrefix)).length
    const fullCharacterCount = Math.floor((decodedBytes - rootBytes) / packetBytes) - 1
    const remaining = decodedBytes - rootBytes - fullCharacterCount * packetBytes
    // The last character has one bounded str32 message instead of a partial MessagePack value.
    const tailTextBytes = remaining - pack.encode(character(fullCharacterCount, 0, characterPrefix)).length - 4
    if (tailTextBytes < 65_536 || tailTextBytes > 3_000_000) throw new Error('Unexpected tail bounds')
    if (pack.encode(character(fullCharacterCount, tailTextBytes, characterPrefix)).length !== remaining)
        throw new Error('Exact decoded fixture size mismatch')
    return { characterPrefix, decodedBytes, characterCount: fullCharacterCount + 1,
        messageCount: fullCharacterCount * fullMessages + 1, fullCharacterCount, tailTextBytes }
}
export function fixtureCharacter(plan: FixturePlan, index: number) {
    if (index < 0 || index >= plan.characterCount) throw new Error('Invalid fixture character index')
    return character(index, index === plan.fullCharacterCount ? plan.tailTextBytes : undefined, plan.characterPrefix)
}
export function* fixturePackets(plan: FixturePlan, charactersFirst = true): Generator<Uint8Array> {
    yield Uint8Array.of(0x80 + Object.keys(rootFields).length + 1)
    if (!charactersFirst) yield* encodedRoot
    yield charactersKey
    const array = new Uint8Array(5)
    array[0] = 0xdd
    new DataView(array.buffer).setUint32(1, plan.characterCount)
    yield array
    for (let index = 0; index < plan.characterCount; index++) yield pack.encode(fixtureCharacter(plan, index)).slice()
    if (charactersFirst) yield* encodedRoot
}
export function messageProjection(messages: LegacyMessage[]): string {
    return JSON.stringify(messages.map(({ role, chatId, data }) => [role, chatId, data]))
}
