// Generates synthetic inputs for the real Hanurai and HypaProcesser oracle.
import { mkdirSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const chat = (content, extra = {}) => ({ role: 'user', content, ...extra })
const recent = [chat('query old'), chat('query middle'), chat('query newest')]
const cases = []
const add = (id, chats, extra = {}) => cases.push({ id, input: {
    chats, currentTokens: 0, maxContextTokens: 1000, split: false, reserve: 0,
    tokenizer: { additionalTokens: 2, useName: 'name' },
    embeddings: { defaultDocument: [1], defaultQuery: [1] }, ...extra,
} })
for (let length = 0; length <= 5; length++) {
    add(`history-${length}`, Array.from({ length }, (_, index) => chat(`history ${index}`)))
}
add('blank-document-and-whitespace-query', [chat(' \t\n'), chat(''), chat(' \n'), chat('last')])
add('split-trims-and-keeps-duplicate-paragraphs', [chat(' alpha \n\n\n\n beta\n\nalpha '), ...recent], {
    split: true, currentTokens: 1000, reserve: 5,
})
add('unsplit-keeps-inner-whitespace', [chat(' alpha \n\n beta '), ...recent], { currentTokens: 1000, reserve: 5 })
add('inclusive-document-edge', [chat('first'), chat('edge'), ...recent], { currentTokens: 1000, reserve: 5 })
add('exact-surviving-content-suppression', [chat('same'), chat(' same'), ...recent], { currentTokens: 1000, reserve: 3 })
add('trimmed-text-differs-from-survivor', [chat(' same '), ...recent])
add('weighted-negative-zero-and-ties', [chat('A\n\nB\n\nC\n\nD'), ...recent], {
    split: true, currentTokens: 980, reserve: 25,
    embeddings: {
        documents: { 'search_document: A': [1, 0, 0], 'search_document: B': [0, 1, 0], 'search_document: C': [0, 0, 1], 'search_document: D': [-1, 0, 0] },
        queries: { 'search_query: query newest': [1, 0, 0], 'search_query: query middle': [0, 2, 0], 'search_query: query old': [0, 0, 0] },
    },
})
add('nan-score-reset-after-width-mismatch', [chat('A\n\nB'), ...recent], {
    split: true, currentTokens: 1000, reserve: 5,
    embeddings: { documents: { 'search_document: A': [1], 'search_document: B': [2] }, queries: {
        'search_query: query newest': [1, 1], 'search_query: query middle': [1], 'search_query: query old': [-1],
    } },
})
add('reserve-equality-does-not-trim', [chat('equal'), ...recent], { currentTokens: 90, maxContextTokens: 100, reserve: 10 })
add('candidate-equality-break-skips-cheaper-later', [chat('abcdefghijklmnopqrstuvwx\n\nx'), ...recent], {
    split: true, currentTokens: 27, maxContextTokens: 29, reserve: 3,
    embeddings: { documents: { 'search_document: abcdefghijklmnopqrstuvwx': [2], 'search_document: x': [1] }, defaultQuery: [1] },
})
add('accepted-before-equality-break', [chat('a\n\nbbbbbbbbbbbbbbbbbbbb\n\nc'), ...recent], {
    split: true, currentTokens: 24, maxContextTokens: 30, reserve: 9,
    embeddings: { documents: { 'search_document: a': [3], 'search_document: bbbbbbbbbbbbbbbbbbbb': [2], 'search_document: c': [1] }, defaultQuery: [1] },
})
add('reserve-exhaustion-empty-history', [], { currentTokens: 20, maxContextTokens: 10, reserve: 5 })
add('reserve-exhaustion-after-all-shifts', [chat('a'), chat('b')], { currentTokens: 30, maxContextTokens: 10, reserve: 5 })
add('empty-memory-with-zero-score-document', [chat('zero'), ...recent], { embeddings: { defaultDocument: [0], defaultQuery: [1] } })
add('utf16-astral-combining-and-crlf-split', [chat(' 😀 é \n\n 한글\r\n\r\n끝 '), ...recent], {
    split: true, currentTokens: 1000, reserve: 8,
})
const metadata = { role: 'assistant', name: 'Named 😀', memo: 'history', thoughts: ['not counted'], attr: ['synthetic'], removable: true,
    multimodals: [{ type: 'image', base64: 'synthetic', width: 1024, height: 768 }], cachePoint: true }
add('named-multimodal-message-trim', [chat('image', metadata), ...recent], { currentTokens: 1000, reserve: 10 })
add('no-name-message-trim', [chat('image', metadata), ...recent], { currentTokens: 1000, reserve: 10,
    tokenizer: { additionalTokens: 3, useName: 'noName' } })
const three = [chat('A'), chat('B'), chat('C'), ...recent]
add('cold-cache-tie-order', three, { currentTokens: 1000, reserve: 8 })
add('warm-cache-tie-order', three, { currentTokens: 1000, reserve: 8,
    cache: ['A', 'B', 'C'].map(text => ({ text: `search_document: ${text}`, vector: [1] })) })
add('fresh-before-cached-tie-order', three, { currentTokens: 1000, reserve: 8,
    cache: [{ text: 'search_document: A', vector: [1] }, { text: 'search_document: C', vector: [1] }] })
add('mixed-cache-width-discards-hits', three, { currentTokens: 1000, reserve: 8,
    cache: [{ text: 'search_document: A', vector: [1] }, { text: 'search_document: B', vector: [1, 0] }] })
add('stale-cache-reembeds-after-fresh', three, { currentTokens: 1000, reserve: 8,
    cache: [{ text: 'search_document: A', vector: [1, 0] }] })
add('cached-duplicate-multiplicity', [chat('dup\n\ndup'), ...recent], { split: true, currentTokens: 1000, reserve: 5,
    cache: [{ text: 'search_document: dup', vector: [1] }] })
add('more-than-fifty-documents-batch-order', [chat(Array.from({ length: 51 }, (_, index) => `part ${index}`).join('\n\n')), ...recent], {
    split: true, currentTokens: 1000, reserve: 5,
})
add('document-inference-failure', [chat('A'), ...recent], { embeddings: { defaultDocument: [1], defaultQuery: [1], fail: { at: 1, message: 'synthetic prepare failure' } } })
add('query-failure-after-cache-write', [chat('A'), ...recent], { embeddings: { defaultDocument: [1], defaultQuery: [1], fail: { at: 3, message: 'synthetic query failure 😀' } } })
add('tokenizer-failure-after-retrieval', [chat('A'), ...recent], { currentTokens: 1000, reserve: 5,
    tokenFailure: { at: 1, message: 'synthetic tokenizer failure' } })
add('candidate-tokenizer-failure-after-trim', [chat('A'), ...recent], { currentTokens: 1000, reserve: 3,
    tokenFailure: { at: 2, message: 'synthetic candidate failure' } })
const directory = fileURLToPath(new URL('../../tests/fixtures/rust-core/memory/hanurai/', import.meta.url))
mkdirSync(directory, { recursive: true })
writeFileSync(`${directory}/policy.cases.json`, `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`)
