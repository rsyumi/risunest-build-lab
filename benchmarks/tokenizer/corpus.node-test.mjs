import assert from 'node:assert/strict'
import test from 'node:test'

import {
    createOracleBenchmarkFixtures,
    loadTokenizerCorpus,
    percentile,
    verifyTokenizerCorpus,
} from './corpus.mjs'

test('both JavaScript oracle artifacts match every literal token ID and error', async () => {
    const corpus = await loadTokenizerCorpus()

    assert.deepEqual(await verifyTokenizerCorpus(corpus), {
        cl100k_base: { idCases: 34, errorCases: 10 },
        o200k_base: { idCases: 34, errorCases: 4 },
    })
})

test('oracle benchmark fixtures retain ordered batch and long-input shapes', async () => {
    const corpus = await loadTokenizerCorpus()
    const fixtures = createOracleBenchmarkFixtures(corpus, 'cl100k_base')

    assert.deepEqual(
        fixtures.map((fixture) => [fixture.name, fixture.texts.length]),
        [
            ['short-segments-1', 1],
            ['short-segments-10', 10],
            ['short-segments-100', 100],
            ['short-segments-1000', 1000],
            ['prompt-over-32-kib', 1],
            ['realistic-prompt-segments', 6],
        ],
    )
    assert.ok(Buffer.byteLength(fixtures[4].texts[0], 'utf8') >= 32 * 1024)
})

test('percentile uses nearest rank without mutating samples', () => {
    const samples = [9, 1, 5, 3]

    assert.equal(percentile(samples, 0.5), 3)
    assert.equal(percentile(samples, 0.95), 9)
    assert.deepEqual(samples, [9, 1, 5, 3])
})
