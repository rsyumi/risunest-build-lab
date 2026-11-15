// Writes the synthetic case files of the Lua edit-trigger oracle (tests/fixtures/rust-core/lua/*.cases.json). Add
// cases here, run `node benchmarks/rust-core-oracles/lua-edit.cases.mjs`, then regenerate the expected files with
// RUST_CORE_ORACLE_WRITE=1.
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/lua/', import.meta.url))
mkdirSync(dir, { recursive: true })

// A plain JSON string when well-formed, otherwise its UTF-16 units.
const units = (...values) => ({ u: values })

function write(set, cases) {
    const ids = new Set()
    for (const entry of cases) { if (ids.has(entry.id)) throw new Error(`duplicate ${set} ${entry.id}`); ids.add(entry.id) }
    const text = `{"schema": 1, "cases": [\n${cases.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`
    if (text.includes('\r')) throw new Error('CR in cases')
    writeFileSync(join(dir, `${set}.cases.json`), text)
    console.log(set, cases.length)
}

// The wrapper source the native host extracts, compared by hash.
write('wrapper', [{ id: 'empty-code' }])

// JS strings as Wasmoon pushes them into Lua.
write('encode', [
    { id: 'empty', text: '' },
    { id: 'ascii', text: 'plain text' },
    { id: 'two-byte', text: 'café' },
    { id: 'three-byte', text: '가나' },
    { id: 'pair', text: '😀' },
    { id: 'nul-cut', text: 'a\u0000b' },
    { id: 'lead-before-ascii', text: units(0xd800, 0x41, 0x42) },
    { id: 'trail-alone', text: units(0x61, 0xdc00, 0x62) },
    { id: 'lead-at-end', text: units(0x61, 0xd83d) },
    { id: 'lead-eats-nul', text: units(0xd800, 0x0, 0x43) },
    { id: 'two-leads', text: units(0xd800, 0xd801, 0x44) },
    { id: 'long-mixed', text: 'x'.repeat(40) + 'é😀' },
])

const rows = [
    { role: 'user', data: 'hello', chatId: 'm1' },
    { role: 'char', data: 'welcome back', chatId: 'm2' },
]
const base = (id, mode, data, triggers, extra = {}) => ({ id, mode, data, index: 1, triggers, modules: [], rows, scriptstate: {}, calls: 1, ...extra })
// Rows with fields beside role, data and chatId, and an id two rows share, for the structural edits.
const fieldRows = [
    { role: 'user', data: 'hello', chatId: 'm1', time: 5 },
    { role: 'char', data: 'welcome back', chatId: 'm2', time: 7, saying: 'speaker' },
    { role: 'user', data: 'dup a', chatId: 'd1' },
    { role: 'char', data: 'dup b', chatId: 'd1', time: 9 },
]
// Every row's data and role after the listener's writes, as the listener reads them back.
const readBack = `local parts = { value, getChatLength(id) }
            for i = 0, getChatLength(id) - 1 do parts[#parts + 1] = getChatRole(id, i) .. '=' .. getChatData(id, i) end
            return table.concat(parts, '|')`
const request = [{ role: 'system', content: 'main prompt' }, { role: 'user', content: 'hello' }]

const authorNoteLink = { id: 'p', fingerprint: '2:cb31c4b8' }
const authorNotePresets = [{ id: 'p', name: 'Synthetic preset', text: 'current note' }]
const readAuthorNote = `listenEdit('editOutput', function(id, value) return value .. '|' .. getAuthorsNote(id) end)`

const stringTag = value => ['string', value]
const rawState = state => Object.entries(state).sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
const owned = (id, body, extra = {}) => base(id, 'start', 'v', [], {
    owner: 'access', bodies: [body + `\nreturn '${id}'`], expectData: id,
    expectState: [[]], expectMessages: [rows.map(row => row.data)], ...extra,
})
const collision = `setChatVar(id, 7, 'A')
    setChatVar(id, '7', 'X')
    setChatVar(id, '7', 'B')
    assert(getChatVar(id, 7) == 'A' and getChatVar(id, '7') == 'B')`
const rawKeyCases = [
    owned('raw-key-primitive-map', `
        assert(setChatVarChanged(id, 7, 'A') == true)
        assert(setChatVarChanged(id, '7', 'X') == true)
        assert(setChatVarChanged(id, 7, 'C') == true)
        assert(getChatVar(id, 7) == 'C' and getChatVar(id, '7') == 'X')
        setChatVar(id, 0/0, 'nan')
        assert(getChatVar(id, 0/0) == 'nan' and setChatVarChanged(id, 0/0, 'nan') == nil)
        setChatVar(id, -0.0, 'zero')
        assert(getChatVar(id, 0) == 'zero' and setChatVarChanged(id, 0, 'zero') == nil)
        setChatVar(id, true, 'raw-bool') setChatVar(id, 'true', 'text-bool')
        assert(getChatVar(id, true) == 'raw-bool' and getChatVar(id, 'true') == 'text-bool')
        setChatVar(id, nil, 'raw-null') setChatVar(id, 'null', 'text-null')
        assert(getChatVar(id, nil) == 'raw-null' and getChatVar(id, 'null') == 'text-null')`, {
        scriptstate: { $7: 'B' }, expectState: [rawState({ $7: stringTag('X'), $NaN: stringTag('nan'),
            $0: stringTag('zero'), $true: stringTag('text-bool'), $null: stringTag('text-null') })],
    }),
    owned('raw-key-array-identity-and-defaults', `
        local key = {1}
        assert(setChatVarChanged(id, key, 'A') == true and getChatVar(id, key) == 'null')
        assert(setChatVarChanged(id, key, 'A') == true)
        setChatVar(id, '1', 'text') setChatVar(id, key, 'last-table')
        assert(getChatVar(id, '1') == 'text' and getChatVar(id, key) == 'null')
        assert(getChatVar(id, 7) == 'null' and getChatVar(id, '7') == 'default7' and getChatVar(id, {7}) == 'null')
        assert(getChatVar(id, 'template') == 'fallback')`, {
        defaultVariables: '7=default7', templateDefaultVariables: '7=shadowed\ntemplate=fallback',
        expectState: [rawState({ $1: stringTag('last-table') })],
    }),
    owned('raw-key-object-identity', `
        local key = {a=1}
        assert(setChatVarChanged(id, key, 'first') == true)
        assert(getChatVar(id, key) == 'null' and setChatVarChanged(id, key, 'first') == true)
        setChatVar(id, '[object Object]', 'text') setChatVar(id, {}, 'last-object')
        assert(getChatVar(id, '[object Object]') == 'text' and getChatVar(id, {}) == 'null')`, {
        expectState: [rawState({ '$[object Object]': stringTag('last-object') })],
    }),
    owned('raw-key-data-errors-preserve-after', `
        for _, key in ipairs({ {__proto__=null}, {toString=false}, {__proto__={toString=false}} }) do
            assert(not pcall(setChatVar, id, key, 'lost'))
        end
        setChatVar(id, {__proto__={1,2}, length='0x1', ['0']='x'}, 'joined')
        assert(getChatVar(id, 'x') == 'null')
        setChatVar(id, 'after', 'kept')`, {
        expectState: [rawState({ $after: stringTag('kept'), $x: stringTag('joined') })],
    }),
    ...[
        ['none', '', 'normal', 'A', rows],
        ['ignored-edit', "setChat(id, 999, 'ignored')", 'normal', 'B', rows],
        ['edit', "setChat(id, 0, 'edited')", 'normal', 'B', [{ ...rows[0], data: 'edited' }, rows[1]]],
        ['operation', 'removeChat(id, 999)', 'normal', 'B', rows],
        ['edits-without-messages', "setChat(id, 0, 'discarded')", 'without-messages', 'A', rows],
        ['operation-without-messages', 'removeChat(id, 0)', 'without-messages', 'B', rows],
        ['append-without-messages', "addChat(id, 'user', 'discarded')", 'without-messages', 'A', rows],
    ].map(([kind, open, finish, expected, messages]) => owned('raw-key-collision-' + kind, collision + '\n' + open, {
        finish, scriptstate: { $7: 'B' }, expectState: [rawState({ $7: stringTag(expected) })],
        expectMessages: [messages.map(row => row.data)],
    })),
    owned('raw-key-malformed-full-chat-opens-operation', collision + `
        setChatVar(id, 'before', 'kept')
        assert(not pcall(setFullChatMain, id, '{bad'))
        setChatVar(id, 'after', 'kept')
        assert(getChatVar(id, 7) == 'A' and getChatVar(id, '7') == 'B')`, {
        scriptstate: { $7: 'B' }, expectState: [rawState({ $7: stringTag('B'), $before: stringTag('kept'), $after: stringTag('kept') })],
    }),
    owned('raw-key-access-operation-retains-map', `
        setChatVar(id, 7, 'numeric') assert(getChatVar(id, '7') == 'B')
        removeChat(id, 999)
        setChatVar(id, '7', 'text') assert(getChatVar(id, 7) == 'numeric')`, {
        scriptstate: { $7: 'B' }, expectState: [rawState({ $7: stringTag('text') })],
    }),
    owned('raw-key-direct-operation-projects-immediately', `
        setChatVar(id, 7, 'numeric') assert(getChatVar(id, '7') == 'numeric')
        setChatVar(id, '7', 'text') assert(getChatVar(id, 7) == 'text')`, {
        owner: 'direct-operation', scriptstate: { $7: 'B' }, expectState: [rawState({ $7: stringTag('text') })],
    }),
    owned('raw-key-live-flush-resets-insertion-order', '', {
        calls: 2, flushAfter: [0], scriptstate: { $7: 'B' }, bodies: [
            `setChatVar(id, 7, 'A') setChatVar(id, '7', 'B') setChatVar(id, 7, 'C')
                assert(getChatVar(id, 7) == 'C' and getChatVar(id, '7') == 'B') return 'flush:first'`,
            `assert(getChatVar(id, 7) == 'C' and getChatVar(id, '7') == 'C')
                setChatVar(id, '7', 'E') setChatVar(id, 7, 'D')
                assert(getChatVar(id, '7') == 'E' and getChatVar(id, 7) == 'D') return 'flush:second'`,
        ], expectData: ['flush:first', 'flush:second'],
        expectState: [rawState({ $7: stringTag('C') }), rawState({ $7: stringTag('D') })],
        expectMessages: [rows.map(row => row.data), rows.map(row => row.data)],
    }),
    owned('raw-key-undefined-null-live-and-durable', `
        setChatVar(id, 'u') assert(getChatVar(id, 'u') == 'old' and setChatVarChanged(id, 'u') == nil)
        setChatVar(id, 'n', nil) assert(type(getChatVar(id, 'n')) == 'userdata' and getChatVar(id, 'n') == null)
        setChatVar(id, 'undefined', 'text') setChatVar(id)
        assert(getChatVar(id) == 'old' and getChatVar(id, 'undefined') == 'text')`, {
        persistence: 'json-roundtrip', scriptstate: { $u: 'old', $undefined: 'old' },
        expectState: [rawState({ $n: ['null'], $u: ['undefined'], $undefined: ['undefined'] })],
        expectStoredState: [rawState({ $n: ['null'] })],
    }),
    owned('raw-key-native-unicode-live-and-durable', `
        local key=string.char(237,160,128)
        local value='raw:' .. string.char(237,176,128)
        setChatVar(id,key,value) assert(setChatVarChanged(id,key,value) == nil)
        setChatVar(id,utf8.char(128512),utf8.char(128512))
        setChatVar(id,'e' .. utf8.char(769),'e' .. utf8.char(769))`, {
        persistence: 'native-json', expectState: [[
            ['$e\u0301', stringTag('e\u0301')], [units(36,0xd800), ['string', units(114,97,119,58,0xdc00)]],
            ['$😀', stringTag('😀')],
        ]], expectStoredState: [rawState({ '$e\u0301': stringTag('e\u0301'), '$😀': stringTag('😀'), '$\uFFFD': stringTag('raw:\uFFFD') })],
    }),
    owned('raw-key-locals-and-persona-cbs', `
        assert(getChatVar(id, 7) == 'local')
        setChatVar(id, {7}, 'changed-local') assert(getChatVar(id, '7') == 'changed-local')
        setChatVar(id, 9, 'numeric') assert(getPersonaDescription(id) == 'stored')
        setChatVar(id, '9', 'string') assert(getPersonaDescription(id) == 'string')
        setChatVar(id, 'completed', 'raw-key-locals-and-persona-cbs')`, {
        locals: { '7': 'local' }, defaultVariables: '7=default7', personaPrompt: '{{getvar::9}}', scriptstate: { $9: 'stored' },
        expectState: [rawState({ $9: stringTag('string'), $completed: stringTag('raw-key-locals-and-persona-cbs') })],
    }),
]

write('edit', [
    base('output-appends-index-and-writes', 'editoutput', 'reply', [`
        listenEdit('editOutput', function(id, value, meta) setChatVar(id, 'seen', value) return value .. '|' .. tostring(meta.index) end)`]),
    base('input-nil-keeps', 'editinput', 'typed', [`listenEdit('editInput', function(id, value, meta) return nil end)`], { index: -1 }),
    base('input-listener-chain', 'editinput', 'a', [`
        listenEdit('editInput', function(id, value) return value .. 'b' end)
        listenEdit('editInput', function(id, value) return value .. 'c' end)`], { index: -1 }),
    base('display-non-string-keeps-previous', 'editdisplay', 'shown', [`
        listenEdit('editDisplay', function(id, value) return value .. '!' end)
        listenEdit('editDisplay', function(id, value) return 5 end)
        listenEdit('editDisplay', function(id, value) return value .. '?' end)`], { index: 0 }),
    base('display-writes-variable-not-message', 'editdisplay', 'row', [`
        listenEdit('editDisplay', function(id, value)
            setChatVar(id, 'shown', value)
            setChat(id, 0, 'changed')
            stopChat(id)
            alertNormal(id, 'ignored')
            return value .. ':' .. getChatVar(id, 'shown')
        end)`], { index: 0 }),
    base('display-refused-async-api-is-fulfilled-nothing', 'editdisplay', 'row', [`
        listenEdit('editDisplay', function(id, value)
            local promise = getTokens(id, 'x')
            return value .. ':' .. type(promise) .. ':' .. tostring(promise:await())
        end)`], { index: 0 }),
    base('output-writes-messages', 'editoutput', 'reply', [`
        listenEdit('editOutput', function(id, value)
            setChat(id, 0, 'edited ' .. getChatData(id, 0))
            setChatRole(id, 1, 'user')
            return value .. ':' .. getChatLength(id) .. ':' .. getChatRole(id, 1)
        end)`]),
    base('output-reads-history-and-state', 'editoutput', 'reply', [`
        listenEdit('editOutput', function(id, value)
            setState(id, 'box', { count = 2 })
            local full = getFullChat(id)
            local last = getChat(id, 1)
            local recent = getRecentChats(id, 1)
            return table.concat({ value, #full, full[1].data, last.role, #recent, recent[1].data, getState(id, 'box').count }, '|')
        end)`]),
    base('process-mode-runs-no-lua', 'editprocess', 'kept', [`
        listenEdit('editOutput', function(id, value) return 'never' end)
        setChatVar('none', 'never', '1')`]),
    base('request-appends-message', 'editRequest', request, [`
        listenEdit('editRequest', function(id, value, meta)
            value[#value + 1] = { role = 'user', content = 'added:' .. tostring(meta.index) }
            return value
        end)`], { index: null }),
    base('request-edits-content-and-extra', 'editRequest', request, [`
        listenEdit('editRequest', function(id, value)
            value[1].content = value[1].content .. ' (edited)'
            value[2].name = 'speaker'
            value[2].custom = 'kept'
            return value
        end)`], { index: null }),
    base('request-returns-string', 'editRequest', request, [`listenEdit('editRequest', function(id, value) return 'flat' end)`], { index: null }),
    base('triggers-chain-in-order', 'editoutput', 'x', [
        `listenEdit('editOutput', function(id, value) return value .. '1' end)`,
        `listenEdit('editOutput', function(id, value) return value .. '2' end)`,
    ], { modules: [`listenEdit('editOutput', function(id, value) return value .. 'm' end)`] }),
    base('init-error-returns-original-and-keeps-writes', 'editoutput', 'orig', [
        `listenEdit('editOutput', function(id, value) setChatVar(id, 'first', 'ran') return value .. '1' end)`,
        `listenEdit('editOutput', function(id, value) return value .. '2' end) this is not lua`,
    ]),
    base('listener-error-keeps-data-and-continues', 'editoutput', 'orig', [
        `listenEdit('editOutput', function(id, value) setChatVar(id, 'before-error', 'kept') setChat(id, 0, 'before-error') error('boom') end)`,
        `listenEdit('editOutput', function(id, value) return value .. '2' end)`,
    ]),
    base('globals-are-retained-across-calls', 'editoutput', 'x', [`
        count = 0
        listenEdit('editOutput', function(id, value) count = count + 1 return value .. count end)`], { calls: 3 }),
    base('json-raw-isolated-surrogate', 'editoutput', 'x', [`
        callListenMain = function(mode, id, value, meta) return '"' .. string.char(237, 160, 128) .. '"' end`]),
    base('log-json-raw-isolated-surrogate', 'editoutput', 'x', [`
        listenEdit('editOutput', function(id, value) logMain('"' .. string.char(237, 160, 128) .. '"') return value .. 'logged' end)`]),
    base('raw-result-when-json-parse-fails', 'editoutput', 'x', [`
        callListenMain = function(mode, id, value, meta) return Promise.resolve('not json') end`]),
    base('direct-string-result', 'editoutput', 'x', [`
        callListenMain = function(mode, id, value, meta) return '"direct"' end`]),
    base('missing-listener-keeps', 'editoutput', 'x', [`callListenMain = nil`]),
    base('table-result-becomes-object', 'editoutput', 'x', [`
        listenEdit('editOutput', function(id, value) return { b = 1, a = { 'p', 'q' }, ['1'] = true } end)`]),
    base('false-result-is-data', 'editoutput', 'x', [`listenEdit('editOutput', function(id, value) return false end)`]),
    base('nul-in-json-result', 'editoutput', 'x', [`listenEdit('editOutput', function(id, value) return 'a\\0b' end)`]),
    base('promise-order', 'editoutput', 'x', [`
        callListenMain = function(mode, id, value, meta)
            local log = {}
            local function add(text) log[#log + 1] = text end
            local p = Promise.resolve(1)
            local a = p:next(function(v) add('a' .. v) return v + 1 end)
            local b = p:next(function(v) add('b' .. v) end)
            local c = a:next(function(v) add('c' .. v) return Promise.resolve(v * 10) end)
            local d = c:next(function(v) add('d' .. v) end)
            local e = Promise.create(function(resolve) add('e') resolve(a) end):next(function(v) add('f' .. v) end)
            local g = Promise.create(function() error('rejected') end):catch(function() add('g') end):finally(function() add('h') end)
            add('sync')
            return Promise.all({ d, e, g, b }):next(function(values) add('all' .. #values) return json.encode(table.concat(log, ',')) end)
        end`]),
    base('await-inside-listener', 'editoutput', 'x', [`
        listenEdit('editOutput', function(id, value)
            local first = Promise.resolve('w'):await()
            local ok, reason = pcall(function() return Promise.create(function(_, reject) reject('no') end):await() end)
            return value .. first .. tostring(ok) .. tostring(reason)
        end)`]),
    base('await-outside-coroutine-fails', 'editoutput', 'x', [`
        callListenMain = function(mode, id, value, meta)
            local ok = pcall(function() return Promise.resolve(1):await() end)
            return json.encode('await:' .. tostring(ok))
        end`]),
    base('wasmoon-globals', 'editdisplay', 'g', [`
        listenEdit('editDisplay', function(id, value)
            local e = Error.create('m')
            print('printed', 1, nil)
            return table.concat({ value, type(null), tostring(null), tostring(null.x == null), type(e), tostring(e), e.message,
                tostring(e.other == null), tostring(getmetatable(Promise.resolve(1))), tostring(Promise.resolve(1) == Promise.resolve(2)) }, ',')
        end)`], { index: 0 }),
    base('lowlevel-llm-is-refused', 'editoutput', 'x', [`
        listenEdit('editOutput', function(id, value) return value .. tostring(LLM(id, { { role = 'user', content = 'hi' } })) end)
        listenEdit('editOutput', function(id, value) return value .. 'after' end)`]),
    // A Lua nil reaches `JSON.parse` as null.
    base('log-parses-each-argument', 'editoutput', 'x', [`
        listenEdit('editOutput', function(id, value) logMain(nil) logMain(true) logMain(1.5) logMain('[1]') return value .. 'logged' end)`]),
    base('display-lone-surrogate-and-nul', 'editdisplay', units(0x61, 0xd800, 0x62, 0x0, 0x63), [`
        listenEdit('editDisplay', function(id, value) return value .. '!' .. #value end)`], { index: 0 }),
    base('display-invalid-utf8-long', 'editdisplay', 'long', [`
        listenEdit('editDisplay', function(id, value) return value .. string.rep('-', 20) .. string.char(255, 237, 160, 128) end)`], { index: 0 }),
    base('display-short-surrogate-bytes', 'editdisplay', 's', [`
        listenEdit('editDisplay', function(id, value) return value .. string.char(237, 160, 128) end)`], { index: 0 }),
    base('no-lua-trigger-returns-content', 'editdisplay', 'plain', [], { index: 0 }),
    // `removeChat` and `insertChat` splice at `toHistorySpliceIndex`; `setFullChatMain` assigns the parsed array, keeping
    // a stored id as often as the chat holds it and the other fields of a message whose id names one on both sides.
    base('remove-chat-and-read-back', 'editoutput', 'r', [`
        listenEdit('editOutput', function(id, value)
            removeChat(id, 0)
            removeChat(id, -1.5)
            removeChat(id, 9)
            ${readBack}
        end)`], { rows: fieldRows }),
    base('insert-chat-and-read-back', 'editoutput', 'i', [`
        listenEdit('editOutput', function(id, value)
            insertChat(id, 1, 'user', 'middle')
            insertChat(id, -1, 'narrator', nil)
            insertChat(id, 99, 'char', 'at the end')
            insertChat(id, -99, 'user', 'at the start')
            ${readBack}
        end)`], { rows: fieldRows }),
    base('set-full-chat-merges-ids', 'editoutput', 'f', [`
        listenEdit('editOutput', function(id, value)
            setFullChatMain(id, json.encode({
                { role = 'char', data = 'm2 again', chatId = 'm2' },
                { role = 'user', data = 'm1 first', chatId = 'm1' },
                { role = 'user', data = 'm1 second', chatId = 'm1' },
                { role = 'narrator', data = 'unknown id', chatId = 'zz' },
                { role = 'char', data = 'd1 kept', chatId = 'd1' },
                { role = 'char', data = 'd1 kept again', chatId = 'd1' },
                { role = 'char', data = 'd1 over', chatId = 'd1' },
                { role = 'user', data = 'no id' },
            }))
            ${readBack}
        end)`], { rows: fieldRows }),
    base('set-full-chat-keeps-a-once-held-id-with-its-fields', 'editoutput', 'k', [`
        listenEdit('editOutput', function(id, value)
            setFullChatMain(id, '[{"role":"user","data":"first","chatId":"m1"},{"role":"char","data":"second","chatId":"m2","time":1}]')
            ${readBack}
        end)`], { rows: fieldRows }),
    base('set-full-chat-then-edits', 'editoutput', 'e', [`
        listenEdit('editOutput', function(id, value)
            setFullChatMain(id, '[{"role":"char","data":"only","chatId":"m2"}]')
            insertChat(id, 0, 'user', 'first')
            setChat(id, 1, 'only edited')
            setChatRole(id, 0, 'char')
            insertChat(id, 1, 'user', 'between')
            removeChat(id, -1)
            ${readBack}
        end)`], { rows: fieldRows }),
    base('edits-after-a-removal-index-the-new-history', 'editoutput', 'n', [`
        listenEdit('editOutput', function(id, value)
            removeChat(id, 1)
            setChat(id, 1, 'was dup a')
            setChatRole(id, -1, 'user')
            ${readBack}
        end)`], { rows: fieldRows }),
    base('set-full-chat-empty', 'editoutput', 'z', [`
        listenEdit('editOutput', function(id, value)
            setFullChatMain(id, '[]')
            insertChat(id, 0, 'char', 'alone')
            ${readBack}
        end)`], { rows: fieldRows }),
    base('structural-edits-across-calls', 'editoutput', 'c', [`
        listenEdit('editOutput', function(id, value)
            insertChat(id, 0, 'user', 'call ' .. getChatLength(id))
            removeChat(id, -1)
            ${readBack}
        end)`], { rows: fieldRows, calls: 2 }),
    base('structural-edits-need-the-key', 'editoutput', 'w', [`
        listenEdit('editOutput', function(id, value)
            removeChat('other', 0)
            insertChat('other', 0, 'user', 'refused')
            setFullChatMain('other', '[]')
            ${readBack}
        end)`], { rows: fieldRows }),
    base('display-structural-edits-are-refused', 'editdisplay', 'd', [`
        listenEdit('editDisplay', function(id, value)
            removeChat(id, 0)
            insertChat(id, 0, 'user', 'refused')
            setFullChatMain(id, '[]')
            return value .. getChatLength(id)
        end)`], { rows: fieldRows, index: 0 }),
    // TS runs these; this host declines them by name.
    base('decline-os-library', 'editoutput', 'x', [`listenEdit('editOutput', function(id, value) return value .. type(os.time()) end)`],
        { decline: 'lua-std:os.time' }),
    base('decline-unported-host-function', 'editoutput', 'x', [`listenEdit('editOutput', function(id, value) return value .. tostring(getGlobalVar(id, 'k')) end)`],
        {}),
    base('decline-safe-alert', 'editoutput', 'x', [`listenEdit('editOutput', function(id, value) alertNormal(id, 'hi') return value end)`],
        { decline: 'lua-host:alertNormal' }),
    base('decline-top-level-await', 'editoutput', 'x', [`
        local ready = Promise.resolve('r'):await()
        listenEdit('editOutput', function(id, value) return value .. ready end)`], { decline: 'lua-await-top-level' }),
    base('decline-trailing-handler', 'editoutput', 'x', [`
        listenEdit('editOutput', function(id, value)
            Promise.resolve(1):next(function() end):next(function() setChatVar(id, 'late', '1') end)
            return value
        end)`], { decline: 'lua-edit-trailing-microtasks' }),
    base('decline-full-chat-object', 'editoutput', 'x', [`
        listenEdit('editOutput', function(id, value) setFullChatMain(id, '{}') return value end)`], { decline: 'lua-host-args:setFullChatMain' }),
    base('decline-full-chat-number-data', 'editoutput', 'x', [`
        listenEdit('editOutput', function(id, value) setFullChatMain(id, '[{"role":"user","data":5}]') return value end)`],
        { decline: 'lua-host-args:setFullChatMain' }),
    base('decline-insert-number-data', 'editoutput', 'x', [`
        listenEdit('editOutput', function(id, value) insertChat(id, 0, 'user', 5) return value end)`], { decline: 'lua-host-args:insertChat' }),
    // `getChatVar` returns `String(value)` for a stored value that is not a string.
    base('chat-var-reads-non-string-values', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value)
            return table.concat({ value, getChatVar(id, 'raw'), getChatVar(id, 'num'), getChatVar(id, 'flag'),
                getChatVar(id, 'nul'), getChatVar(id, 'obj'), getChatVar(id, 'missing') }, '|')
        end)`], { scriptstate: { $raw: [1, false], $num: 2.5, $flag: true, $nul: null, $obj: { a: 1 } } }),
    // Numbers format and parse with a decimal point through state, JSON and the string library.
    base('state-number-round-trip', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value)
            local before = getState(id, 'n')
            setState(id, 'n', 1.5)
            return table.concat({ value, tostring(before), tostring(getState(id, 'n')), json.encode({ 1.5, 0.1 }), json.encode({ n = 2.75 }),
                tostring(1.5), string.format('%.14g', 2.25), tostring(tonumber('3.5')), tostring(tonumber('3,5')) }, '|')
        end)`], { calls: 2 }),
    base('chat-var-plain-object-and-array-assignment', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value)
            local before = getChatVar(id, 'object')
            local object = { a = 1, nested = { flag = true }, list = { 3, 4 } }
            local first = setChatVarChanged(id, 'object', object)
            local read = getChatVar(id, 'object')
            local ok, field = pcall(function() return read.a end)
            local nestedOk, nested = pcall(function() return read.nested.flag end)
            object.a = 2
            local after = getChatVar(id, 'object')
            local afterOk, afterField = pcall(function() return after.a end)
            local again = setChatVarChanged(id, 'object', object)
            setChatVar(id, 'array', { 10, false, { a = 'nested' } })
            local array = getChatVar(id, 'array')
            local arrayOk, item = pcall(function() return array[3].a end)
            return table.concat({ value, type(before), type(before) == 'string' and before or 'structured',
                tostring(first), type(read), tostring(ok), ok and tostring(field) or 'error',
                tostring(nestedOk), nestedOk and tostring(nested) or 'error',
                tostring(afterOk), afterOk and tostring(afterField) or 'error', tostring(again),
                type(array), tostring(arrayOk), arrayOk and tostring(item) or 'error' }, '|')
        end)`], { calls: 2, expectData: [
            'v|string|null|true|table|true|1|true|true|true|1|true|table|true|nested',
            'v|string|[object Object]|true|table|true|1|true|true|true|1|true|table|true|nested',
        ] }),
    base('chat-var-acyclic-alias-assignment', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value)
            local shared = { n = 1 }
            setChatVar(id, 'alias', { a = shared, b = shared })
            local read = getChatVar(id, 'alias')
            local ok, alias = pcall(function() return read.a == read.b end)
            local mutationOk = pcall(function() read.a.n = 7 end)
            local later = getChatVar(id, 'alias')
            local laterOk, number = pcall(function() return later.b.n end)
            return table.concat({ value, type(read), tostring(ok), ok and tostring(alias) or 'error',
                tostring(mutationOk), tostring(laterOk), laterOk and tostring(number) or 'error' }, '|')
        end)`], { calls: 2, expectData: 'v|table|true|true|true|true|1' }),
    base('chat-var-nonfinite-number-assignment', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value)
            local before = getChatVar(id, 'nan')
            local beforePos = getChatVar(id, 'positive')
            local beforeNeg = getChatVar(id, 'negative')
            setChatVar(id, 'nan', 0/0)
            setChatVar(id, 'positive', math.huge)
            setChatVar(id, 'negative', -math.huge)
            setChatVar(id, 'zero', 0)
            local n = getChatVar(id, 'nan')
            local p = getChatVar(id, 'positive')
            local m = getChatVar(id, 'negative')
            local nanChanged = setChatVarChanged(id, 'nan', 0/0)
            local positiveChanged = setChatVarChanged(id, 'positive', math.huge)
            local zeroChanged = setChatVarChanged(id, 'zero', -0.0)
            return table.concat({ value, type(before), tostring(before), type(beforePos), tostring(beforePos),
                type(beforeNeg), tostring(beforeNeg), type(n), tostring(n ~= n), type(p), tostring(p == math.huge),
                type(m), tostring(m == -math.huge), tostring(nanChanged), tostring(positiveChanged), tostring(zeroChanged) }, '|')
        end)`], { calls: 2, persistence: 'json-roundtrip', expectData: 'v|string|null|string|null|string|null|number|true|number|true|number|true|true|nil|nil' }),
    base('chat-var-explicit-nil-and-null-values', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value)
            local before = getChatVar(id, 'explicit')
            setChatVar(id, 'explicit', nil)
            local read = getChatVar(id, 'explicit')
            setChatVar(id, 'nested', { null, { flag = null }, false })
            local nested = getChatVar(id, 'nested')
            local ok, equal = pcall(function() return nested[1] == null and nested[2].flag == null end)
            local changed = setChatVarChanged(id, 'explicit', null)
            return table.concat({ value, type(before), tostring(before), type(read), tostring(read),
                tostring(read == nil), tostring(read == null), type(null), tostring(ok), ok and tostring(equal) or 'error', tostring(changed) }, '|')
        end)`], { calls: 2, expectData: [
            'v|string|null|userdata|null|false|true|userdata|true|true|nil',
            'v|string|null|string|null|false|false|userdata|true|true|nil',
        ] }),
    base('last-message-getters-return-structured-data', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value)
            local character = getCharacterLastMessage(id)
            local user = getUserLastMessage(id)
            assert(type(character) == 'table' and character.nested.a == 1)
            assert(type(user) == 'table' and user[1] == 3 and user[2] == false and user[3] == null)
            character.nested.a = 9
            assert(getCharacterLastMessage(id).nested.a == 1)
            return value .. '|structured'
        end)`], { expectData: 'v|structured', rows: [
            { role: 'user', data: [3, false, null], chatId: 'u' },
            { role: 'char', data: { nested: { a: 1 } }, chatId: 'c' },
        ] }),
    base('global-getter-returns-structured-values-and-coerces-data-keys', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value)
            local object = getGlobalVar('wrong', 'object')
            local array = getGlobalVar(id, 'array')
            assert(object.a == 1 and array[1] == 2 and array[2] == false and array[3] == null)
            object.a = 9
            assert(getGlobalVar(id, 'object').a == 1)
            return table.concat({ value, getGlobalVar(id, 7), getGlobalVar(id, true), getGlobalVar(id, nil),
                getGlobalVar(id), getGlobalVar(id, {}), getGlobalVar(id, { 1, 2 }) }, '|')
        end)`], { expectData: 'v|number key|boolean key|null key|missing key|object key|array key', globals: { object: { a: 1 }, array: [2, false, null],
            '7': 'number key', true: 'boolean key', null: 'null key', undefined: 'missing key',
            '[object Object]': 'object key', '1,2': 'array key' } }),
    base('data-keys-preserve-prototypes-nullish-errors-and-number-format', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value)
            assert(getGlobalVar(id, {}) == 'object' and getGlobalVar(id, { 1, 2 }) == 'array')
            local inherited = { __proto__ = { 1, 2 }, length = 1, ['0'] = 'x' }
            assert(getGlobalVar(id, inherited) == 'generic join')
            assert(getGlobalVar(id, { __proto__ = { 1, 2 }, length = '0x1', ['0'] = 'x' }) == 'generic join')
            assert(getGlobalVar(id, { __proto__ = { 1, 2 }, length = null }) == 'empty')
            assert(getGlobalVar(id, { __proto__ = { 1, 2 }, length = 1, ['0'] = null }) == 'empty')
            assert(getGlobalVar(id, { __proto__ = { 1, 2 }, join = 0 }) == 'object')
            assert(getGlobalVar(id, 1e21) == 'large' and getGlobalVar(id, 1e-7) == 'small')
            assert(getGlobalVar(id, true) == 'boolean' and getGlobalVar(id, nil) == 'null value')
            for _, key in ipairs({ { __proto__ = null }, { toString = false }, { __proto__ = { toString = false } } }) do
                assert(not pcall(getGlobalVar, id, key))
            end
            setChatVar(id, 'after', 'kept')
            return value .. '|keys'
        end)`], { expectData: 'v|keys', globals: { '[object Object]': 'object', '1,2': 'array', x: 'generic join', '': 'empty',
            '1e+21': 'large', '1e-7': 'small', true: 'boolean', null: 'null value' } }),
    base('author-note-linked-preset-preserves-raw-copy-after-reload', 'editoutput', 'v', [readAuthorNote], {
        calls: 2, persistence: 'json-roundtrip', expectData: 'v|current note',
        authorNote: { note: '😀', link: authorNoteLink, presets: authorNotePresets } }),
    base('author-note-deleted-preset-reads-empty', 'editoutput', 'v', [readAuthorNote], {
        expectData: 'v|', authorNote: { note: '😀', link: authorNoteLink, presets: [] } }),
    base('author-note-malformed-preset-is-not-a-match', 'editoutput', 'v', [readAuthorNote], {
        expectData: 'v|', authorNote: { note: '😀', link: authorNoteLink, presets: [{ id: 'p', text: 'invalid preset' }] } }),
    base('author-note-malformed-preset-list-is-catchable', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value)
            setChatVar(id, 'before', 'kept')
            local ok = pcall(getAuthorsNote, id)
            setChatVar(id, 'after', 'kept')
            return value .. '|' .. tostring(ok) .. '|' .. getChatVar(id, 'before') .. '|' .. getChatVar(id, 'after')
        end)`], { expectData: 'v|false|kept|kept', authorNote: { note: '😀', link: authorNoteLink, presets: {} } }),
    base('author-note-detached-copy-reads-raw-note', 'editoutput', 'v', [readAuthorNote], {
        expectData: 'v|manual note', authorNote: { note: 'manual note', link: authorNoteLink, presets: authorNotePresets } }),
    base('author-note-malformed-link-reads-raw-note', 'editoutput', 'v', [readAuthorNote], {
        expectData: 'v|raw note', authorNote: { note: 'raw note', link: { id: 7, fingerprint: 'invalid' }, presets: authorNotePresets } }),
    base('global-getter-preserves-unicode-key-equality', 'editoutput', 'v', [`
        listenEdit('editOutput', function(id, value) return value .. '|' .. getGlobalVar(id, '한글😀') end)`], {
        expectData: 'v|unicode value', globals: { '한글😀': 'unicode value' } }),
    base('author-note-missing-copy-reads-empty', 'editoutput', 'v', [readAuthorNote], {
        expectData: 'v|', authorNote: {} }),
    ...rawKeyCases,

])
