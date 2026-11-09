// Writes the case files the CBS host oracle (`cbs-host.oracle.test.ts`) reads under tests/fixtures/rust-core/cbs:
// `case-table`, the case mapping of every code point in the engine that runs the oracle, and `model-info`, the model
// record the CBS model readers see for the model the native route admits. Run
// `node benchmarks/rust-core-oracles/cbs-host.cases.mjs`, then the oracle with RUST_CORE_ORACLE_WRITE=1.
// `case-table-webview` is the same table recorded in a WebView engine by `cbs-case-table-webview.capture.mjs`.
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const write = (set, cases) => {
    const text = `{"schema":1,"cases":[\n${cases.map((entry) => JSON.stringify(entry)).join(',\n')}\n]}\n`
    writeFileSync(fileURLToPath(new URL(`../../tests/fixtures/rust-core/cbs/${set}.cases.json`, import.meta.url)), text)
}
write('case-table', [{ id: 'all-code-points' }])
write('case-table-webview', [{ id: 'all-code-points' }])
write('model-info', [{ id: 'ollama-hosted' }])
