// Synthetic typed settings for the actual echo_model catalog route.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const out = process.argv[2] ?? resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/echo')
const cases = [
    { id: 'absent' },
    { id: 'nullish', db: { echoMessage: null, echoDelay: null } },
    { id: 'empty', db: { echoMessage: '' } },
    { id: 'configured', db: { echoMessage: 'Synthetic Echo.' } },
    { id: 'utf16', db: { echoMessage: '한글 😀\u0000\r\n\ud800x\udfff' } },
    { id: 'zero', db: { echoMessage: 'Zero.', echoDelay: 0 } },
    { id: 'negative', db: { echoMessage: 'Negative.', echoDelay: -3 } },
    { id: 'fractional-negative', db: { echoMessage: 'Negative fraction.', echoDelay: -0.125 } },
    { id: 'positive', db: { echoMessage: 'Delayed.', echoDelay: 2 } },
    { id: 'fractional-positive', db: { echoMessage: 'Fraction.', echoDelay: 0.125 } },
    { id: 'sub-millisecond', db: { echoMessage: 'Sub-millisecond.', echoDelay: 0.0005 } },
    { id: 'delayed-default', db: { echoDelay: 1.5 } },
    { id: 'delayed-empty', db: { echoMessage: '', echoDelay: 0.5 } },
]
mkdirSync(out, { recursive: true })
writeFileSync(resolve(out, 'results.cases.json'), `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`)
