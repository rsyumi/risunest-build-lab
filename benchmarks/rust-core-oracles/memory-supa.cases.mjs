// Generates synthetic inputs for the real SupaMemory oracle: settings, stored records, chats and boundary tapes.
import { mkdirSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const message = (role, content, memo, extra = {}) => ({ role, content, ...(memo === undefined ? {} : { memo }), ...extra })
// `count` alternating user and assistant messages with memos `${prefix}0`, `${prefix}1`, ...
const conversation = (count, prefix = 'c', from = 0) => Array.from({ length: count }, (_, index) =>
    message((from + index) % 2 ? 'assistant' : 'user', `${prefix} message ${from + index}`, `${prefix}${from + index}`))
const success = result => ({ reply: { type: 'success', result } })
const write = (set, cases) => {
    const directory = fileURLToPath(new URL(`../../tests/fixtures/rust-core/memory/${set}/`, import.meta.url))
    mkdirSync(directory, { recursive: true })
    writeFileSync(`${directory}policy.cases.json`, `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`)
}

const supa = []
const add = (id, input) => supa.push({ id, input: {
    settings: {}, charName: 'Kai', room: {}, summaries: [], invocations: [{ currentTokens: 0, maxContextTokens: 60 }], ...input,
} })

// Under the limit and the limit itself.
add('under-limit-unchanged', { chats: conversation(4), invocations: [{ currentTokens: 40, maxContextTokens: 60 }] })
add('at-limit-unchanged', { chats: conversation(4), invocations: [{ currentTokens: 50, maxContextTokens: 60 }] })

// Stored records and the NewChat prefix.
add('newchat-trims-prefix-then-fits', {
    chats: [message('user', 'before one', 'b0'), message('assistant', 'before two', 'b1'), message('assistant', 'greeting', 'NewChat'), ...conversation(3)],
    invocations: [{ currentTokens: 55, maxContextTokens: 60 }],
})
add('newchat-first-keeps-everything', {
    chats: [message('assistant', 'greeting', 'NewChat'), ...conversation(7)], invocations: [{ currentTokens: 61, maxContextTokens: 60 }],
    summaries: [success('Greeting summary.')],
})
add('stored-id-skips-prefix-and-fits', {
    chats: conversation(6), room: { supaMemoryData: 'c2\nEarlier events.' }, invocations: [{ currentTokens: 55, maxContextTokens: 60 }],
})
add('stored-punctuation-removed', {
    chats: conversation(6), settings: { removePunctuationHypa: true }, room: { supaMemoryData: 'c2\nEarlier, events: (kept)! a-b_c ~ok.' },
    invocations: [{ currentTokens: 55, maxContextTokens: 60 }],
})
add('stored-multiline-data-and-crlf-id', {
    chats: [...conversation(2), message('user', 'crlf memo', 'c2\r'), ...conversation(3, 'c', 3)],
    room: { supaMemoryData: 'c2\r\nFirst line.\nSecond line.' }, invocations: [{ currentTokens: 55, maxContextTokens: 60 }],
})
add('stored-id-not-found', { chats: conversation(4), room: { supaMemoryData: 'zz\nNever matched.' }, invocations: [{ currentTokens: 55, maxContextTokens: 60 }] })
add('stored-short-record-ignored', {
    chats: conversation(8), room: { supaMemoryData: 'c1\nx' }, invocations: [{ currentTokens: 56, maxContextTokens: 60 }],
    summaries: [success('Sum A.')],
})
add('hypa-record-read-as-supa', { chats: conversation(4), room: { supaMemoryData: 'hypa:\n[]' }, invocations: [{ currentTokens: 55, maxContextTokens: 60 }] })

// Chunked summarization.
add('one-chunk-summary', { chats: conversation(8), invocations: [{ currentTokens: 66, maxContextTokens: 60 }], summaries: [success('Sum A.')] })
add('first-chat-oversize-is-included', {
    chats: [message('user', 'big', 'big0'), ...conversation(5, 'c', 1)], tokenizer: { overrides: [{ text: 'big', length: 30 }] },
    invocations: [{ currentTokens: 70, maxContextTokens: 60 }], summaries: [success('Big summary.')],
})
add('resummarize-after-four-entries', {
    chats: conversation(8), room: { supaMemoryData: 'c0\nA one\n\nB two\n\nC three\n\nD four' },
    invocations: [{ currentTokens: 63, maxContextTokens: 60 }], summaries: [success('Chunk sum.'), success('Merged.')],
})
add('ran-out-resummarizes-memory-then-fits', {
    chats: conversation(3), room: { supaMemoryData: 'c1\nA b c d' }, tokenizer: { overrides: [{ text: 'A b c d', length: 15 }] },
    invocations: [{ currentTokens: 51, maxContextTokens: 60 }], summaries: [success('Short.')],
})
add('ran-out-twice-not-enough-tokens', {
    chats: conversation(3), room: { supaMemoryData: 'c0\nA b c d' }, invocations: [{ currentTokens: 51, maxContextTokens: 60 }],
    summaries: [success('Short.')],
})
add('empty-chats-over-limit', { chats: [], invocations: [{ currentTokens: 60, maxContextTokens: 60 }], summaries: [success('Nothing.')] })
add('summary-fail-reply', {
    chats: conversation(8), invocations: [{ currentTokens: 66, maxContextTokens: 60 }], summaries: [{ reply: { type: 'fail', result: 'quota' } }],
})
add('summary-request-throws', { chats: conversation(8), invocations: [{ currentTokens: 66, maxContextTokens: 60 }], summaries: [{ throw: 'network down' }] })
add('tokenizer-failure-throws', {
    chats: conversation(8), tokenizer: { failText: 'c message 2' }, invocations: [{ currentTokens: 66, maxContextTokens: 60 }],
})
add('memo-less-break-chat-gives-undefined-last-id', {
    chats: [...conversation(4), message('user', 'no memo here'), ...conversation(3, 'c', 5)],
    invocations: [{ currentTokens: 66, maxContextTokens: 60 }], summaries: [success('Sum A.')],
})
add('names-by-role-in-batch-text', {
    chats: [message('user', 'hello there', 'n0'), message('assistant', 'hi back', 'n1'), message('system', 'a note', 'n2'),
        message('user', 'named', 'n3', { name: 'Someone' }), ...conversation(4, 'c', 4)],
    settings: { username: 'Rin' }, invocations: [{ currentTokens: 66, maxContextTokens: 60 }], summaries: [success('Names summary.'), success('Second names summary.')],
})
add('max-supa-chunk-size-caps-batches', {
    chats: conversation(8), settings: { maxSupaChunkSize: 10 }, invocations: [{ currentTokens: 66, maxContextTokens: 60 }],
    summaries: [success('Pair one.'), success('Pair two.'), success('Pair three.')],
})
add('result-newlines-collapse', {
    chats: conversation(8), invocations: [{ currentTokens: 66, maxContextTokens: 60 }], summaries: [success('Line one\n\n\nLine two\r\n')],
})
add('second-invocation-reads-stored-memory', {
    chats: conversation(10), invocations: [{ currentTokens: 66, maxContextTokens: 60 }, { currentTokens: 66, maxContextTokens: 60, persistPrevious: true }],
    summaries: [success('Sum A.')],
})
add('utf16-content-and-memos', {
    chats: [message('user', 'lone \ud800 unit', 'u\udc00'), ...conversation(7, 'c', 1)],
    invocations: [{ currentTokens: 66, maxContextTokens: 60 }], summaries: [success('Surrogate \udfff summary.')],
})

// The `$utf16` tag carries lone surrogates through JSON.
const tag = value => {
    if (Array.isArray(value)) return value.map(tag)
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, tag(child)]))
    if (typeof value === 'string' && /[\ud800-\udfff]/.test(value) && !/^(?:[^\ud800-\udfff]|[\ud800-\udbff][\udc00-\udfff])*$/.test(value)) {
        return { $utf16: Array.from({ length: value.length }, (_, index) => value.charCodeAt(index)) }
    }
    return value
}
write('supa', tag(supa))

// Legacy Hypa: SupaMemory with `asHyper`, whose record is `hypa:` and a JSON array of `{ id, supa, hypa }`.
const legacy = []
const hyper = (id, input) => legacy.push({ id, input: {
    settings: {}, charName: 'Kai', room: {}, summaries: [], invocations: [{ currentTokens: 0, maxContextTokens: 60 }], ...input,
} })
const record = entries => ({ supaMemoryData: `hypa:\n${JSON.stringify(entries, null, 2)}` })

hyper('under-limit-unchanged', { chats: conversation(4), invocations: [{ currentTokens: 50, maxContextTokens: 60 }] })
hyper('first-run-writes-hypa-record', { chats: conversation(8), invocations: [{ currentTokens: 66, maxContextTokens: 60 }], summaries: [success('Sum A.')] })
hyper('plain-record-ignored', {
    chats: conversation(8), room: { supaMemoryData: 'c2\nEarlier events.' }, invocations: [{ currentTokens: 66, maxContextTokens: 60 }],
    summaries: [success('Sum A.')],
})
hyper('stored-record-retrieves-past-events', {
    chats: conversation(8), room: record([{ id: 'c2', supa: 'Old summary.', hypa: ['Alpha event.', 'Beta event.', 'Gamma event.', 'Delta event.', 'x', 'Old summary.'] }]),
    invocations: [{ currentTokens: 51, maxContextTokens: 60 }], summaries: [success('Chunk.')],
})
hyper('ran-out-updates-first-entry-in-place', {
    chats: conversation(3), room: record([{ supa: 'A b c d', extra: 'kept', id: 'c1', hypa: ['Alpha event.'] }]),
    tokenizer: { overrides: [{ text: 'A b c d', length: 15 }] }, invocations: [{ currentTokens: 51, maxContextTokens: 60 }], summaries: [success('Short.')],
})
hyper('record-not-array-after-newchat-trim', {
    chats: [message('user', 'before', 'b0'), message('assistant', 'greeting', 'NewChat'), ...conversation(3)],
    room: { supaMemoryData: 'hypa:\n {"id": "c1"} ' }, invocations: [{ currentTokens: 55, maxContextTokens: 60 }],
})
hyper('record-id-not-found-counts-every-entry', {
    chats: conversation(3), room: record([{ id: 'zz', supa: 's', hypa: [] }, { id: 'yy', supa: 't', hypa: [] }]),
    invocations: [{ currentTokens: 55, maxContextTokens: 60 }],
})
hyper('second-entry-selected-with-punctuation-removed', {
    chats: conversation(8), settings: { removePunctuationHypa: true },
    room: record([{ id: 'zz', supa: 's', hypa: [] }, { id: 'c1', supa: 'Second.', hypa: ['Alpha event.', 'Alpha event.', 'Beta, event!'] }]),
    invocations: [{ currentTokens: 51, maxContextTokens: 60 }], summaries: [success('Chunk.')],
})
hyper('merge-after-three-entries', {
    chats: conversation(8), room: record([{ id: 'c0', supa: 'A one\n\nB two\n\nC three', hypa: [] }]),
    invocations: [{ currentTokens: 51, maxContextTokens: 60 }], summaries: [success('Chunk\n\nsum.'), success('Merged.')],
})
hyper('second-invocation-reparses-written-record', {
    chats: conversation(10), invocations: [{ currentTokens: 66, maxContextTokens: 60 }, { currentTokens: 66, maxContextTokens: 60, persistPrevious: true }],
    summaries: [success('Sum A.')],
})
hyper('memo-less-break-writes-entry-without-id', {
    chats: [...conversation(4), message('user', 'no memo here'), ...conversation(3, 'c', 5)],
    invocations: [{ currentTokens: 66, maxContextTokens: 60 }, { currentTokens: 66, maxContextTokens: 60, persistPrevious: true }],
    summaries: [success('Sum A.')],
})
hyper('query-skips-system-and-inlay-and-names-speakers', {
    chats: [...conversation(4), message('user', 'start', 'c4'), message('system', 'note', 'n5'), message('user', 'img', 'inlayImage-6'),
        message('user', 'named', 'c7', { name: 'Someone' }), message('assistant', 'reply', 'c8'), message('user', 'last', 'c9')],
    room: record([{ id: 'c4', supa: 'Base.', hypa: ['Alpha event.', 'Beta event.', 'Gamma event.', 'Delta event.'] }]),
    invocations: [{ currentTokens: 52, maxContextTokens: 60 }],
})
hyper('document-embedding-failure-throws', {
    chats: conversation(8), room: record([{ id: 'c1', supa: 'Base.', hypa: ['Alpha event.'] }]), embeddings: { fail: { purpose: 'document', message: 'embed down' } },
    invocations: [{ currentTokens: 51, maxContextTokens: 60 }],
})
hyper('query-embedding-failure-throws', {
    chats: conversation(8), room: record([{ id: 'c1', supa: 'Base.', hypa: ['Alpha event.'] }]), embeddings: { fail: { purpose: 'query', message: 'query down' } },
    invocations: [{ currentTokens: 51, maxContextTokens: 60 }],
})
hyper('utf16-chunks-round-trip', {
    chats: [message('user', 'lone \ud800 unit', 'u\udc00'), ...conversation(9, 'c', 1)],
    invocations: [{ currentTokens: 66, maxContextTokens: 60 }, { currentTokens: 66, maxContextTokens: 60, persistPrevious: true }],
    summaries: [success('Surrogate \udfff summary.')],
})
write('hypa-legacy', tag(legacy))
