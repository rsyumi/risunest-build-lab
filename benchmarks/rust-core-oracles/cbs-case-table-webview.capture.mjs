// Records the case mapping of every code point in a WebView's JavaScript engine into
// tests/fixtures/rust-core/cbs/case-table-webview.expected.json, the table `cbs-host.oracle.test.ts` records in Node,
// so the Rust tests can pin their Unicode 17 tables to the engine the app runs in. A contract set: the page restates the
// CBS `lower`, `upper` and `capitalize` callbacks of src/ts/cbsPure.ts and the Final_Sigma classes of the Node oracle,
// and the engine, not TS, decides the results.
//
//   node benchmarks/rust-core-oracles/cbs-case-table-webview.capture.mjs --browser <msedge.exe>
//   node benchmarks/rust-core-oracles/cbs-case-table-webview.capture.mjs --cdp http://127.0.0.1:<port>
//
// `webviewEngine.mjs` describes both options. RUST_CORE_ORACLE_WRITE=1 writes the expected file; otherwise the capture
// is compared with it.
import { readOracleCases, settleOracle } from './oracleFile.ts'
import { evaluateInEngine } from './webviewEngine.mjs'

// Runs in the engine. Each table lists the code points the callback maps to something else, a surrogate code point
// as its lone unit; each class run holds bit 1 when a capital sigma before the code point stays medial and bit 0 when
// one after it is final.
function capture() {
    const functions = {
        lower: (text) => text.toLocaleLowerCase(),
        upper: (text) => text.toLocaleUpperCase(),
        capitalize: (text) => text.charAt(0).toUpperCase() + text.slice(1),
    }
    const tables = { lower: [], upper: [], capitalize: [] }
    const classes = []
    for (let code = 0; code <= 0x10ffff; code++) {
        const text = code >= 0xd800 && code <= 0xdfff ? String.fromCharCode(code) : String.fromCodePoint(code)
        for (const name of Object.keys(functions)) {
            const mapped = functions[name](text)
            if (mapped !== text) tables[name].push([code, mapped])
        }
        const sigma = Number(functions.lower('ΑΣ' + text)[1] === 'σ') * 2 + Number(functions.lower('Α' + text + 'Σ').endsWith('ς'))
        const last = classes.at(-1)
        if (last && last[2] === sigma) last[1] = code
        else classes.push([code, code, sigma])
    }
    const locale = new Intl.DateTimeFormat().resolvedOptions().locale
    return { userAgent: navigator.userAgent, locale, ...tables, classes }
}

const set = readOracleCases('cbs', 'case-table-webview')
const result = await evaluateInEngine(`(${capture})()`, 'case-table-webview-')
console.log(`${result.userAgent}: lower ${result.lower.length}, upper ${result.upper.length}, capitalize ${result.capitalize.length}, classes ${result.classes.length}`)
console.log(settleOracle(set, [result], { generator: import.meta.url, tsSources: [] }))
