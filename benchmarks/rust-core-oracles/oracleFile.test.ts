import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, expect, it, vi } from 'vitest'
import { fixtureRoot, readOracleCases, settleOracle, writeVariable } from './oracleFile'

const roots: string[] = []
afterEach(() => {
    vi.unstubAllEnvs()
    for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

const casesText = '{"schema":1,"cases":[{"id":"a","input":"x"},{"id":"b","input":"y"}]}\n'
const sources = { generator: 'benchmarks/rust-core-oracles/lane.oracle.test.ts', tsSources: ['entry.ts'] }
const write = { ...sources, write: true }
const compare = { ...sources, write: false }

function repository(cases: string) {
    const root = mkdtempSync(join(tmpdir(), 'risunest-rust-core-oracle-'))
    roots.push(root)
    mkdirSync(join(root, fixtureRoot, 'lane'), { recursive: true })
    writeFileSync(join(root, fixtureRoot, 'lane', 'set.cases.json'), cases)
    writeFileSync(join(root, 'entry.ts'), 'export const value = 1\n')
    return root
}

it('fails on TS output drift and fixture edits but not on informational fields', () => {
    const root = repository(casesText)
    const set = readOracleCases('lane', 'set', root)
    vi.stubEnv(writeVariable, '1')
    expect(settleOracle(set, [{ text: 'x' }, { text: 'y' }], sources)).toBe('written')
    const path = join(root, fixtureRoot, 'lane', 'set.expected.json')
    const written = JSON.parse(readFileSync(path, 'utf8'))
    expect(written).toMatchObject({
        schema: 1, generator: sources.generator, fixtureSha256: set.fixtureSha256, unicode: process.versions.unicode,
        cases: [{ id: 'a', result: { text: 'x' } }, { id: 'b', result: { text: 'y' } }],
    })
    writeFileSync(path, JSON.stringify({ ...written, tsSourceSha256: {}, node: 'v0.0.0', unicode: '0.0', vitest: '0.0.0' }))
    expect(settleOracle(set, [{ text: 'x' }, { text: 'y' }], compare)).toBe('matched')
    expect(() => settleOracle(set, [{ text: 'x' }, { text: 'z' }], compare)).toThrow(/case b drifted/)
    writeFileSync(join(root, fixtureRoot, 'lane', 'set.cases.json'), casesText.replace('"y"', '"w"'))
    expect(() => settleOracle(readOracleCases('lane', 'set', root), [{ text: 'x' }, { text: 'y' }], compare)).toThrow(/fixtureSha256/)
})

it('rejects what a Rust replay could not reproduce', () => {
    expect(() => readOracleCases('lane', 'set', repository(casesText.replace('\n', '\r\n')))).toThrow(/LF line endings/)
    const set = readOracleCases('lane', 'set', repository(casesText))
    expect(() => settleOracle(set, [{ text: 'x' }, { text: undefined }], write)).toThrow(/case b: the result does not survive JSON/)
    expect(() => settleOracle(set, [-0, 1], write)).toThrow(/case a: the result does not survive JSON/)
    expect(() => settleOracle(set, [1, 2], compare)).toThrow(/is missing; run with RUST_CORE_ORACLE_WRITE=1/)
})

it('reads a nested lane and rejects a lane that leaves the fixture root', () => {
    const root = repository(casesText)
    mkdirSync(join(root, fixtureRoot, 'lane', 'nested'), { recursive: true })
    writeFileSync(join(root, fixtureRoot, 'lane', 'nested', 'set.cases.json'), casesText)
    expect(readOracleCases('lane/nested', 'set', root).cases).toHaveLength(2)
    for (const lane of ['lane//nested', '/lane', 'lane/', 'lane/..', '../lane', 'lane/./nested', 'lane\nested', 'Lane']) {
        expect(() => readOracleCases(lane, 'set', root)).toThrow(/Invalid oracle set name/)
    }
})
