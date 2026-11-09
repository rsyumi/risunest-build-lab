import { expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// This oracle has no mounted UI. Application startup effects remain suspended.
vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: (_start: () => void) => () => {} }
})

import { createPureCbsCallbacks, makeArray, parseArray } from '../../src/ts/cbsPure'
import { caseUnicode, tailoredCase } from '../../src/ts/process/nativeHostCapture'
import { getModelInfo } from '../../src/ts/model/modellist'
import { normalizeDatabaseDefaults, setDatabaseLite, type Database } from '../../src/ts/storage/database.svelte'

// The case files come from cbs-host.cases.mjs.
it('records the case mapping of every code point through the CBS case functions', () => {
    const set = readOracleCases<OracleCase>('cbs', 'case-table')
    expect(set.cases.map(entry => entry.id)).toEqual(['all-code-points'])
    const callbacks = createPureCbsCallbacks({ parseArray, makeArray, calcString: () => 0 })
    const functions = { lower: callbacks.lower, upper: callbacks.upper, capitalize: callbacks.capitalize }
    const tables: Record<keyof typeof functions, [number, string][]> = { lower: [], upper: [], capitalize: [] }
    // Runs of code points with one Final_Sigma context class: bit 1 when a capital sigma before the code point stays
    // medial, bit 0 when one after it is final.
    const classes: [number, number, number][] = []
    for (let code = 0; code <= 0x10ffff; code++) {
        // A surrogate code point is its lone unit.
        const text = code >= 0xd800 && code <= 0xdfff ? String.fromCharCode(code) : String.fromCodePoint(code)
        for (const name of Object.keys(functions) as (keyof typeof functions)[]) {
            const mapped = functions[name]([text])
            if (mapped !== text) tables[name].push([code, mapped])
        }
        const sigma = Number(functions.lower(['ΑΣ' + text])[1] === 'σ') * 2
            + Number(functions.lower(['Α' + text + 'Σ']).endsWith('ς'))
        const last = classes.at(-1)
        if (last && last[2] === sigma) last[1] = code
        else classes.push([code, code, sigma])
    }
    const result = { caseUnicode: caseUnicode(), tailoredCase: tailoredCase(), ...tables, classes }
    expect(settleOracle(set, [result], { generator: import.meta.url,
        tsSources: ['src/ts/cbsPure.ts', 'src/ts/process/nativeHostCapture.ts', 'src/ts/process/caseUnicode.ts'] })).toMatch(/written|matched/)
})

it('records the model record the CBS model readers see for Ollama', () => {
    const set = readOracleCases<OracleCase>('cbs', 'model-info')
    const results = set.cases.map(entry => {
        setDatabaseLite(normalizeDatabaseDefaults({
            aiModel: entry.id, characters: [], botPresets: [], plugins: [], modules: [], enabledModules: [],
        } as unknown as Database))
        return { json: JSON.stringify(getModelInfo(entry.id)) }
    })
    expect(settleOracle(set, results, { generator: import.meta.url, tsSources: ['src/ts/model/modellist.ts'] })).toMatch(/written|matched/)
})
