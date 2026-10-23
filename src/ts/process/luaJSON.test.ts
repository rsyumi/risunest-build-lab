// @vitest-environment node

import { readFile } from 'node:fs/promises'
import { afterAll, beforeAll, expect, test } from 'vitest'
import { LuaFactory, type LuaEngine } from 'wasmoon'

const differential = String.raw`
local baseline = require 'json_baseline'
local candidate = require 'json'

local function dump(value, out)
  local kind = type(value)
  if kind == 'table' then
    local keys = {}
    for key in pairs(value) do keys[#keys + 1] = key end
    table.sort(keys, function(a, b)
      if type(a) ~= type(b) then return type(a) < type(b) end
      return a < b
    end)
    out[#out + 1] = '{'
    for _, key in ipairs(keys) do
      dump(key, out)
      out[#out + 1] = '='
      dump(value[key], out)
      out[#out + 1] = ','
    end
    out[#out + 1] = '}'
  elseif kind == 'number' then
    out[#out + 1] = math.type(value) .. ' ' .. string.format('%.17g', value)
  elseif kind == 'string' then
    out[#out + 1] = string.format('%q', value)
  else
    out[#out + 1] = tostring(value)
  end
end

local function outcome(decoder, text)
  local ok, value = pcall(decoder.decode, text)
  if not ok then
    -- The chunk name and line differ between the two files; the message must not.
    local message = tostring(value):gsub('^[^:]+:%d+: ', '')
    return 'error ' .. message
  end
  local out = {}
  dump(value, out)
  return 'value ' .. table.concat(out)
end

local function compare(text)
  local expected, actual = outcome(baseline, text), outcome(candidate, text)
  if expected == actual then return nil, expected end
  return string.format('%q\n  baseline  %s\n  candidate %s', text, expected, actual), expected
end

function compareText(text)
  return compare(text) or ''
end

local seed = 0
local function random(n)
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed % n + 1
end

local pieces = {
  'a', 'word', ' ', '\\"', '\\\\', '\\/', '\\b', '\\f', '\\n', '\\r', '\\t',
  '\\u0041', '\\u00e9', '\\u00E9', '\\uac00', '\\ud83d\\ude00', '\\uD83D\\uDE00', '\\ud800',
  '\\udc00', '\\ud83d\\u0041', '\\u12', '\\uZZZZ', '\\x', '\\', '\\u0000',
  '가', '😀', 'é', '\0', '\1', '\31', '\127', '\128', '\255', '\t', '\n',
  '"', '{', '}', '[', ']', ',', ':',
}
local scalars = {
  '0', '-1', '1.5', '1e3', '-0', '12345678901234567890', '0x10', '1.', '.5',
  'true', 'false', 'null', 'nul', 'tru',
}
local mutations = { '"', '\\', '{', '}', '[', ']', ',', ':', ' ', 'u', '0', '\0', '\31', '\200' }

local function makeString()
  local parts = {}
  for _ = 1, random(8) - 1 do parts[#parts + 1] = pieces[random(#pieces)] end
  return '"' .. table.concat(parts) .. '"'
end

local function makeValue(depth)
  local kind = random(depth > 2 and 3 or 5)
  if kind == 1 then return makeString() end
  if kind == 2 then return scalars[random(#scalars)] end
  if kind == 3 then return ' ' .. makeString() .. '\n' end
  local items = {}
  for _ = 1, random(4) - 1 do
    items[#items + 1] = kind == 4 and makeValue(depth + 1) or (makeString() .. ':' .. makeValue(depth + 1))
  end
  return kind == 4 and ('[' .. table.concat(items, ',') .. ']') or ('{' .. table.concat(items, ',') .. '}')
end

local function mutate(text)
  local position = random(#text + 1)
  local action = random(3)
  if action == 1 then return text:sub(1, position - 1) .. text:sub(position + 1) end
  local inserted = mutations[random(#mutations)]
  if action == 2 then return text:sub(1, position - 1) .. inserted .. text:sub(position) end
  return text:sub(1, position - 1) .. inserted .. text:sub(position + 1)
end

function fuzz(count, startSeed)
  seed = startSeed
  local differences, decoded, rejected = {}, 0, 0
  for _ = 1, count do
    local text = makeValue(0)
    if random(2) == 1 then text = mutate(text) end
    local difference, expected = compare(text)
    if expected:sub(1, 5) == 'value' then decoded = decoded + 1 else rejected = rejected + 1 end
    if difference and #differences < 5 then differences[#differences + 1] = difference end
  end
  return string.format('%d %d\n%s', decoded, rejected, table.concat(differences, '\n'))
end
`


let engine: LuaEngine
let decode: (text: string) => string
beforeAll(async () => {
    const factory = new LuaFactory()
    await factory.mountFile('json.lua', await readFile('public/lua/json.lua', 'utf8'))
    await factory.mountFile('json_baseline.lua', await readFile('tests/fixtures/lua/json.baseline.lua', 'utf8'))
    engine = await factory.createEngine()
    await engine.doString(differential)
    await engine.doString(`json=require('json')
        function decode(text) return json.encode(json.decode(text)) end
        function badControl(byte) return json.decode('"abc'..string.char(byte)..'"') end`)
    const roundtrip = engine.global.get('decode')
    decode = text => JSON.parse(roundtrip(text))
})
afterAll(() => { engine?.global.close() })

test('decodes plain spans, every escaped control byte, quotes, slashes and Unicode exactly', () => {
    const values = ['', 'plain '.repeat(20_000), '\"\\\n\t\u0001'.repeat(20_000), '\\"/\b\f\n\r\t', '한글 日本語 😀 é',
        Array.from({ length: 32 }, (_, i) => String.fromCharCode(i)).join('')]
    for (let i = 0; i < 256; i++) values.push(`prefix${String.fromCharCode(i)}suffix`)
    for (const value of values) expect(decode(JSON.stringify(value))).toBe(value)
    expect(decode('"\\u0061\\ud55c\\ud83d\\ude00"')).toBe('a한😀')
})

test('reports the original bad byte and line/column after skipped spans', () => {
    for (let i = 0; i < 32; i++) {
        expect(() => engine.global.get('badControl')(i))
            .toThrow('control character in string at line 1 col 5')
    }
    for (const value of ['"abc', '"plain ']) {
        expect(() => decode(value)).toThrow('expected closing quote for string at line 1 col 1')
    }
    expect(() => decode('"abc\\q"')).toThrow("invalid escape char 'q' in string at line 1 col 5")
    expect(() => decode('"abc\\u12"')).toThrow('invalid unicode escape in string at line 1 col 5')
    expect(() => decode('[\n "abc\x01"]')).toThrow('control character in string at line 2 col 6')
    expect(() => decode('"done" true')).toThrow('trailing garbage at line 1 col 8')
})

test.each([1, 7, 1234567])('decodes generated and mutated JSON from seed %i exactly like the previous decoder', (seed) => {
    const [counts, ...differences] = (engine.global.get('fuzz')(4_000, seed) as string).split('\n')
    const [decoded, rejected] = counts.split(' ').map(Number)
    expect(differences.join('\n')).toBe('')
    // Both the decoded and the rejected paths are compared, not just one of them.
    expect(decoded).toBeGreaterThan(400)
    expect(rejected).toBeGreaterThan(400)
})
