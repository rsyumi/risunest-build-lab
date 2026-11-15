import { expect, it } from 'vitest'
import type { MultiModal, OpenAIChat } from '../../src/ts/process/index.svelte'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// Strings that are not well-formed travel as their UTF-16 units, since serde_json rejects isolated surrogate escapes.
type Text = string | { u: number[] }
// ["undefined"], ["null"], ["bool", b], ["number", repr], ["string", s], ["array", items], ["object", [key, value][]]
type Tagged = ['undefined'] | ['null'] | ['bool', boolean] | ['number', string] | ['string', Text] | ['array', Tagged[]] | ['object', [Text, Tagged][]]

// The case files come from ir.cases.mjs. Every set runs JS built-ins or restates a TS contract, so no product source
// file is exercised or hashed.
const settle = { generator: import.meta.url, tsSources: [] }

function decodeText(text: Text) {
    if (typeof text === 'string') return text
    let value = ''
    for (const unit of text.u) value += String.fromCharCode(unit)
    return value
}
// String.prototype.isWellFormed is ES2024, past the configured lib.
function wellFormed(text: string) {
    for (let index = 0; index < text.length; index++) {
        const unit = text.charCodeAt(index)
        if (unit >= 0xdc00 && unit <= 0xdfff) return false
        if (unit >= 0xd800 && unit <= 0xdbff) {
            const next = text.charCodeAt(index + 1)
            if (!(next >= 0xdc00 && next <= 0xdfff)) return false
            index++
        }
    }
    return true
}
function encodeText(text: string): Text {
    return wellFormed(text) ? text : { u: Array.from({ length: text.length }, (_, index) => text.charCodeAt(index)) }
}
const decodeNumber = (repr: string) => repr === '-0' ? -0 : Number(repr)
const encodeNumber = (value: number) => Object.is(value, -0) ? '-0' : String(value)

function defineAll(target: object, source: Record<string, unknown>) {
    // defineProperty keeps `__proto__` an own data property, as JSON.parse creates it.
    for (const key of Object.keys(source)) {
        Object.defineProperty(target, key, { value: source[key], enumerable: true, writable: true, configurable: true })
    }
}

function decode(value: Tagged): unknown {
    switch (value[0]) {
        case 'undefined': return undefined
        case 'null': return null
        case 'bool': return value[1]
        case 'number': return decodeNumber(value[1])
        case 'string': return decodeText(value[1])
        case 'array': return value[1].map(decode)
        case 'object': {
            const object: Record<string, unknown> = {}
            for (const [key, item] of value[1]) {
                Object.defineProperty(object, decodeText(key), { value: decode(item), enumerable: true, writable: true, configurable: true })
            }
            return object
        }
    }
}

function encode(value: unknown): Tagged {
    if (value === undefined) return ['undefined']
    if (value === null) return ['null']
    if (typeof value === 'boolean') return ['bool', value]
    if (typeof value === 'number') return ['number', encodeNumber(value)]
    if (typeof value === 'string') return ['string', encodeText(value)]
    if (Array.isArray(value)) return ['array', value.map(encode)]
    return ['object', Object.entries(value as object).map(([key, item]): [Text, Tagged] => [encodeText(key), encode(item)])]
}

function numberFromBits(bits: string) {
    const view = new DataView(new ArrayBuffer(8))
    view.setBigUint64(0, BigInt(`0x${bits}`))
    return view.getFloat64(0)
}

// A contract, not a run of the TS builders: the object literal inside sendChat (`const chat:OpenAIChat = {...}` in
// src/ts/process/index.svelte.ts) and exampleMessage cannot run here without stubbing the parser and the stores, so
// this restates their order. `name` follows `content` as in exampleMessage, empty multimodals are deleted as sendChat
// deletes them, and `removable` and `cachePoint` come last as sendChat assigns them later.
function canonicalMessage(input: Record<string, unknown>) {
    const { role, content, name, memo, attr, multimodals, thoughts, removable, cachePoint, ...extra } = input as unknown as OpenAIChat & Record<string, unknown>
    const chat: OpenAIChat = {
        role, content, name, memo, attr,
        multimodals: (multimodals as unknown as Record<string, unknown>[] | undefined)?.map(canonicalMultimodal),
        thoughts,
    }
    if (chat.multimodals?.length === 0) delete chat.multimodals
    if (removable !== undefined) chat.removable = removable
    if (cachePoint !== undefined) chat.cachePoint = cachePoint
    defineAll(chat, extra)
    return chat
}

// The contract order of the `MultiModal` builders in src/ts/process/index.svelte.ts and src/ts/process/scriptings.ts.
function canonicalMultimodal(input: Record<string, unknown>) {
    const { type, base64, width, height, ...extra } = input as unknown as MultiModal & Record<string, unknown>
    const multimodal: MultiModal = { type, base64, width, height }
    defineAll(multimodal, extra)
    return multimodal
}

function parseResult(source: string) {
    try {
        const parsed: unknown = JSON.parse(source)
        return { value: encode(parsed), text: JSON.stringify(parsed) }
    } catch (error) {
        return { error: (error as Error).name }
    }
}

function run<C extends OracleCase>(set: string, result: (entry: C) => unknown) {
    const cases = readOracleCases<C>('ir', set)
    expect(settleOracle(cases, cases.cases.map(result), settle)).toMatch(/^(written|matched)$/)
}

it('JSON.parse keeps order, duplicates, -0 and isolated surrogates', () => {
    run<OracleCase & { source: string }>('json-parse', ({ source }) => parseResult(source))
})

it('JSON.parse over text holding raw isolated surrogates', () => {
    run<OracleCase & { source: Text }>('json-parse-units', ({ source }) => parseResult(decodeText(source)))
})

it('JSON.stringify omits undefined, writes -0 as 0 and escapes isolated surrogates', () => {
    run<OracleCase & { value: Tagged }>('json-stringify', ({ value }) => {
        const text = JSON.stringify(decode(value))
        return text === undefined ? { undefined: true } : { text }
    })
})

it('Number.prototype.toString', () => {
    run<OracleCase & { bits: string }>('number-to-string', ({ bits }) => ({ text: numberFromBits(bits).toString() }))
})

it('Number(string)', () => {
    run<OracleCase & { text: Text }>('number-from-text', ({ text }) => ({ number: encodeNumber(Number(decodeText(text))) }))
})

it('String.prototype.trim, trimStart and trimEnd', () => {
    run<OracleCase & { text: Text }>('trim', ({ text }) => {
        const value = decodeText(text)
        return { trim: encodeText(value.trim()), trimStart: encodeText(value.trimStart()), trimEnd: encodeText(value.trimEnd()) }
    })
})

it('OpenAIChat and MultiModal objects in the contract construction order', () => {
    run<OracleCase & { kind: 'message' | 'multimodal'; value: Tagged }>('prompt-message', ({ kind, value }) => {
        const input = decode(value) as Record<string, unknown>
        return { text: JSON.stringify(kind === 'message' ? canonicalMessage(input) : canonicalMultimodal(input)) }
    })
})
