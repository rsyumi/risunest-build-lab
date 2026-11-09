// Generates synthetic inputs for the real HypaV3 oracle: settings, stored records, chats and boundary tapes.
import { mkdirSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const message = (role, content, memo, extra = {}) => ({ role, content, ...(memo === undefined ? {} : { memo }), ...extra })
// `count` alternating user and assistant messages with memos `${prefix}0`, `${prefix}1`, ...
const conversation = (count, prefix = 'c') => Array.from({ length: count }, (_, index) =>
    message(index % 2 ? 'assistant' : 'user', `${prefix} message ${index}`, `${prefix}${index}`))
const summary = (text, chatMemos, extra = {}) => ({ text, chatMemos, isImportant: false, ...extra })
const success = result => ({ reply: { type: 'success', result } })
const cases = []
const add = (id, input) => cases.push({ id, input: {
    settings: {}, maxResponse: 0, room: {}, summaries: [], invocations: [{ currentTokens: 0, maxContextTokens: 100 }], ...input,
} })
// Deterministic Math.random values in [0, 1).
const randomTape = (count, seed) => Array.from({ length: count }, () => {
    seed = (seed * 1103515245 + 12345) % 2147483648
    return seed / 2147483648
})
const noSimilar = { recentMemoryRatio: 1, similarMemoryRatio: 0 }

// No stored summaries.
add('empty-chats-reserve-empty-wrapper', { chats: [] })
add('short-history-reserves-empty-wrapper', { chats: conversation(3), invocations: [{ currentTokens: 15, maxContextTokens: 100 }] })
add('short-history-over-limit-cannot-summarize', { chats: conversation(3), invocations: [{ currentTokens: 98, maxContextTokens: 100 }] })
add('ratio-sum-over-one', { chats: conversation(3), settings: { recentMemoryRatio: 0.7, similarMemoryRatio: 0.4 } })
add('max-response-subtracted-first', { chats: conversation(3), maxResponse: 7.5, invocations: [{ currentTokens: 0.1, maxContextTokens: 100 }] })

// Summarization batches.
add('summarize-two-batches-recent-selection', {
    chats: conversation(14), settings: { ...noSimilar, maxChatsPerSummary: 4, memoryTokensRatio: 0.3 },
    invocations: [{ currentTokens: 110, maxContextTokens: 100 }],
    summaries: [success('First summary.'), success('Second summary.')],
})
add('skip-rules-count-and-deduct', {
    chats: [
        message('user', 'example one', 'e0', { name: 'example_user' }),
        message('assistant', 'example two', 'e1', { name: 'example_assistant' }),
        message('user', 'example three', 'NewChatExample'),
        message('assistant', 'greeting', 'NewChat'),
        message('user', ' \t\n ', 'w0'),
        message('user', 'user line', 'u0'),
        message('assistant', 'assistant line', 'a0'),
        ...conversation(4, 'r'),
    ],
    settings: { ...noSimilar, doNotSummarizeUserMessage: true, maxChatsPerSummary: 7 },
    invocations: [{ currentTokens: 95, maxContextTokens: 60 }],
    summaries: [success('Assistant summary.')],
})
add('all-skipped-batch-adds-no-summary', {
    chats: [message('user', 'example', 'e0', { name: 'example_user' }), message('user', '   ', 'w0'), ...conversation(3, 'r')],
    settings: { ...noSimilar, maxChatsPerSummary: 2 },
    invocations: [{ currentTokens: 58, maxContextTokens: 60 }],
})
add('stop-before-extra-target', {
    chats: conversation(12), settings: { ...noSimilar, extraSummarizationRatio: 0.1, maxChatsPerSummary: 4 },
    invocations: [{ currentTokens: 96, maxContextTokens: 100 }],
    summaries: [success('Only summary.')],
})
add('inlay-tokens-sanitized-in-batch-text', {
    chats: [
        message('user', 'a {{inlay::abc}} b {{inlayed::x}} c {{inlayeddata::y}}', 'i0'),
        message('assistant', '{{inlay::}}}} {{inlay::a\nb}} {{inlays::x}} {{inlay::q}}z}}', 'i1'),
        message('user', '{{{{inlay::nested}}}} {{INLAY::upper}} {{inlayed::}}', 'i2'),
        ...conversation(3, 'r'),
    ],
    settings: { ...noSimilar, maxChatsPerSummary: 3 },
    invocations: [{ currentTokens: 60, maxContextTokens: 50 }],
    summaries: [success('Sanitized summary.')],
})
add('summary-thoughts-stripped-and-trimmed', {
    chats: conversation(6), settings: { ...noSimilar, maxChatsPerSummary: 3 },
    invocations: [{ currentTokens: 60, maxContextTokens: 50 }],
    summaries: [success('  <Thoughts>one</Thoughts>Kept\n<Thoughts>multi\nline</Thoughts> tail <Thoughts>open  ')],
})
add('summary-failure-after-success-persists', {
    chats: conversation(12), settings: { ...noSimilar, maxChatsPerSummary: 3 },
    invocations: [{ currentTokens: 120, maxContextTokens: 60 }],
    summaries: [success('Kept summary.'), { reply: { type: 'fail', result: 'synthetic provider failure' } }],
})
add('summary-empty-reply', {
    chats: conversation(6), settings: { ...noSimilar, maxChatsPerSummary: 3 },
    invocations: [{ currentTokens: 60, maxContextTokens: 50 }], summaries: [success(' \n ')],
})
add('summary-only-thoughts', {
    chats: conversation(6), settings: { ...noSimilar, maxChatsPerSummary: 3 },
    invocations: [{ currentTokens: 60, maxContextTokens: 50 }], summaries: [success('<Thoughts>hidden</Thoughts>  ')],
})
add('summary-request-throws', {
    chats: conversation(6), settings: { ...noSimilar, maxChatsPerSummary: 3 },
    invocations: [{ currentTokens: 60, maxContextTokens: 50 }], summaries: [{ throw: 'synthetic network failure' }],
})
add('max-chats-one-per-batch', {
    chats: conversation(7), settings: { ...noSimilar, maxChatsPerSummary: 1 },
    invocations: [{ currentTokens: 45, maxContextTokens: 40 }],
    summaries: [success('One.'), success('Two.'), success('Three.')],
})
add('query-chat-count-zero-uses-whole-history', {
    chats: conversation(4), settings: { recentMemoryRatio: 0, similarMemoryRatio: 1, queryChatCount: 0, maxChatsPerSummary: 2 },
    room: { hypaV3Data: { summaries: [summary('Stored\n\nchunk', ['c0'])] } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})

// Stored records and continuation.
const stored = [summary('Old one.', ['c0', 'c1']), summary('Old two.', ['c2', 'c3'])]
add('continue-after-last-summary-memo', {
    chats: conversation(8), settings: noSimilar, room: { hypaV3Data: { summaries: stored } },
    invocations: [{ currentTokens: 40, maxContextTokens: 100 }],
})
add('last-memo-not-found-starts-at-zero', {
    chats: conversation(6), settings: { ...noSimilar, preserveOrphanedMemory: true },
    room: { hypaV3Data: { summaries: [summary('Gone.', ['gone'])] } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('empty-last-memo-set-matches-memo-less-chat', {
    chats: [...conversation(2), message('user', 'no memo'), ...conversation(2, 'r')], settings: noSimilar,
    room: { hypaV3Data: { summaries: [summary('Empty set.', [])] } },
    invocations: [{ currentTokens: 25, maxContextTokens: 100 }],
})
add('null-memo-reads-undefined-and-deduplicates', {
    chats: [...conversation(2), message('user', 'memo-less'), ...conversation(2, 'r')], settings: noSimilar,
    room: { hypaV3Data: { summaries: [summary('Nulls.', ['c0', null, 'c0', 'c1', null])] } },
    invocations: [{ currentTokens: 25, maxContextTokens: 100 }],
})
add('orphan-cleanup-removes-missing-memos', {
    chats: conversation(6), settings: noSimilar,
    room: { hypaV3Data: { summaries: [summary('Keep.', ['c0']), summary('Drop.', ['c1', 'missing']), summary('Number memo.', [5]),
        summary('Keep empty.', []), summary('Keep last.', ['c2'])] } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('preserve-orphaned-memory-keeps-all', {
    chats: conversation(6), settings: { ...noSimilar, preserveOrphanedMemory: true },
    room: { hypaV3Data: { summaries: [summary('Keep.', ['c0']), summary('Orphan.', ['missing'])] } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('window-effective-memos-keep-summaries', {
    chats: conversation(4, 'w'), settings: noSimilar, prepared: { effectiveMessageMemos: ['old0', 'old1', 'w0'] },
    room: { hypaV3Data: { summaries: [summary('Window old.', ['old0', 'old1']), summary('Window gone.', ['old2'])] } },
    invocations: [{ currentTokens: 20, maxContextTokens: 100 }],
})
add('boundary-prepared-starts-at-history-index', {
    chats: conversation(6), settings: noSimilar,
    prepared: { boundaryMemo: 'old1', effectiveMessageMemos: ['old0', 'old1', 'c0'], historyStartIndex: 2 },
    room: { hypaV3Data: { summaries: [summary('Boundary.', ['old0', 'old1'])] } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('boundary-prepared-stale-throws', {
    chats: conversation(6), settings: noSimilar,
    prepared: { boundaryMemo: 'other', effectiveMessageMemos: ['old0', 'old1'], historyStartIndex: 2 },
    room: { hypaV3Data: { summaries: [summary('Boundary.', ['old0', 'old1'])] } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('boundary-prepared-without-summaries-starts-at-zero', {
    chats: conversation(6), settings: noSimilar,
    prepared: { boundaryMemo: 'other', effectiveMessageMemos: ['c0'], historyStartIndex: 2 },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('stored-shape-preserved-and-metrics-replaced', {
    chats: conversation(6), settings: noSimilar,
    room: { note: 'room field', hypaV3Data: {
        categories: [{ id: 'k', name: 'Category' }], lastSelectedSummaries: [0],
        summaries: [
            { ['__proto__']: 'own', text: 'Shaped.', isImportant: true, chatMemos: ['c0'], categoryId: 'k', tags: ['t'], extra: { nested: [1, null] } },
            summary('Plain.', ['c1'], { tags: [] }),
        ],
        metrics: { lastImportantSummaries: [9], lastRecentSummaries: [], lastSimilarSummaries: [], lastRandomSummaries: [] },
        modalSettings: { displayMode: 'all', displayRangeFrom: 0, displayRangeTo: 1, displayRecentCount: 2, displayImportant: true, displaySelected: false },
        '7': 'index key', unknown: { $number: '-0' },
    } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('second-invocation-reads-json-memory', {
    chats: conversation(10), settings: { ...noSimilar, maxChatsPerSummary: 3 },
    invocations: [{ currentTokens: 70, maxContextTokens: 60 }, { currentTokens: 70, maxContextTokens: 60, persistPrevious: true }],
    summaries: [success('First run.'), success('Second run.')],
})
add('utf16-content-and-memos', {
    chats: [message('user', { $utf16: [97, 0xd800, 98] }, { $utf16: [0xdc00, 120] }), message('assistant', 'astral 😀 text', 'a😀'), ...conversation(3, 'r')],
    settings: { ...noSimilar, maxChatsPerSummary: 2 },
    invocations: [{ currentTokens: 40, maxContextTokens: 30 }],
    summaries: [success('Summary 😀 with astral.')],
})

// Important, recent, similar and random selection.
const five = ['Alpha fact.', 'Beta fact.', 'Gamma fact.', 'Delta fact.', 'Epsilon fact.']
const storedFive = (extra = () => ({})) => five.map((text, index) => summary(text, [`c${index}`], extra(index)))
add('important-selection-breaks-at-first-oversize', {
    chats: conversation(8), settings: { recentMemoryRatio: 1, similarMemoryRatio: 0, memoryTokensRatio: 0.3 },
    room: { hypaV3Data: { summaries: storedFive(index => ({ isImportant: index !== 2 })) } },
    tokenizer: { overrides: [{ text: 'Beta fact.\n\n', length: 40 }] },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('recent-selection-from-end-breaks', {
    chats: conversation(8), settings: { recentMemoryRatio: 1, similarMemoryRatio: 0, memoryTokensRatio: 0.2 },
    room: { hypaV3Data: { summaries: storedFive() } },
    tokenizer: { overrides: [{ text: 'Gamma fact.\n\n', length: 20 }] },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
const similarSummaries = [summary('Dragons\n\nCastles', ['c0']), summary('Rivers\n\n \n\nDragons', ['c1']), summary('Forests', ['c2']), summary('Moons\n\nTides', ['c3'])]
const similarVectors = { Dragons: [1, 0, 0], Castles: [0, 1, 0], Rivers: [0, 0, 1], Forests: [0.5, 0.5, 0], Moons: [0, 0.25, 0.75], Tides: [-1, 0, 0],
    'c message 7': [1, 0.1, 0], 'c message 6': [0, 1, 0], 'c message 5': [0, 0, 1] }
add('similar-selection-ranks-chunks-to-summaries', {
    chats: conversation(8), settings: { recentMemoryRatio: 0.3, similarMemoryRatio: 0.7, memoryTokensRatio: 0.3 },
    room: { hypaV3Data: { summaries: similarSummaries } }, embeddings: { vectors: similarVectors },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('similar-correction-adds-summary-query', {
    chats: conversation(8), settings: { recentMemoryRatio: 0, similarMemoryRatio: 1, memoryTokensRatio: 0.2, enableSimilarityCorrection: true },
    room: { hypaV3Data: { summaries: similarSummaries } }, embeddings: { vectors: { ...similarVectors, 'Recent correction.': [0, 0.5, 0.5] } },
    summaries: [success('Recent correction.')],
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('similar-correction-failure-persists', {
    chats: conversation(8), settings: { recentMemoryRatio: 0, similarMemoryRatio: 1, enableSimilarityCorrection: true },
    room: { hypaV3Data: { summaries: similarSummaries } }, embeddings: { vectors: similarVectors },
    summaries: [{ reply: { type: 'fail', result: 'synthetic correction failure' } }],
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('similar-no-recent-queries-after-prepare', {
    chats: [...conversation(4), message('user', ' ', 'b0'), message('assistant', '\n', 'b1'), message('user', '', 'b2')],
    settings: { recentMemoryRatio: 0, similarMemoryRatio: 1, enableSimilarityCorrection: true },
    room: { hypaV3Data: { summaries: similarSummaries } }, embeddings: { vectors: similarVectors },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('similar-empty-chunk-corpus', {
    chats: conversation(6), settings: { recentMemoryRatio: 0, similarMemoryRatio: 1 },
    room: { hypaV3Data: { summaries: [summary(' \n\n ', ['c0']), summary('', ['c1'])] } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('similar-duplicate-chunk-text-and-ties', {
    chats: conversation(6), settings: { recentMemoryRatio: 0, similarMemoryRatio: 1, memoryTokensRatio: 0.5 },
    room: { hypaV3Data: { summaries: [summary('Same\n\nOne', ['c0']), summary('Same', ['c1']), summary('Two\n\nSame', ['c2'])] } },
    embeddings: { vectors: { Same: [1, 0], One: [1, 0], Two: [0, 0], 'c message 5': [1, 0], 'c message 4': [1, 1], 'c message 3': [0, 1] } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('similar-nan-scores-from-width-mismatch', {
    chats: conversation(6), settings: { recentMemoryRatio: 0, similarMemoryRatio: 1, memoryTokensRatio: 0.5 },
    room: { hypaV3Data: { summaries: [summary('Wide', ['c0']), summary('Narrow', ['c1']), summary('Zero', ['c2'])] } },
    embeddings: { vectors: { Wide: [1, 1], Narrow: [1], Zero: [0, 0], 'c message 5': [1, 1], 'c message 4': [2], 'c message 3': [-1, 0] } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('similar-prepare-failure-persists', {
    chats: conversation(6), settings: { recentMemoryRatio: 0, similarMemoryRatio: 1 },
    room: { hypaV3Data: { summaries: similarSummaries.slice(0, 2) } },
    embeddings: { vectors: similarVectors, fail: { purpose: 'document', message: 'synthetic document failure' } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('similar-query-failure-persists', {
    chats: conversation(8), settings: { recentMemoryRatio: 0, similarMemoryRatio: 1 },
    room: { hypaV3Data: { summaries: similarSummaries.slice(0, 2) } },
    embeddings: { vectors: similarVectors, fail: { purpose: 'query', text: 'c message 6', message: 'synthetic query failure' } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('similar-candidate-tokenizer-failure-persists', {
    chats: conversation(8), settings: { recentMemoryRatio: 0, similarMemoryRatio: 1 },
    room: { hypaV3Data: { summaries: similarSummaries } }, embeddings: { vectors: similarVectors },
    tokenizer: { failText: 'Forests\n\n' },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('random-selection-continues-past-oversize', {
    chats: conversation(8), settings: { recentMemoryRatio: 0, similarMemoryRatio: 0, memoryTokensRatio: 0.2 },
    room: { hypaV3Data: { summaries: storedFive() } },
    tokenizer: { overrides: [{ text: 'Beta fact.\n\n', length: 30 }] }, random: randomTape(16, 7),
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
add('random-shuffle-seventy-merges', {
    chats: conversation(8), settings: { recentMemoryRatio: 0, similarMemoryRatio: 0, memoryTokensRatio: 0.5 },
    room: { hypaV3Data: { summaries: Array.from({ length: 70 }, (_, index) => summary(`Fact ${index}.`, [])) } },
    random: randomTape(600, 12345),
    invocations: [{ currentTokens: 30, maxContextTokens: 200 }],
})
add('all-three-categories-default-ratios', {
    chats: conversation(10), settings: { memoryTokensRatio: 0.4 },
    room: { hypaV3Data: { summaries: [...similarSummaries, ...storedFive().map((entry, index) => ({ ...entry, chatMemos: [`c${index + 4}`] }))] } },
    embeddings: { vectors: similarVectors }, random: randomTape(40, 99),
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})

// Chunk separators.
const separatorCase = (id, summaryChunkSeparator, text) => add(id, {
    chats: conversation(6), settings: { recentMemoryRatio: 0, similarMemoryRatio: 1, summaryChunkSeparator },
    room: { hypaV3Data: { summaries: [summary(text, ['c0'])] } },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})
separatorCase('separator-plain-literal', '---', 'a---b------ c ---')
separatorCase('separator-regex-literal-form', '/--/g', 'x--y----z')
separatorCase('separator-empty-splits-units', '', 'ab c')
separatorCase('separator-escapes', '\\n\\t\\.', 'p\n\t.q\n\t.r')
separatorCase('separator-actual-newlines', '\n', 'one\ntwo\n\nthree')
separatorCase('separator-trailing-backslash-falls-back', '\\', 'u\\v\n\nw')
separatorCase('separator-duplicate-flags-fall-back', '/x/gg', 'axb\n\nc')

// Thrown errors.
add('final-overflow-throws-without-memory', {
    chats: conversation(8), settings: { ...noSimilar, maxChatsPerSummary: 3 },
    invocations: [{ currentTokens: 60, maxContextTokens: 50 }],
    summaries: [success('First.'), success('Overflow summary.')],
    tokenizer: { overrides: [{ text: '<Past Events Summary>\nOverflow summary.\n</Past Events Summary>', length: 200 }] },
})
add('prefix-tokenizer-failure-throws', {
    chats: conversation(6), settings: noSimilar, room: { hypaV3Data: { summaries: [summary('Stored.', ['c1'])] } },
    tokenizer: { failText: 'c message 1' },
    invocations: [{ currentTokens: 30, maxContextTokens: 100 }],
})

const directory = fileURLToPath(new URL('../../tests/fixtures/rust-core/memory/hypa-v3/', import.meta.url))
mkdirSync(directory, { recursive: true })
writeFileSync(`${directory}/policy.cases.json`, `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`)
