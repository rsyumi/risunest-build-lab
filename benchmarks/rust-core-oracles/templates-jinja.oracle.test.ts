import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import { Template } from '@huggingface/jinja'
import { afterAll, beforeAll, expect, it, vi } from 'vitest'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// Strings that are not well-formed travel as their UTF-16 units, since serde_json rejects isolated surrogate escapes.
type Text = string | { u: number[] }
type Now = { year: number; month: number; day: number; hour: number; minute: number }
type JinjaCase = OracleCase & { template: Text; context?: string; now?: Now; decline?: string }
// `parse` is a throw from `new Template`, `render` one from `Template.render`.
type Result = { ok: Text } | { error: 'parse' | 'render'; message: Text }

// The cases come from templates-jinja.cases.mjs and run only the `@huggingface/jinja` package the app imports, so no
// product source file is exercised or hashed. The package version is pinned below instead.
const settle = { generator: import.meta.url, tsSources: [] }
const set = readOracleCases<JinjaCase>('templates', 'jinja')

function decodeText(text: Text) {
    if (typeof text === 'string') return text
    let value = ''
    for (const unit of text.u) value += String.fromCharCode(unit)
    return value
}
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
const encodeText = (text: string): Text => wellFormed(text) ? text : { u: Array.from({ length: text.length }, (_, index) => text.charCodeAt(index)) }
const message = (error: unknown) => encodeText(error instanceof Error ? error.message : String(error))

const zone = process.env.TZ
beforeAll(() => {
    // strftime_now reads local time; a fixed zone and clock make it the case's `now`.
    process.env.TZ = 'UTC'
    vi.useFakeTimers({ toFake: ['Date'] })
})
afterAll(() => {
    vi.useRealTimers()
    if (zone === undefined) delete process.env.TZ
    else process.env.TZ = zone
})

function run(entry: JinjaCase): Result {
    if (entry.now) {
        const { year, month, day, hour, minute } = entry.now
        vi.setSystemTime(new Date(Date.UTC(year, month - 1, day, hour, minute, 30)))
        const local = new Date()
        expect([local.getFullYear(), local.getMonth() + 1, local.getDate(), local.getHours(), local.getMinutes()]).toEqual([year, month, day, hour, minute])
    }
    let template: Template
    try {
        template = new Template(decodeText(entry.template))
    } catch (error) {
        return { error: 'parse', message: message(error) }
    }
    try {
        const context = entry.context === undefined ? undefined : JSON.parse(entry.context) as Record<string, unknown>
        return { ok: encodeText(template.render(context)) }
    } catch (error) {
        return { error: 'render', message: message(error) }
    }
}

it('pins the @huggingface/jinja version the app ships', () => {
    // The package exports only its entry, which sits in dist/.
    const path = join(dirname(createRequire(import.meta.url).resolve('@huggingface/jinja')), '..', 'package.json')
    expect((JSON.parse(readFileSync(path, 'utf8')) as { version: string }).version).toBe('0.5.10')
})

it('renders the jinja engine cases', () => {
    const results = set.cases.map(run)
    expect(results.some(result => 'ok' in result)).toBe(true)
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})
