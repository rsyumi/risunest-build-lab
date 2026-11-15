// Writes the JSON constants the formats crate embeds (crates/core/formats/src/*_constants.json) from the TS values the
// formats oracles record, so the crate never carries a hand copy. Run from the repository root:
//
//     RUST_CORE_ORACLE_WRITE=1 pnpm vitest run --project harness benchmarks/rust-core-oracles/formats-presets.oracle.test.ts benchmarks/rust-core-oracles/formats-risusave.oracle.test.ts benchmarks/rust-core-oracles/formats-prepare.oracle.test.ts
//     node benchmarks/rust-core-oracles/formats-constants.mjs
//
// The first command records each set's `constants` case from the TS modules (review the expected-file diff); the second
// writes that value, as `JSON.stringify(value, null, 2)` with LF and one trailing newline. `--check` only compares.
import { readFileSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../../', import.meta.url))
// [oracle set, case id, crate file]
const outputs = [
    ['presets', 'constants', 'crates/core/formats/src/preset_constants.json'],
    ['risusave', 'constants', 'crates/core/formats/src/risusave_constants.json'],
    ['prepare', 'defaults-constants', 'crates/core/formats/src/defaults_constants.json'],
]
const check = process.argv.includes('--check')

let stale = 0
for (const [set, id, output] of outputs) {
    const expectedPath = `tests/fixtures/rust-core/formats/${set}.expected.json`
    const expected = JSON.parse(readFileSync(`${root}${expectedPath}`, 'utf8'))
    const entry = expected.cases?.find(entry => entry.id === id)
    if (typeof entry?.result?.ok !== 'string') throw new Error(`${expectedPath} has no recorded ${id} case`)
    const text = `${JSON.stringify(JSON.parse(entry.result.ok), null, 2)}\n`
    let current = null
    try { current = readFileSync(`${root}${output}`, 'utf8') } catch {}
    if (current === text) { console.log(`${output} is current`); continue }
    if (check) { console.error(`${output} differs from ${expectedPath}`); stale += 1; continue }
    writeFileSync(`${root}${output}`, text)
    console.log(`wrote ${output}`)
}
process.exit(stale === 0 ? 0 : 1)
