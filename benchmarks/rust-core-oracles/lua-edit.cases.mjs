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
        { decline: 'lua-host:getGlobalVar' }),
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
])
