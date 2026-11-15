import { createHash } from 'node:crypto'
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, isAbsolute, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { isDeepStrictEqual } from 'node:util'

// Rust replay tests embed both files and check fixtureSha256, so the bytes must not depend on the platform.
export const fixtureRoot = 'tests/fixtures/rust-core'
export const writeVariable = 'RUST_CORE_ORACLE_WRITE'

// Built from a path string: the happy-dom environment replaces the global URL, which fileURLToPath rejects.
const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const setName = /^[a-z0-9][a-z0-9-]*$/

export type OracleCase = { id: string }
export type ExpectedCase = { id: string; result: unknown }
export type ExpectedFile = {
    schema: number
    generator: string
    fixtureSha256: string
    // Informational: neither these hashes nor the versions take part in the drift check.
    tsSourceSha256: Record<string, string>
    node: string
    // The Unicode version of the runtime's case and class tables, which a Rust replay can require.
    unicode?: string
    vitest: string
    cases: ExpectedCase[]
}
export type OracleSet<C extends OracleCase> = {
    root: string
    casesPath: string
    expectedPath: string
    fixtureSha256: string
    schema: number
    cases: C[]
}
export type SettleOptions = {
    // import.meta.url of the oracle test, or its repository-relative path.
    generator: string
    // Repository-relative TS entry files the oracle calls.
    tsSources: string[]
    // Defaults to RUST_CORE_ORACLE_WRITE=1.
    write?: boolean
}

const sha256 = (data: string | Uint8Array) => createHash('sha256').update(data).digest('hex')

function repositoryPath(root: string, path: string) {
    const absolute = path.startsWith('file:') ? fileURLToPath(path) : isAbsolute(path) ? path : join(root, path)
    const result = relative(root, absolute).replaceAll('\\', '/')
    if (result === '' || result.startsWith('../') || isAbsolute(result)) throw new Error(`Oracle path outside the repository: ${path}`)
    return result
}

function preview(value: unknown) {
    const text = JSON.stringify(value) ?? 'undefined'
    return text.length > 400 ? `${text.slice(0, 400)}...` : text
}

export function readOracleCases<C extends OracleCase>(lane: string, set: string, root = repositoryRoot): OracleSet<C> {
    // A lane is segments joined by '/', each following the set-name pattern, so no empty, '.', '..' or backslash segment passes.
    const segments = lane.split('/')
    if (!segments.every(segment => setName.test(segment)) || !setName.test(set)) throw new Error(`Invalid oracle set name: ${lane}/${set}`)
    const base = join(root, fixtureRoot, ...segments, set)
    const casesPath = `${base}.cases.json`
    const display = repositoryPath(root, casesPath)
    const bytes = readFileSync(casesPath)
    if (bytes.includes(0x0d)) throw new Error(`${display} must use LF line endings`)
    const file = JSON.parse(new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes)) as { schema?: unknown; cases?: unknown }
    if (typeof file !== 'object' || file === null || !Number.isSafeInteger(file.schema) || (file.schema as number) < 1) {
        throw new Error(`${display} needs a positive integer schema`)
    }
    if (!Array.isArray(file.cases) || file.cases.length === 0) throw new Error(`${display} needs a nonempty cases array`)
    const ids = new Set<string>()
    for (const entry of file.cases as { id?: unknown }[]) {
        if (typeof entry?.id !== 'string' || entry.id === '') throw new Error(`${display} has a case without a string id`)
        if (ids.has(entry.id)) throw new Error(`${display} repeats case ${entry.id}`)
        ids.add(entry.id)
    }
    return { root, casesPath, expectedPath: `${base}.expected.json`, fixtureSha256: sha256(bytes), schema: file.schema as number, cases: file.cases as C[] }
}

// Writes the expected file in write mode; otherwise fails on any difference from the committed one.
export function settleOracle<C extends OracleCase>(set: OracleSet<C>, results: unknown[], options: SettleOptions): 'written' | 'matched' {
    const display = repositoryPath(set.root, set.expectedPath)
    if (results.length !== set.cases.length) throw new Error(`${display}: ${results.length} results for ${set.cases.length} cases`)
    const cases = set.cases.map((entry, index): ExpectedCase => {
        const text = JSON.stringify(results[index])
        const result: unknown = text === undefined ? undefined : JSON.parse(text)
        if (text === undefined || !isDeepStrictEqual(result, results[index])) {
            throw new Error(`${display} case ${entry.id}: the result does not survive JSON; encode undefined, -0, NaN and non-plain values explicitly`)
        }
        return { id: entry.id, result }
    })
    const header = { schema: set.schema, generator: repositoryPath(set.root, options.generator), fixtureSha256: set.fixtureSha256 }
    if (options.write ?? process.env[writeVariable] === '1') {
        const tsSourceSha256 = Object.fromEntries(options.tsSources.map(source => [
            repositoryPath(set.root, source),
            sha256(readFileSync(join(set.root, source), 'utf8').replaceAll('\r\n', '\n')),
        ]))
        const vitest = (createRequire(import.meta.url)('vitest/package.json') as { version: string }).version
        const file: ExpectedFile = { ...header, tsSourceSha256, node: process.version, unicode: process.versions.unicode, vitest, cases }
        writeFileSync(set.expectedPath, `${JSON.stringify(file, null, 2)}\n`)
        return 'written'
    }
    const rerun = `review the change and rerun with ${writeVariable}=1`
    if (!existsSync(set.expectedPath)) throw new Error(`${display} is missing; run with ${writeVariable}=1 to generate it`)
    const expected = JSON.parse(readFileSync(set.expectedPath, 'utf8')) as Partial<ExpectedFile>
    for (const key of ['schema', 'generator', 'fixtureSha256'] as const) {
        if (expected[key] !== header[key]) throw new Error(`${display} ${key} is ${preview(expected[key])}, regenerated ${preview(header[key])}; ${rerun}`)
    }
    if (!Array.isArray(expected.cases) || expected.cases.length !== cases.length) {
        throw new Error(`${display} holds ${Array.isArray(expected.cases) ? expected.cases.length : 'no'} cases, regenerated ${cases.length}; ${rerun}`)
    }
    cases.forEach((entry, index) => {
        if (!isDeepStrictEqual(expected.cases![index], entry)) {
            throw new Error(`${display} case ${entry.id} drifted: committed ${preview(expected.cases![index])}, regenerated ${preview(entry)}; ${rerun}`)
        }
    })
    return 'matched'
}
