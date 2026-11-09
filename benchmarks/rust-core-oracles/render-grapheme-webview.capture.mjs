// Records where a WebView's `Intl.Segmenter` ends the first grapheme cluster into
// tests/fixtures/rust-core/render/grapheme-webview.expected.json: the recording `render-display.oracle.test.ts` makes
// in Node (grapheme.expected.json), restated for the page, so the Rust rules of the thought preview can be checked
// against the engine the app runs in. A contract set: the engine, not TS, decides the results.
//
//   node benchmarks/rust-core-oracles/render-grapheme-webview.capture.mjs --browser <msedge.exe>
//   node benchmarks/rust-core-oracles/render-grapheme-webview.capture.mjs --cdp http://127.0.0.1:<port>
//   node benchmarks/rust-core-oracles/render-grapheme-webview.capture.mjs --node
//
// `webviewEngine.mjs` describes the first two. `--node` runs the restatement in this Node instead and checks it against
// the Node recording. RUST_CORE_ORACLE_WRITE=1 writes the expected file; otherwise the capture is compared with it.
import { readFileSync } from 'node:fs'
import { isDeepStrictEqual } from 'node:util'
import { readOracleCases, settleOracle } from './oracleFile.ts'
import { evaluateInEngine } from './webviewEngine.mjs'

// Runs in the engine: the cases of render-display.oracle.test.ts, plus the Unicode 18 case markers among the probes and
// the engine's identity.
function capture(cases) {
    const segmenter = new Intl.Segmenter(undefined, { granularity: 'grapheme' })
    const first = (text) => segmenter.segment(text)[Symbol.iterator]().next().value?.segment ?? ''
    // A surrogate code point stays one lone unit.
    const char = (code) => String.fromCodePoint(code)
    const joins = (lead, second) => first(char(lead) + char(second)).length > char(lead).length
    const ranges = (test) => {
        const found = []
        for (let code = 0; code <= 0x10ffff; code++) {
            if (!test(code)) continue
            const last = found.at(-1)
            if (last && last[1] === code - 1) last[1] = code
            else found.push([code, code])
        }
        return found
    }
    const standIn = (code) => code === 0x0a || code === 0x0d ? code
        : code < 0x20 || code === 0x7f ? 0x09
        : code >= 0xac00 && code <= 0xd7a3 ? ((code - 0xac00) % 28 === 0 ? 0xac00 : 0xac01)
        : 0x61
    const differs = (code, map) => map(char(code)) !== char(code)
    return cases.map(entry => {
        switch (entry.id) {
            case 'first-cluster': {
                const classMismatches = []
                for (const lead of entry.classLeads) {
                    for (const [start, end] of entry.simple) {
                        for (let second = start; second <= end; second++) {
                            if (joins(lead, second) !== joins(lead, standIn(second))) classMismatches.push([lead, second])
                        }
                    }
                }
                return { joined: entry.seconds.map(second => ranges(lead => joins(lead, second))), classMismatches }
            }
            case 'extend-after-a':
                return ranges(code => first('a' + char(code)).length > 1)
            case 'probes':
                return {
                    case16: char(entry.case16Small).toUpperCase() === char(entry.case16Capital),
                    case17: differs(entry.case17, text => text.toUpperCase()),
                    case18: differs(entry.case18Small, text => text.toUpperCase()) || differs(entry.case18Capital, text => text.toLowerCase()),
                    segment16: first(char(entry.segment16) + 'a') === char(entry.segment16) + 'a',
                    segment17: first('a' + char(entry.segment17)) === 'a' + char(entry.segment17),
                }
            case 'engine':
                return { userAgent: navigator.userAgent, locale: new Intl.DateTimeFormat().resolvedOptions().locale }
            default:
                throw new Error(`unknown grapheme case ${entry.id}`)
        }
    })
}

const set = readOracleCases('render', 'grapheme-webview')
if (process.argv.includes('--node')) {
    const node = JSON.parse(readFileSync(readOracleCases('render', 'grapheme').expectedPath, 'utf8')).cases.map(entry => entry.result)
    const results = capture(set.cases)
    const { case18: _, ...probes } = results[2]
    const same = [isDeepStrictEqual(results[0], node[0]), isDeepStrictEqual(results[1], node[1]), isDeepStrictEqual(probes, node[2])]
    console.log(`restatement in Node ${process.versions.node} (ICU ${process.versions.icu}) against grapheme.expected.json: ${same.every(Boolean) ? 'equal' : `differs ${same}`}`)
    if (!same.every(Boolean)) process.exitCode = 1
} else {
    const results = await evaluateInEngine(`(${capture})(${JSON.stringify(set.cases)})`, 'grapheme-webview-')
    console.log(`${results.at(-1).userAgent}: probes ${JSON.stringify(results[2])}, joined after 'a' ${results[0].joined[3].length} ranges`)
    console.log(settleOracle(set, results, { generator: import.meta.url, tsSources: [] }))
}
