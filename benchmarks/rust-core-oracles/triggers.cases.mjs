// Writes tests/fixtures/rust-core/triggers/run.cases.json and route-declines.cases.json, the synthetic inputs of
// triggers.oracle.test.ts.
// Usage: node benchmarks/rust-core-oracles/triggers.cases.mjs, then regenerate the expected files with the oracle.
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const base = {
    root: {
        username: 'Tester', templateDefaultVariables: 'tmpl=fromTemplate\nboth=template', modules: [], enabledModules: [],
        moduleIntergration: '', personas: [], personaPrompt: '', selectedPersona: 0, aiModel: 'custom',
        currentPluginProvider: 'trigger-oracle-tokenizer', useTokenizerCaching: false, customTokenizer: '',
    },
    character: {
        chaId: 'char-a', name: 'Aria', type: 'character', chatPage: 0, triggerscript: [], customscript: [], globalLore: [],
        defaultVariables: 'dflt=fromChar\nboth=char', firstMessage: 'Hello there', alternateGreetings: ['Alt zero', 'Alt one'],
        desc: 'A test character', lowLevelAccess: false,
    },
    chat: {
        id: 'chat-a', name: 'Chat', note: 'base note', localLore: [], fmIndex: -1,
        message: [
            { role: 'user', data: 'hello world', chatId: 'm0' },
            { role: 'char', data: 'Hi! How are you?', chatId: 'm1' },
            { role: 'user', data: 'I am Fine, thanks', chatId: 'm2' },
            { role: 'char', data: 'Great to hear', chatId: 'm3' },
        ],
        scriptstate: { $hp: '10', $name: 'Bob', $arr: '["a","b","c"]', $dict: '{"k":"v","2":"two"}', $flag: 'true', $num: '1.0' },
    },
}

const cases = []
function add(id, spec) {
    const entry = { id, mode: spec.mode ?? 'start' }
    for (const key of ['manualName', 'triggerId', 'previousTriggerId', 'root', 'chat', 'random', 'native', 'admitted', 'earlier', 'earlierEdits', 'llm']) {
        if (spec[key] !== undefined) entry[key] = spec[key]
    }
    // Case variables add to the base ones, so the shallow chat override keeps them; any other value replaces them.
    const state = spec.chat?.scriptstate
    if (state && typeof state === 'object' && !Array.isArray(state)) entry.chat = { ...spec.chat, scriptstate: { ...base.chat.scriptstate, ...state } }
    const character = { ...(spec.character ?? {}) }
    if (spec.triggers) character.triggerscript = spec.triggers
    if (Object.keys(character).length) entry.character = character
    cases.push(entry)
}

// A trigger, a v2 effect, and the effects used most.
const T = (comment, type, effect, extra = {}) => ({ comment, type, conditions: [], effect, ...extra })
const S = (effect, extra) => T('', 'start', effect, extra)
const e = (type, fields = {}, indent = 0) => ({ type, ...fields, indent })
const set = (name, value, operator = '=', valueType = 'value', indent = 0) =>
    e('v2SetVar', { operator, var: name, valueType, value }, indent)
const local = (name, value, valueType = 'value', indent = 0) => e('v2DeclareLocalVar', { var: name, value, valueType }, indent)
const iff = (source, condition, target, targetType = 'value', indent = 0) =>
    e('v2If', { condition, targetType, target, source }, indent)
const adv = (sourceType, source, condition, targetType, target, indent = 0) =>
    e('v2IfAdvanced', { condition, sourceType, source, targetType, target }, indent)
const end = (indent, endOfLoop = false) => e('v2EndIndent', endOfLoop ? { endOfLoop: true } : {}, indent)
const els = (indent = 0) => e('v2Else', {}, indent)
const run = (target, indent = 0) => e('v2RunTrigger', { target }, indent)
const out = (type, fields = {}, indent = 0) => e(type, { outputVar: 'out', ...fields }, indent)
const cond = (type, fields) => ({ type, ...fields })
const msg = (role, data, chatId) => ({ role, data, chatId })
const marks = ['start', 'output', 'input', 'manual'].map(type => T(type === 'manual' ? 'm' : type, type, [set(`ran_${type}`, '1')]))

// Which triggers run.
add('select-start', { triggers: marks })
add('select-output', { mode: 'output', triggers: marks })
add('select-input', { mode: 'input', triggers: marks })
add('select-manual-by-name', { mode: 'manual', manualName: 'go', triggers: [...marks, T('go', 'start', [set('ran_go', '1')])] })
add('select-manual-unknown-name', { mode: 'manual', manualName: 'nothing', triggers: marks })
add('select-triggercode-any-mode', { triggers: [T('code', 'output', [{ type: 'triggercode', code: 'x' }, set('code', 'ran')])] })
add('select-no-triggers', { triggers: [] })
add('select-none-match', { mode: 'output', triggers: [S([set('a', '1')])] })
add('select-unknown-effect-ignored', { triggers: [S([{ type: 'futureEffect', value: 'x' }, set('after', 'ok')])] })
const module = (id, trigger, extra = {}) => ({ id, name: id, description: '', trigger, ...extra })
add('select-character-then-module', {
    triggers: [S([set('seq', 'c')])],
    root: { modules: [module('m1', [S([e('v2ConcatString', { source1: 'seq', source1Type: 'var', source2: 'm', source2Type: 'value', outputVar: 'seq' })])])], enabledModules: ['m1'] },
})
add('select-module-not-enabled', { triggers: [S([set('seq', 'c')])], root: { modules: [module('m1', [S([set('seq', 'm')])])], enabledModules: [] } })
// The selected preset owns `moduleIntergration` and `templateDefaultVariables`; the stored root mirrors of both are
// stale. TS derives the effective values from the preset when it loads, and the run selects and reads those.
const appendSeq = (suffix) => e('v2ConcatString', { source1: 'seq', source1Type: 'var', source2: suffix, source2Type: 'value', outputVar: 'seq' })
add('select-module-from-the-effective-preset', {
    triggers: [S([set('seq', 'c'), set('template', 'tmpl', '=', 'var')])],
    root: {
        modules: [module('mirror-mod', [S([appendSeq('-mirror')])]), module('preset-mod', [S([appendSeq('-preset')])])],
        moduleIntergration: 'mirror-mod', templateDefaultVariables: 'tmpl=fromMirror',
        botPresets: [{ id: 'preset-a', name: 'Preset A', aiModel: 'custom', currentPluginProvider: 'trigger-oracle-tokenizer',
            moduleIntergration: 'preset-mod', templateDefaultVariables: 'tmpl=fromPreset' }],
        botPresetsId: 0,
    },
})
add('select-module-low-level', {
    triggers: [S([e('v2SendAIprompt')])],
    root: { modules: [module('m1', [S([e('v2SendAIprompt')])], { lowLevelAccess: true })], enabledModules: ['m1'] },
})
add('select-character-low-level', { character: { lowLevelAccess: true }, triggers: [S([e('v2SendAIprompt')])] })
add('select-send-ai-without-low-level', { triggers: [S([e('v2SendAIprompt'), set('after', '1')])] })
add('select-host-effects-without-low-level', {
    triggers: [S([
        e('v2RunLLM', { value: 'q', valueType: 'value', model: 'model', outputVar: 'llm' }),
        e('v2ImgGen', { value: 'q', valueType: 'value', negValue: '', negValueType: 'value', outputVar: 'img' }),
        e('v2CheckSimilarity', { source: 'a', sourceType: 'value', value: 'a§b', valueType: 'value', outputVar: 'sim' }),
        set('after', '1'),
    ])],
})

// Conditions.
const guarded = (conditions, name = 'pass') => [{ ...S([set(name, 'yes')]), conditions }]
const varCond = (variable, operator, value) => cond('var', { var: variable, operator, value })
add('cond-var-equal-pass', { triggers: guarded([varCond('hp', '=', '10')]) })
add('cond-var-equal-fail', { triggers: guarded([varCond('hp', '=', '11')]) })
add('cond-var-not-equal', { triggers: guarded([varCond('hp', '!=', '11')]) })
add('cond-var-greater', { triggers: guarded([varCond('hp', '>', '9')]) })
add('cond-var-greater-fail', { triggers: guarded([varCond('hp', '>', '10')]) })
add('cond-var-less', { triggers: guarded([varCond('hp', '<', '10.5')]) })
add('cond-var-at-least', { triggers: guarded([varCond('hp', '>=', '10')]) })
add('cond-var-at-most-fail', { triggers: guarded([varCond('hp', '<=', '9')]) })
add('cond-var-nan-compare', { triggers: guarded([varCond('name', '>', '1')]) })
add('cond-var-nan-not-less', { triggers: guarded([varCond('name', '<=', '1')]) })
add('cond-var-null-missing', { triggers: guarded([varCond('missing', 'null', '')]) })
add('cond-var-null-present', { triggers: guarded([varCond('hp', 'null', '')]) })
add('cond-var-true', { triggers: guarded([varCond('flag', 'true', '')]) })
add('cond-var-true-one', { chat: { scriptstate: { $flag: '1' } }, triggers: guarded([varCond('flag', 'true', '')]) })
add('cond-var-true-other', { chat: { scriptstate: { $flag: 'yes' } }, triggers: guarded([varCond('flag', 'true', '')]) })
add('cond-var-character-default', { triggers: guarded([varCond('dflt', '=', 'fromChar'), varCond('both', '=', 'char')]) })
add('cond-var-template-default', { triggers: guarded([varCond('tmpl', '=', 'fromTemplate')]) })
add('cond-var-stored-number', { chat: { scriptstate: { $count: 7 } }, triggers: guarded([varCond('count', '=', '7')]) })
add('cond-var-stored-null-uses-default', { chat: { scriptstate: { $dflt: null } }, triggers: guarded([varCond('dflt', '=', 'fromChar')]) })
add('cond-value-literal', { triggers: guarded([cond('value', { var: 'abc', operator: '=', value: 'abc' })]) })
add('cond-chatindex', { triggers: guarded([cond('chatindex', { operator: '=', value: '4' }), cond('chatindex', { operator: '>', value: '2' })]) })
add('cond-chatindex-fail', { triggers: guarded([cond('chatindex', { operator: '<', value: '4' })]) })
add('cond-cbs-value', { chat: { scriptstate: { $alias: 'Bob', $name: 'Bob' } }, triggers: guarded([varCond('alias', '=', '{{getvar::name}}')]) })
add('cond-cbs-in-stored-value', { chat: { scriptstate: { $tpl: '{{getvar::name}}', $name: 'Bob' } }, triggers: guarded([varCond('tpl', '=', 'Bob')]) })
add('cond-unknown-operator', { triggers: guarded([varCond('hp', '~', '1')]) })
add('cond-unknown-type', { triggers: guarded([cond('future', { value: 'x' })]) })
add('cond-second-fails', { triggers: guarded([varCond('hp', '=', '10'), varCond('name', '=', 'Al')]) })
const exists = (value, type2, depth) => cond('exists', { value, type2, depth })
add('cond-exists-strict', { triggers: guarded([exists('Great', 'strict', 1)]) })
add('cond-exists-strict-word-only', { triggers: guarded([exists('Gre', 'strict', 1)]) })
add('cond-exists-loose', { triggers: guarded([exists('FINE', 'loose', 2)]) })
add('cond-exists-loose-outside-depth', { triggers: guarded([exists('FINE', 'loose', 1)]) })
add('cond-exists-depth-zero-all', { triggers: guarded([exists('hello', 'loose', 0)]) })
add('cond-exists-regex', { triggers: guarded([exists('H.*you\\?', 'regex', 3)]) })
add('cond-exists-regex-miss', { triggers: guarded([exists('^nothing', 'regex', 4)]) })
add('cond-exists-regex-invalid', { triggers: [S([set('before', '1')]), ...guarded([exists('(', 'regex', 4)])] })
add('cond-exists-unknown-kind', { triggers: guarded([exists('zzz', 'fuzzy', 4)]) })

// Variable writes and arithmetic.
add('setvar-assign', { triggers: [S([set('a', 'x'), set('b', 'hp', '=', 'var'), set('c', '{{getvar::name}}!')])] })
add('setvar-arithmetic', {
    triggers: [S([
        set('hp', '5', '+='), set('m', '3', '-='), set('t', '4', '*='), set('hp', '3', '/='), set('r', '10', '='), set('r', '4', '%='),
        set('nan', 'x', '+='), set('name', '1', '+='), set('inf', '0', '='), set('inf', '0', '/='), set('neg', '-0', '='),
        set('neg', '-1', '*='), set('float', '0.1', '='), set('float', '0.2', '+='), set('big', '1e21', '+='),
    ])],
})
add('setvar-unknown-operator', { triggers: [S([set('hp', '1', '^=')])] })
add('setvar-same-value-not-written', { triggers: [S([set('hp', '10'), set('name', 'Bob'), set('fresh', 'null')])] })
add('setvar-key-from-cbs', { triggers: [S([set('{{getvar::name}}', 'named')])] })
add('setvar-default-not-stored', { triggers: [S([set('dflt', 'fromChar'), set('copy', 'dflt', '=', 'var')])] })
add('setvar-rewrite-back', { triggers: [S([set('hp', '1'), set('hp', '10'), set('read', 'hp', '=', 'var')])] })
add('setvar-pending-read-in-cbs', { triggers: [S([set('a', 'first'), set('b', '{{getvar::a}}-second')])] })

// Local variables and scopes.
add('local-read', { triggers: [S([local('loc', 'x'), set('out', 'loc', '=', 'var')])] })
add('local-shadows-chat', { triggers: [S([local('hp', 'local'), set('out', 'hp', '=', 'var'), set('hp', '5'), set('out2', 'hp', '=', 'var')])] })
add('local-in-cbs', { triggers: [S([local('loc', 'inner'), set('out', '[{{getvar::loc}}]')])] })
add('local-from-var', { triggers: [S([local('loc', 'name', 'var'), set('out', 'loc', '=', 'var')])] })
add('local-cleared-at-end', {
    triggers: [S([iff('hp', '=', '10'), local('loc', 'block', 'value', 1), set('in', 'loc', '=', 'var', 1), end(1), set('after', 'loc', '=', 'var')])],
})
add('local-across-triggers', { triggers: [S([local('loc', 'kept')]), S([set('out', 'loc', '=', 'var')])] })
add('local-declare-assigns-lower', {
    triggers: [S([local('loc', 'a'), iff('hp', '=', '10'), local('loc', 'b', 'value', 1), end(1), set('out', 'loc', '=', 'var')])],
})
add('local-set-arithmetic', { triggers: [S([local('n', '2'), set('n', '3', '+='), set('out', 'n', '=', 'var')])] })
add('local-isolated-in-runtrigger', {
    triggers: [S([local('loc', 'outer'), run('sub'), set('back', 'loc', '=', 'var')]),
        T('sub', 'manual', [set('seen', 'loc', '=', 'var'), local('loc2', 'inner')]), S([set('late', 'loc2', '=', 'var')])],
})

// Branches.
add('if-numeric-equal', { triggers: [S([iff('num', '=', '1'), set('eq', 'yes', '=', 'value', 1), end(1)])] })
add('if-string-equal', { triggers: [S([iff('name', '=', 'Bob'), set('eq', 'yes', '=', 'value', 1), end(1)])] })
add('if-not-equal', { triggers: [S([iff('num', '!=', '1'), set('a', 'yes', '=', 'value', 1), end(1), iff('name', '!=', 'Al'), set('b', 'yes', '=', 'value', 1), end(1)])] })
add('if-comparisons', {
    triggers: [S([
        iff('hp', '>', '9'), set('gt', '1', '=', 'value', 1), end(1), iff('hp', '<', '9'), set('lt', '1', '=', 'value', 1), end(1),
        iff('hp', '>=', '10'), set('ge', '1', '=', 'value', 1), end(1), iff('hp', '<=', '9.99'), set('le', '1', '=', 'value', 1), end(1),
    ])],
})
add('if-target-var', { triggers: [S([iff('hp', '=', 'hp', 'var'), set('same', '1', '=', 'value', 1), end(1)])] })
add('if-else-true', { triggers: [S([iff('hp', '=', '10'), set('branch', 'then', '=', 'value', 1), end(1), els(), set('branch', 'else', '=', 'value', 1), end(1), set('after', '1')])] })
add('if-else-false', { triggers: [S([iff('hp', '=', '0'), set('branch', 'then', '=', 'value', 1), end(1), els(), set('branch', 'else', '=', 'value', 1), end(1), set('after', '1')])] })
add('if-nested', {
    triggers: [S([iff('hp', '=', '10'), iff('name', '=', 'Bob', 'value', 1), set('deep', '1', '=', 'value', 2), end(2),
        iff('name', '=', 'Al', 'value', 1), set('miss', '1', '=', 'value', 2), end(2), set('mid', '1', '=', 'value', 1), end(1)])],
})
add('if-no-matching-end', { triggers: [S([iff('hp', '=', '0'), set('skipped', '1', '=', 'value', 1), set('also', '1')]), S([set('next', '1')])] })
const advCase = (id, source, condition, target, sourceType = 'value', targetType = 'value') =>
    add(id, { triggers: [S([adv(sourceType, source, condition, targetType, target), set('pass', '1', '=', 'value', 1), end(1)])] })
advCase('ifadv-source-var', 'hp', '=', '10', 'var')
advCase('ifadv-in', 'b', '∈', '["a","b"]')
advCase('ifadv-in-miss', 'z', '∈', '["a","b"]')
advCase('ifadv-in-invalid', 'b', '∈', 'not json')
advCase('ifadv-contains', '["a","b"]', '∋', 'a')
advCase('ifadv-contains-string-json', '"abc"', '∋', 'b')
advCase('ifadv-not-in', 'z', '∉', '["a"]')
advCase('ifadv-not-in-invalid', 'z', '∉', '{')
advCase('ifadv-not-contains', '["a"]', '∌', 'a')
advCase('ifadv-not-contains-invalid', '[', '∌', 'a')
advCase('ifadv-approx-number', '1.00001', '≒', '1')
advCase('ifadv-approx-number-far', '1.001', '≒', '1')
advCase('ifadv-approx-text', 'Hello World', '≒', 'helloworld')
advCase('ifadv-equiv-true', '1', '≡', 'true')
advCase('ifadv-equiv-false', 'no', '≡', 'false')
advCase('ifadv-equiv-text', 'x', '≡', 'x')
advCase('ifadv-unknown-condition', 'x', '?', 'x')
advCase('ifadv-in-number-array', '1', '∈', '[1,2]')

// Loops.
add('loop-break', {
    triggers: [S([e('v2Loop'), set('n', '1', '+=', 'value', 1), iff('n', '>=', '3', 'value', 1), e('v2BreakLoop', {}, 2), end(2), end(1, true), set('after', 'n', '=', 'var')])],
})
add('loopn-three', { triggers: [S([e('v2LoopNTimes', { value: '3', valueType: 'value' }), set('n', '1', '+=', 'value', 1), end(1, true)])] })
add('loopn-var-count', { chat: { scriptstate: { $times: '4' } }, triggers: [S([e('v2LoopNTimes', { value: 'times', valueType: 'var' }), set('n', '2', '+=', 'value', 1), end(1, true)])] })
add('loopn-zero-runs-once', { triggers: [S([e('v2LoopNTimes', { value: '0', valueType: 'value' }), set('n', '1', '+=', 'value', 1), end(1, true)])] })
add('loopn-nan-runs-once', { triggers: [S([e('v2LoopNTimes', { value: 'many', valueType: 'value' }), set('n', '1', '+=', 'value', 1), end(1, true)])] })
add('loopn-nested-counter', {
    triggers: [S([e('v2LoopNTimes', { value: '3', valueType: 'value' }), e('v2LoopNTimes', { value: '2', valueType: 'value' }, 1),
        set('inner', '1', '+=', 'value', 2), end(2, true), set('outer', '1', '+=', 'value', 1), end(1, true)])],
})
add('loop-local-cleared-each-pass', {
    triggers: [S([e('v2LoopNTimes', { value: '2', valueType: 'value' }), set('seen', 'loc', '+=', 'var', 1), local('loc', '5', 'value', 1), end(1, true)])],
})
add('loop-end-without-loop-restarts', {
    triggers: [S([set('n', '1', '+='), iff('n', '>=', '3'), e('v2StopTrigger', {}, 1), end(1), end(1, true)])],
})
add('break-to-first-loop-end', {
    triggers: [S([e('v2BreakLoop'), set('skipped', '1'), end(2, true), set('after', '1')])],
})
add('stop-trigger-continues-next', { triggers: [S([set('a', '1'), e('v2StopTrigger'), set('b', '1')]), S([set('c', '1')])] })
add('effect-without-indent', { triggers: [S([iff('hp', '=', '0'), { type: 'v2SetVar', operator: '=', var: 'x', valueType: 'value', value: '1' }, end(1), set('y', '1')])] })

// Nested triggers.
add('runtrigger-nested', { triggers: [S([set('a', '1'), run('sub'), set('c', 'b', '=', 'var')]), T('sub', 'manual', [set('b', 'from-sub')])] })
add('runtrigger-recursion-limit', { triggers: [S([run('loop')]), T('loop', 'manual', [set('n', '1', '+='), run('loop')])] })
add('runtrigger-breadth-limit', { triggers: [S(Array.from({ length: 12 }, () => run('inc'))), T('inc', 'manual', [set('n', '1', '+=')])] })
add('runtrigger-low-level-unbounded', {
    character: { lowLevelAccess: true },
    triggers: [S([run('loop')]), { ...T('loop', 'manual', [set('n', '1', '+='), run('loop')]), conditions: [varCond('n', '<', '14')] }],
})
add('runtrigger-system-prompt-and-tokens', {
    triggers: [S([e('v2SystemPrompt', { location: 'start', value: 'outer start', valueType: 'value' }), run('sub'),
        e('v2SystemPrompt', { location: 'promptend', value: 'outer end', valueType: 'value' })]),
    T('sub', 'manual', [e('v2SystemPrompt', { location: 'historyend', value: 'inner history', valueType: 'value' })])],
})
add('runtrigger-stop-propagates', { triggers: [S([run('sub')]), T('sub', 'manual', [e('v2StopPromptSending')])] })
add('runtrigger-send-ai-not-propagated', { character: { lowLevelAccess: true }, triggers: [S([run('sub')]), T('sub', 'manual', [e('v2SendAIprompt')])] })
add('runtrigger-trigger-id-cleared', {
    mode: 'manual', manualName: 'click', triggerId: 'btn-1',
    triggers: [T('click', 'manual', [set('before', '{{trigger_id}}'), run('sub'), set('after', '{{trigger_id}}')]), T('sub', 'manual', [set('inside', '{{trigger_id}}')])],
})
add('runtrigger-missing-target', { triggers: [S([run('nobody'), set('after', '1')])] })
add('runtrigger-sees-messages', { triggers: [S([e('v2Impersonate', { role: 'user', value: 'added', valueType: 'value' }), run('sub')]), T('sub', 'manual', [out('v2GetLastMessage')])] })
add('runtrigger-lua-shaped-targets-run', { triggers: [S([run('sub')]), T('sub', 'manual', [set('x', '1')]), T('code', 'output', [{ type: 'triggercode', code: '' }, set('code', '1', '+=')])] })

// Trigger ids.
add('triggerid-manual-keeps', { mode: 'manual', manualName: 'click', triggerId: 'btn-2', triggers: [T('click', 'manual', [set('id', '{{trigger_id}}')])] })
add('triggerid-start-restores', { previousTriggerId: 'prev', triggers: [S([set('id', '{{trigger_id}}')])] })
add('triggerid-manual-empty', { mode: 'manual', manualName: 'click', triggerId: '', previousTriggerId: 'prev', triggers: [T('click', 'manual', [set('id', '{{trigger_id}}')])] })
add('triggerid-no-triggers-untouched', { previousTriggerId: 'prev', triggers: [] })

// System prompts.
add('system-prompt-locations', {
    chat: { scriptstate: { $sp: 'from var' } },
    triggers: [S([e('v2SystemPrompt', { location: 'start', value: 'Begin {{getvar::name}}', valueType: 'value' }),
        e('v2SystemPrompt', { location: 'historyend', value: 'sp', valueType: 'var' }),
        e('v2SystemPrompt', { location: 'promptend', value: 'Close', valueType: 'value' }),
        e('v2SystemPrompt', { location: 'start', value: 'Again', valueType: 'value' })])],
})
add('system-prompt-empty-value', { triggers: [S([e('v2SystemPrompt', { location: 'start', value: '', valueType: 'value' })])] })

// Chat edits and reads.
const cut = (start, end, startType = 'value', endType = 'value') => e('v2CutChat', { start, startType, end, endType })
const modify = (index, value, indexType = 'value', valueType = 'value') => e('v2ModifyChat', { index, indexType, value, valueType })
const impersonate = (role, value, valueType = 'value') => e('v2Impersonate', { role, value, valueType })
add('cut-range', { triggers: [S([cut('1', '3'), out('v2GetMessageCount')])] })
add('cut-nan-defaults', { triggers: [S([cut('x', 'missing', 'value', 'var'), out('v2GetMessageCount')])] })
add('cut-negative', { triggers: [S([cut('-2', '4')])] })
add('cut-empty-result', { triggers: [S([cut('3', '1'), out('v2GetLastMessage')])] })
add('cut-fractional', { triggers: [S([cut('0.7', '2.9')])] })
add('modify-valid', { triggers: [S([modify('1', 'edited {{getvar::name}}'), out('v2GetMessageAtIndex', { index: '1', indexType: 'value' })])] })
add('modify-out-of-range', { triggers: [S([modify('9', 'x'), modify('-1', 'y'), modify('1.5', 'z'), modify('', 'empty')])] })
add('modify-var-types', { chat: { scriptstate: { $idx: '2', $text: 'from var' } }, triggers: [S([modify('idx', 'text', 'var', 'var')])] })
add('modify-then-read-last', { triggers: [S([modify('3', 'new last'), out('v2GetLastMessage'), e('v2GetLastCharMessage', { outputVar: 'char' })])] })
add('impersonate-roles', { triggers: [S([impersonate('user', 'u1'), impersonate('char', 'c1'), impersonate('narrator', 'n1'), impersonate('user', 'name', 'var'), out('v2GetMessageCount')])] })
add('cut-then-modify', { triggers: [S([cut('2', '4'), modify('0', 'first now'), modify('3', 'gone')])] })
add('cut-then-impersonate', { triggers: [S([cut('0', '1'), impersonate('char', 'tail'), out('v2GetLastMessage')])] })
add('message-reads', {
    triggers: [S([out('v2GetLastMessage', { outputVar: 'last' }), out('v2GetMessageAtIndex', { index: '0', indexType: 'value', outputVar: 'at0' }),
        out('v2GetMessageAtIndex', { index: '7', indexType: 'value', outputVar: 'at7' }), out('v2GetMessageAtIndex', { index: '-1', indexType: 'value', outputVar: 'atn' }),
        out('v2GetMessageAtIndex', { index: '1.5', indexType: 'value', outputVar: 'atf' }), out('v2GetMessageAtIndex', { index: 'hp', indexType: 'var', outputVar: 'atv' }),
        out('v2GetMessageCount', { outputVar: 'count' }), out('v2GetLastUserMessage', { outputVar: 'user' }), out('v2GetLastCharMessage', { outputVar: 'char' })])],
})
add('message-reads-empty-chat', {
    chat: { message: [] },
    triggers: [S([out('v2GetLastMessage', { outputVar: 'last' }), out('v2GetMessageCount', { outputVar: 'count' }),
        out('v2GetLastUserMessage', { outputVar: 'user' }), out('v2GetLastCharMessage', { outputVar: 'char' })])],
})
add('first-message-default', { triggers: [S([out('v2GetFirstMessage')])] })
add('first-message-alternate', { chat: { fmIndex: 1 }, triggers: [S([out('v2GetFirstMessage')])] })
add('first-message-out-of-range', { chat: { fmIndex: 5 }, triggers: [S([out('v2GetFirstMessage')])] })
add('first-message-undefined-overwrites', {
    chat: { fmIndex: 5, scriptstate: { $out: 'x' } },
    triggers: [S([out('v2GetFirstMessage'), set('read', 'out', '=', 'var'), out('v2GetFirstMessage')])],
})
add('first-message-missing-index', { chat: { fmIndex: null, scriptstate: { $out: 'x' } }, triggers: [S([out('v2GetFirstMessage')])] })
// `alternateGreetings[fmIndex]` with the index as a property key: a string index, and `length`, which stores a number.
add('first-message-text-index', { chat: { fmIndex: '1' }, triggers: [S([out('v2GetFirstMessage')])] })
add('first-message-length-key', { chat: { fmIndex: 'length' }, triggers: [S([out('v2GetFirstMessage')])] })
const search = (value, condition, depth, valueType = 'value', depthType = 'value') => out('v2QuickSearchChat', { value, valueType, condition, depth, depthType })
add('quick-search-strict', { triggers: [S([search('Great', 'strict', '1', 'value', 'value'), search('hear', 'strict', '1', 'value', 'value')].map((effect, index) => ({ ...effect, outputVar: `s${index}` })))] })
add('quick-search-loose', { triggers: [S([search('FINE', 'loose', '2'), { ...search('fine', 'loose', '1'), outputVar: 'o2' }])] })
add('quick-search-regex', { triggers: [S([search('How\\s+are', 'regex', '3'), { ...search('^zzz', 'regex', '4'), outputVar: 'o2' }])] })
add('quick-search-depth-nan', { triggers: [S([search('hello', 'loose', 'deep')])] })
add('quick-search-unknown-condition', { triggers: [S([search('hello', 'fuzzy', '4')])] })
add('note-get-set', { triggers: [S([out('v2GetAuthorNote', { outputVar: 'before' }), e('v2SetAuthorNote', { value: 'new note', valueType: 'value' }), out('v2GetAuthorNote', { outputVar: 'after' })])] })
add('note-undefined', { chat: { note: null }, triggers: [S([out('v2GetAuthorNote')])] })
add('stop-prompt-sending', { triggers: [S([e('v2StopPromptSending'), set('after', '1')])] })

// Strings.
const src = (type, source, fields = {}, sourceType = 'value') => out(type, { source, sourceType, ...fields })
add('string-char-at', {
    triggers: [S([src('v2GetCharAt', 'h😀llo', { index: '1', indexType: 'value', outputVar: 'a' }), src('v2GetCharAt', 'abc', { index: '5', indexType: 'value', outputVar: 'b' }),
        src('v2GetCharAt', 'abc', { index: '-1', indexType: 'value', outputVar: 'c' }), src('v2GetCharAt', 'name', { index: '0', indexType: 'value', outputVar: 'd' }, 'var'),
        src('v2GetCharAt', 'abc', { index: '1.0', indexType: 'value', outputVar: 'e' })])],
})
add('string-char-count', { triggers: [S([src('v2GetCharCount', 'a😀b'), { ...src('v2GetCharCount', 'name', {}, 'var'), outputVar: 'o2' }])] })
add('string-case', {
    triggers: [S([src('v2ToLowerCase', 'ÀBÇ ΟΔΟΣ', { outputVar: 'lower' }), src('v2ToUpperCase', 'straße ǆ', { outputVar: 'upper' }),
        src('v2ToLowerCase', 'İSTANBUL', { outputVar: 'dotted' }), src('v2ToUpperCase', 'name', { outputVar: 'var' }, 'var')])],
})
const setCharAt = (source, index, value, outputVar) => src('v2SetCharAt', source, { index, indexType: 'value', value, valueType: 'value', outputVar })
add('string-set-char-at', {
    triggers: [S([setCharAt('a😀c', '1', 'X', 'a'), setCharAt('abc', '5', 'Z', 'b'), setCharAt('abc', '-1', 'Z', 'c'), setCharAt('abc', 'x', 'Z', 'd'), setCharAt('abc', '0', '', 'e')])],
})
const split = (source, delimiter, delimiterType, outputVar) => src('v2SplitString', source, { delimiter, delimiterType, outputVar })
add('string-split', { chat: { scriptstate: { $sep: '-' } }, triggers: [S([split('a,b,,c', ',', 'value', 'a'), split('héy', '', 'value', 'b'), split('x-y', 'sep', 'var', 'c'), split('', ',', 'value', 'd')])] })
add('string-concat', { triggers: [S([out('v2ConcatString', { source1: 'name', source1Type: 'var', source2: ' & co', source2Type: 'value' })])] })
const extract = (value, regex, flags, result, outputVar) => out('v2ExtractRegex', { value, valueType: 'value', regex, regexType: 'value', flags, flagsType: 'value', result, resultType: 'value', outputVar })
add('extract-regex', {
    triggers: [S([extract('id: 42-7', '(\\d+)-(\\d+)', '', '$2/$1 [$&] $$ $3 $10', 'a'), extract('ABC', 'b', 'i', 'got $&', 'b'),
        extract('none', '\\d', 'g', 'x$1y$&z$$', 'c'), extract('a$&b', '\\$&', '', '<$&>', 'd'), extract('price $5', '\\$(\\d)', '', 'v=$1', 'e')])],
})
// The match replaces `$&` as a replacement pattern, so its own `$`, `$'` and `$$` expand against the format.
add('extract-regex-match-patterns', {
    triggers: [S([extract('x$`y', '\\$`', '', 'L$&R', 'a'), extract("x$'y", "\\$'", '', 'L$&R', 'b'), extract('x$$y', '\\$\\$', '', 'L$&R', 'c')])],
})
add('extract-regex-invalid', { triggers: [S([set('before', '1'), modify('0', 'dropped'), extract('x', '(', '', '$1', 'a'), set('after', '1')])] })
const regexTest = (value, regex, flags, outputVar) => out('v2RegexTest', { value, valueType: 'value', regex, regexType: 'value', flags, flagsType: 'value', outputVar })
add('regex-test', { triggers: [S([regexTest('Hello', '^h', 'i', 'a'), regexTest('Hello', '^h', '', 'b'), regexTest('x', '(', '', 'c'), regexTest('abc', 'b', 'g', 'd')])] })

// Arrays.
const arr = (type, fields = {}) => e(type, { var: 'arr', ...fields })
add('array-make', { triggers: [S([e('v2MakeArrayVar', { var: 'fresh' }), e('v2MakeArrayVar', { var: 'arr' })])] })
add('array-make-bracket-name', { triggers: [S([set('a', '1'), e('v2MakeArrayVar', { var: '[list]' }), set('b', '1')])], native: 'unsupported-trigger-early-return' })
add('array-length', {
    chat: { scriptstate: { $bad: 'nope', $nul: 'null', $obj: '{"a":1}' } },
    triggers: [S([arr('v2GetArrayVarLength', { outputVar: 'a' }), arr('v2GetArrayVarLength', { var: 'bad', outputVar: 'b' }),
        arr('v2GetArrayVarLength', { var: 'nul', outputVar: 'c' }), arr('v2GetArrayVarLength', { var: 'missing', outputVar: 'd' })])],
})
const get = (index, outputVar, name = 'arr') => arr('v2GetArrayVar', { var: name, index, indexType: 'value', outputVar })
add('array-get', {
    chat: { scriptstate: { $mixed: '["s",null]', $bad: '[' } },
    triggers: [S([get('1', 'a'), get('3', 'b'), get('-1', 'c'), get('x', 'd'), get('1', 'e', 'mixed'), get('0', 'f', 'bad')])],
})
add('array-get-number-element', { chat: { scriptstate: { $nums: '[1,2]' } }, triggers: [S([get('0', 'a', 'nums')])] })
const setAt = (index, value, name = 'arr') => arr('v2SetArrayVar', { var: name, index, indexType: 'value', value, valueType: 'value' })
add('array-set', { chat: { scriptstate: { $bad: '{' } }, triggers: [S([setAt('1', 'B'), setAt('x', 'nan'), setAt('-1', 'neg'), setAt('5', 'far'), setAt('0', 'v', 'bad')])] })
add('array-set-formatting', { chat: { scriptstate: { $sp: '[ "a" , 1 ]' } }, triggers: [S([setAt('0', 'a', 'sp')])] })
add('array-push-pop', {
    chat: { scriptstate: { $bad: 'x', $empty: '[]' } },
    triggers: [S([arr('v2PushArrayVar', { value: 'd', valueType: 'value' }), arr('v2PopArrayVar', { outputVar: 'popped' }),
        arr('v2PushArrayVar', { var: 'bad', value: 'd', valueType: 'value' }), arr('v2PopArrayVar', { var: 'empty', outputVar: 'none' }),
        arr('v2PopArrayVar', { var: 'gone', outputVar: 'gonePop' })])],
})
add('array-shift-unshift', {
    triggers: [S([arr('v2ShiftArrayVar', { outputVar: 'first' }), arr('v2UnshiftArrayVar', { value: 'name', valueType: 'var' }),
        arr('v2ShiftArrayVar', { var: 'nothing', outputVar: 'none' }), arr('v2UnshiftArrayVar', { var: 'nothing2', value: 'x', valueType: 'value' })])],
})
add('array-splice-slice', {
    triggers: [S([arr('v2SpliceArrayVar', { start: '1', startType: 'value', item: 'ins', itemType: 'value' }),
        arr('v2SliceArrayVar', { start: '1', startType: 'value', end: '3', endType: 'value', outputVar: 'mid' }),
        arr('v2SliceArrayVar', { start: '-2', startType: 'value', end: 'x', endType: 'value', outputVar: 'nanEnd' }),
        arr('v2SpliceArrayVar', { start: '-1', startType: 'value', item: 'neg', itemType: 'value' }),
        arr('v2SpliceArrayVar', { var: 'none', start: '0', startType: 'value', item: 'x', itemType: 'value' })])],
})
add('array-index-remove', {
    triggers: [S([arr('v2GetIndexOfValueInArrayVar', { value: 'b', valueType: 'value', outputVar: 'ib' }),
        arr('v2GetIndexOfValueInArrayVar', { value: 'z', valueType: 'value', outputVar: 'iz' }),
        arr('v2RemoveIndexFromArrayVar', { index: '0', indexType: 'value' }), arr('v2RemoveIndexFromArrayVar', { index: 'x', indexType: 'value' }),
        arr('v2GetIndexOfValueInArrayVar', { var: 'none', value: 'b', valueType: 'value', outputVar: 'inone' })])],
})
const join = (name, delimiter, outputVar, varType = 'var') => e('v2JoinArrayVar', { var: name, varType, delimiter, delimiterType: 'value', outputVar })
add('array-join', {
    chat: { scriptstate: { $mixed: '[1,null,true,[2,3],{"a":1},"s",1e21,-0]' } },
    triggers: [S([join('arr', '+', 'a'), join('mixed', '|', 'b'), join('["x","y"]', ', ', 'c', 'value'), join('name', ',', 'd'), join('none', ',', 'e')])],
})

// Dictionaries.
const dict = (type, fields = {}) => e(type, { var: 'dict', varType: 'var', ...fields })
add('dict-make-clear', { triggers: [S([e('v2MakeDictVar', { var: 'fresh' }), e('v2ClearDict', { var: 'dict' })])] })
add('dict-make-brace-name', { triggers: [S([set('a', '1'), e('v2MakeDictVar', { var: '{d}' })])], native: 'unsupported-trigger-early-return' })
add('dict-get-has', {
    chat: { scriptstate: { $bad: 'x' } },
    triggers: [S([dict('v2GetDictVar', { key: 'k', keyType: 'value', outputVar: 'k' }), dict('v2GetDictVar', { key: 'zz', keyType: 'value', outputVar: 'zz' }),
        dict('v2GetDictVar', { var: 'bad', key: 'k', keyType: 'value', outputVar: 'bad' }), dict('v2HasDictKey', { key: '2', keyType: 'value', outputVar: 'h2' }),
        dict('v2HasDictKey', { key: 'toString', keyType: 'value', outputVar: 'hs' }), dict('v2GetDictVar', { var: '{"q":"lit"}', varType: 'value', key: 'q', keyType: 'value', outputVar: 'lit' })])],
})
add('dict-set-delete', {
    chat: { scriptstate: { $bad: '[' } },
    triggers: [S([dict('v2SetDictVar', { key: '1', keyType: 'value', value: 'one', valueType: 'value' }), dict('v2SetDictVar', { key: 'z', keyType: 'value', value: 'name', valueType: 'var' }),
        dict('v2DeleteDictKey', { key: 'k', keyType: 'value' }), dict('v2SetDictVar', { var: 'bad', key: 'a', keyType: 'value', value: 'b', valueType: 'value' }),
        dict('v2SetDictVar', { var: 'x', varType: 'value', key: 'a', keyType: 'value', value: 'b', valueType: 'value' }),
        dict('v2DeleteDictKey', { var: 'nothing', key: 'a', keyType: 'value' })])],
})
add('dict-size-keys-values', {
    chat: { scriptstate: { $nums: '{"b":"1","10":"x","a":"2","2":"y"}' } },
    triggers: [S([dict('v2GetDictSize', { var: 'nums', outputVar: 'size' }), dict('v2GetDictKeys', { var: 'nums', outputVar: 'keys' }),
        dict('v2GetDictValues', { var: 'nums', outputVar: 'values' }), dict('v2GetDictSize', { var: 'missing', outputVar: 'nsize' }),
        dict('v2GetDictKeys', { var: 'missing', outputVar: 'nkeys' })])],
})
// A `catch` parses the output name again when the `try` threw after parsing it, and once when `JSON.parse` threw.
const twice = '{{random::p::q}}'
add('catch-parses-output-again', {
    random: Array.from({ length: 20 }, (_, index) => (index % 2 ? 0.9 : 0.1)),
    chat: { scriptstate: { $nul: 'null', $obj: '{"a":"1"}', $bad: '{' } },
    triggers: [S([arr('v2GetArrayVarLength', { var: 'nul', outputVar: `len${twice}` }), get('0', `get${twice}`, 'nul'),
        arr('v2GetIndexOfValueInArrayVar', { var: 'obj', value: 'a', valueType: 'value', outputVar: `idx${twice}` }),
        arr('v2SliceArrayVar', { var: 'obj', start: '0', startType: 'value', end: '1', endType: 'value', outputVar: `slice${twice}` }),
        join('obj', ',', `join${twice}`), dict('v2GetDictVar', { var: 'nul', key: 'a', keyType: 'value', outputVar: `dget${twice}` }),
        dict('v2HasDictKey', { var: 'nul', key: 'a', keyType: 'value', outputVar: `dhas${twice}` }),
        dict('v2GetDictSize', { var: 'nul', outputVar: `dsize${twice}` }), get('0', `once${twice}`, 'bad')])],
})

// Values a run stores as they are: `getVar` returns them unconverted until the commit, and each reader converts them its
// own way (`Number`, `+`, `===`, `includes`, string methods, `JSON.parse`, property keys).
const raw = { $mix: '[1,0,true,[3,4],{"k":"v"},2,null,"s"]' }
const pick = (index, outputVar) => get(String(index), outputVar, 'mix')
const concat = (source1, source1Type, source2, source2Type, outputVar) => out('v2ConcatString', { source1, source1Type, source2, source2Type, outputVar })
add('raw-arithmetic-and-concat', {
    chat: { scriptstate: raw },
    triggers: [S([pick(0, 'one'), pick(5, 'two'), pick(2, 'yes'), pick(3, 'pair'), pick(4, 'obj'), pick(1, 'zero'), pick(6, 'nul'),
        set('sum', 'one', '+=', 'var'), set('sum', 'two', '+=', 'var'), set('sum', 'yes', '+=', 'var'), set('sum', 'pair', '+=', 'var'),
        concat('one', 'var', 'two', 'var', 'add'), concat('one', 'var', 'yes', 'var', 'addBool'), concat('one', 'var', ' x', 'value', 'cat'),
        concat('pair', 'var', 'one', 'var', 'catPair'), concat('obj', 'var', 'one', 'var', 'catObj'), concat('add', 'var', 'zero', 'var', 'addAgain')])],
})
add('raw-compare-and-identity', {
    chat: { scriptstate: raw },
    triggers: [S([pick(0, 'one'), pick(4, 'obj'), pick(3, 'pair'), pick(2, 'yes'),
        set('copy', 'one', '=', 'var'), set('copy', 'one', '=', 'var'), set('ocopy', 'obj', '=', 'var'), set('ocopy', 'obj', '=', 'var'), pick(4, 'obj2'),
        iff('one', '=', '1'), set('numEq', '1', '=', 'value', 1), end(1),
        adv('var', 'obj', '=', 'var', 'ocopy'), set('sameRef', '1', '=', 'value', 1), end(1),
        adv('var', 'obj', '=', 'var', 'obj2'), set('otherRef', '1', '=', 'value', 1), end(1),
        adv('var', 'one', '∈', 'value', '[1,2]'), set('inNum', '1', '=', 'value', 1), end(1),
        adv('var', 'pair', '∈', 'value', '"3,4!"'), set('inText', '1', '=', 'value', 1), end(1),
        adv('var', 'yes', '≡', 'value', 'true'), set('equivBool', '1', '=', 'value', 1), end(1),
        adv('var', 'pair', '≡', 'var', 'pair'), set('equivSame', '1', '=', 'value', 1), end(1),
        adv('var', 'yes', '>', 'value', '0'), set('boolGt', '1', '=', 'value', 1), end(1),
        e('v2LoopNTimes', { value: 'one', valueType: 'var' }), set('loops', '1', '+=', 'value', 1), end(1, true)])],
})
add('raw-string-and-array-readers', {
    chat: { scriptstate: raw },
    random: [0.5],
    triggers: [S([pick(0, 'one'), pick(3, 'pair'), pick(4, 'obj'), pick(2, 'yes'),
        src('v2GetCharAt', 'pair', { index: '1', indexType: 'value', outputVar: 'at' }, 'var'),
        src('v2GetCharAt', 'one', { index: '0', indexType: 'value', outputVar: 'atNum' }, 'var'),
        src('v2GetCharAt', 'obj', { index: '0', indexType: 'value', outputVar: 'atObj' }, 'var'),
        src('v2GetCharCount', 'pair', { outputVar: 'count' }, 'var'),
        src('v2SetCharAt', 'pair', { index: '0', indexType: 'value', value: 'yes', valueType: 'var', outputVar: 'setAt' }, 'var'),
        split('a1b', 'one', 'var', 'parts'), e('v2SystemPrompt', { location: 'start', value: 'pair', valueType: 'var' }),
        join('pair', ',', 'joinPair'), join('one', ',', 'joinOne'),
        arr('v2PushArrayVar', { value: 'obj', valueType: 'var' }), arr('v2PushArrayVar', { value: 'one', valueType: 'var' }),
        arr('v2GetIndexOfValueInArrayVar', { value: 'one', valueType: 'var', outputVar: 'idxOne' }), arr('v2PopArrayVar', { outputVar: 'popped' }),
        arr('v2SetArrayVar', { index: '0', indexType: 'value', value: 'pair', valueType: 'var' }),
        arr('v2GetArrayVarLength', { var: 'one', outputVar: 'lenOne' }),
        out('v2RegexTest', { value: 'one', valueType: 'var', regex: '^1$', regexType: 'value', flags: '', flagsType: 'value', outputVar: 'reOne' }),
        { ...search('one', 'strict', '4', 'var'), outputVar: 'strictOne' },
        e('v2CutChat', { start: 'one', startType: 'var', end: 'pair', endType: 'var' }),
        out('v2Random', { min: 'one', minType: 'var', max: 'yes', maxType: 'var', outputVar: 'rand' }),
        e('v2ConsoleLog', { source: 'pair', sourceType: 'var' }), e('v2ConsoleLog', { source: 'obj', sourceType: 'var' }),
        local('loc', 'obj', 'var'), set('fromLocal', 'loc', '=', 'var')])],
})
add('raw-dict-readers', {
    chat: { scriptstate: raw },
    triggers: [S([pick(0, 'one'), pick(5, 'two'), pick(3, 'pair'),
        dict('v2SetDictVar', { key: 'one', keyType: 'var', value: 'pair', valueType: 'var' }),
        dict('v2GetDictVar', { key: 'two', keyType: 'var', outputVar: 'byNum' }), dict('v2GetDictVar', { key: '1', keyType: 'value', outputVar: 'nested' }),
        dict('v2HasDictKey', { key: 'one', keyType: 'var', outputVar: 'hasOne' }), dict('v2DeleteDictKey', { key: 'two', keyType: 'var' }),
        dict('v2GetDictKeys', { outputVar: 'keys' }), dict('v2GetDictVar', { var: 'one', key: 'x', keyType: 'value', outputVar: 'numDict' }),
        dict('v2GetDictVar', { var: '[1,2]', varType: 'value', key: 'length', keyType: 'value', outputVar: 'lengthKey' }),
        arr('v2PopArrayVar', { var: 'mix', outputVar: 'last' }), arr('v2ShiftArrayVar', { var: 'mix', outputVar: 'first' })])],
})
add('first-message-null-greeting', {
    character: { alternateGreetings: [null, 5] },
    chat: { fmIndex: 0, scriptstate: { $out: 'x' } },
    triggers: [S([out('v2GetFirstMessage'), set('read', 'out', '=', 'var'), set('n', 'out', '+=', 'var'), { ...out('v2GetFirstMessage'), outputVar: 'again' }])],
})
add('first-message-number-greeting', { character: { alternateGreetings: ['a', 5] }, chat: { fmIndex: 1 }, triggers: [S([out('v2GetFirstMessage'), set('n', 'out', '+=', 'var')])] })
// Readers that throw on a non-string, and those the native runner cannot express.
add('raw-lowercase-throws', { chat: { scriptstate: raw }, triggers: [S([pick(0, 'one'), src('v2ToLowerCase', 'one', {}, 'var')])] })
add('raw-cbs-getvar-throws', { chat: { scriptstate: raw }, triggers: [S([pick(0, 'one'), set('shown', '[{{getvar::one}}]')])] })
add('raw-condition-throws', { chat: { scriptstate: raw }, triggers: [S([pick(0, 'one')]), { ...S([set('ran', '1')]), conditions: [varCond('one', '=', '1')] }] })
add('raw-split-receiver-throws', { chat: { scriptstate: raw }, triggers: [S([pick(3, 'pair'), split('pair', ',', 'value', 'parts')].map((effect, index) => index ? { ...effect, sourceType: 'var' } : effect))] })
add('raw-cbs-getvar-falsy-keeps-tag', {
    chat: { scriptstate: raw },
    triggers: [S([local('zero', 'x'), pick(1, 'zero'), set('shown', '[{{getvar::zero}}]')])],
})
// `isTruthy` and `===` compare strings, so a non-string variable is never truthy or equal.
const when = (condition) => `{{#when::${condition}}}T{{:else}}F{{/when}}`
add('raw-cbs-conditions', {
    chat: { scriptstate: raw },
    triggers: [S([pick(0, 'one'), pick(3, 'pair'), local('yes', 'x'), pick(2, 'yes'), set('shown', ['var::one', 'one::vis::1', 'one::visnot::1',
        'var::pair', 'pair::vis::3,4', 'var::yes', 'yes::vis::true', 'yes::visnot::true'].map(when).join(''))])],
})
// A callback that reparses text keeps its tag when that text throws, and the trigger goes on.
add('raw-cbs-getvar-nested-keeps-tag', {
    chat: { scriptstate: raw, note: '{{getvar::one}}' },
    triggers: [S([local('one', 'x'), pick(0, 'one'), set('shown', '{{authornote}}'), set('after', '1')])],
})
add('raw-modify-chat-stores-value', {
    chat: { scriptstate: raw },
    triggers: [S([local('one', 'x'), pick(0, 'one'), modify('0', 'one', 'value', 'var')])],
})
// `authornote` reparses the note inside its callback: a falsy note falls through to the template default, and a
// truthy non-string throws there, so the matcher keeps the tag.
const note = (name) => e('v2SetAuthorNote', { value: name, valueType: 'var' })
add('raw-note-reads', {
    chat: { scriptstate: raw },
    triggers: [S([local('one', 'x'), pick(0, 'one'), pick(1, 'zero'), pick(4, 'obj'),
        note('zero'), set('readZero', '{{authornote}}'), note('obj'), set('readObj', '{{authornote}}'), note('one'), set('readOne', '{{authornote}}')])],
})
add('raw-note-stored', { chat: { note: 5 }, triggers: [S([set('read', '{{authornote}}')])] })

// v1 effects: every value is parsed, none has a value type, and `extractRegex` and the host effects need low-level access.
const v1 = (type, fields) => ({ type, ...fields })
add('v1-setvar', {
    triggers: [S([v1('setvar', { var: 'a', value: '{{getvar::name}}', operator: '=' }), v1('setvar', { var: 'hp', value: '5', operator: '+=' }),
        v1('setvar', { var: 'm', value: '2', operator: '-=' }), v1('setvar', { var: 't', value: '3', operator: '*=' }),
        v1('setvar', { var: 'hp', value: '3', operator: '/=' }), v1('setvar', { var: 'r', value: '7', operator: '%=' }),
        v1('setvar', { var: '{{getvar::name}}', value: 'k', operator: '=' })])],
})
add('v1-system-prompt-and-impersonate', {
    triggers: [S([v1('systemprompt', { location: 'start', value: 'S {{getvar::name}}' }), v1('systemprompt', { location: 'promptend', value: 'E' }),
        v1('systemprompt', { location: 'other', value: 'X' }), v1('impersonate', { role: 'user', value: 'u {{getvar::hp}}' }),
        v1('impersonate', { role: 'char', value: 'c' }), v1('impersonate', { role: 'narrator', value: 'n' })])],
})
add('v1-stop-and-runtrigger', {
    triggers: [S([v1('stop', {}), v1('runtrigger', { value: 'sub' }), v1('runtrigger', { value: '{{getvar::name}}' })]),
        T('sub', 'manual', [set('sub', 'ran')]), T('{{getvar::name}}', 'manual', [set('literal', 'ran')])],
})
add('v1-cutchat-modifychat', { triggers: [S([v1('modifychat', { index: '1', value: 'edited {{getvar::name}}' }), v1('cutchat', { start: '1', end: '3' }), out('v2GetMessageCount')])] })
add('v1-cutchat-nan', {
    triggers: [S([v1('cutchat', { start: 'x', end: '3' }), out('v2GetMessageCount', { outputVar: 'a' }), v1('cutchat', { start: '0', end: 'y' }),
        out('v2GetMessageCount', { outputVar: 'b' })])],
})
add('v1-host-effects-without-low-level', {
    triggers: [S([v1('showAlert', { alertType: 'normal', value: 'hi', inputVar: 'x' }), v1('runLLM', { value: 'q', inputVar: 'llm' }),
        v1('checkSimilarity', { value: 'a§b', source: 'a', inputVar: 'sim' }), v1('runImgGen', { value: 'q', negValue: '', inputVar: 'img' }),
        v1('extractRegex', { value: 'x', regex: 'x', flags: '', result: '$&', inputVar: 'ex' }), v1('sendAIprompt', {}), set('after', '1')])],
})
add('v1-extract-regex-low-level', {
    character: { lowLevelAccess: true },
    triggers: [S([v1('extractRegex', { value: 'id {{getvar::hp}}-7', regex: '(\\d+)-(\\d+)(x)?', flags: '', result: '$2/$1 [$&] $3 $9 $$', inputVar: '{{getvar::name}}' }),
        v1('sendAIprompt', {})])],
})
add('v1-extract-regex-no-match', { character: { lowLevelAccess: true }, triggers: [S([set('before', '1'), v1('extractRegex', { value: 'abc', regex: '\\d', flags: '', result: 'none', inputVar: 'ex' })])] })

// Logs and random.
add('console-log', { triggers: [S([e('v2ConsoleLog', { source: 'logged {{getvar::name}}', sourceType: 'value' }), set('after', '1')])] })
add('random-range', { random: [0, 0.5, 0.999999], triggers: [S([1, 2, 3].map(n => out('v2Random', { min: '1', minType: 'value', max: '6', maxType: 'value', outputVar: `r${n}` })))] })
add('random-nan', { random: [0.25], triggers: [S([out('v2Random', { min: 'x', minType: 'value', max: '3', maxType: 'value' })])] })

// `v2ReplaceString`: the matches `input.replace(regex, ...)` visits, rebuilt by `replaceRegexMatches` with the trigger's own
// `$` handling, and the `catch` that stores the source again.
const replace = (source, regex, flags, result, replacement, outputVar, types = {}) => out('v2ReplaceString', {
    source, sourceType: 'value', regex, regexType: 'value', flags, flagsType: 'value', result, resultType: 'value',
    replacement, replacementType: 'value', outputVar, ...types,
})
add('replace-string-format', {
    triggers: [S([replace('a1-b22 c333', '([a-z])(\\d+)', 'g', '[$2:$1|$0|$&|$3|$01|$10|a$$$b|$]', 'R', 'fmt'),
        replace('abc', 'b', 'g', '$0', 'X', 'whole'), replace('a1b2', '\\d', '', '<$&>', '', 'first'),
        replace('{{getvar::name}} x', 'x', 'g', '$0', '{{getvar::name}}', 'parsed'), replace('p$`q', '\\$`', 'g', '<$&>', '', 'matchExpands')])],
})
add('replace-string-target-group', {
    triggers: [S([replace('x12y12', '(\\d)(\\d)', 'g', '$2', "<$&|$`|$'|$$|$1|$>", 'target'), replace('x1', '(\\d)(\\d)?', '', '$2', 'R', 'unmatched'),
        replace('aa', '(a)', 'g', '$1', '$&$&', 'doubled'), replace('ab', '(b)', 'g', '$01', 'R', 'padded')])],
})
add('replace-string-empty-matches', {
    triggers: [S([replace('a😀b', 'x*', 'gu', '[$&]', '', 'unicode'), replace('a😀b', 'x*', 'g', '[$&]', '', 'units'),
        replace('aaa', 'a*?', 'g', '<$0>', '', 'lazy'), replace('abab', 'b', 'y', '_', '', 'sticky'), replace('bab', 'b', 'gy', '_', '', 'stickyGlobal')])],
})
add('replace-string-named-groups', {
    triggers: [S([replace('ab ab', '(?<w>b)', 'g', '$1@$2', '', 'offsets'), replace('a1x', '\\d(?<w>x)', '', '$2', 'Z', 'offsetTarget'),
        replace('b', '(?<w>b)', '', '[$2]', '', 'zeroOffset')])],
})
add('replace-string-caught', {
    chat: { scriptstate: raw },
    triggers: [S([pick(0, 'one'), pick(6, 'nul'), pick(4, 'obj'), pick(3, 'pair'),
        replace('one', 'x', 'g', '$0', 'R', 'rawSource', { sourceType: 'var' }), replace('abc', '(', 'g', '$0', 'R', 'badRegex'),
        replace('abc', 'b', 'q', '$0', 'R', 'badFlags'), replace('1b1', '1', 'one', '$0', 'R', 'rawFlags', { flagsType: 'var' }),
        replace('abc', 'b', 'g', 'one', 'R', 'rawFormat', { resultType: 'var' }), replace('abc', 'x', 'g', 'one', 'R', 'rawFormatUnused', { resultType: 'var' }),
        replace('abc', 'b', 'g', '$0', 'nul', 'nullWhole', { replacementType: 'var' }), replace('abc', '(b)', 'g', '$1', 'nul', 'nullTarget', { replacementType: 'var' }),
        replace('abc', 'b', 'g', '$0', 'obj', 'objWhole', { replacementType: 'var' }), replace('abc', 'b', 'g', '$0', 'pair', 'arrayWhole', { replacementType: 'var' })])],
})
add('replace-string-catch-reparses', { random: [0.1, 0.9, 0.1], triggers: [S([replace(`s${twice}`, '(', 'g', '$0', 'R', `out${twice}`)])] })

const nums = { $nums: '[0,1]' }
add('raw-cbs-getvar-pending-falsy-keeps-tag', {
    chat: { scriptstate: nums },
    triggers: [S([e('v2SystemPrompt', { location: 'start', value: 'S', valueType: 'value' }), get('0', 'z', 'nums'), set('shown', '{{getvar::z}}'),
        modify('0', 'edited')])],
})
add('raw-note-after-effects', {
    chat: { scriptstate: nums },
    triggers: [S([impersonate('user', 'u'), get('1', 'n', 'nums'), note('n'), set('after', '1')])],
})
add('raw-modify-chat-after-effects-stores-value', {
    chat: { scriptstate: nums },
    triggers: [S([set('a', '1'), get('1', 'n', 'nums'), modify('0', 'n', 'value', 'var'), set('after', '1')])],
})
// Message data need not be a string. CBS writes `'' + data` in `history` with arguments, keeps the tag of a reader
// that reparses it, and returns it as the callback result elsewhere: a falsy value keeps the tag, and a truthy one
// throws unless its `var` holds a value.
const stored = (...data) => data.map((value, index) => msg(index % 2 ? 'char' : 'user', value, `m${index}`))
add('message-data-cbs-reads', {
    chat: { message: stored(0, null, [1, null, [2.5, true]], -1.5e-7, 1e21, true, '', false) },
    triggers: [S([set('last', '{{lastmessage}}'), set('log0', '{{previouschatlog::0}}'), set('log1', '{{previouschatlog::1}}'),
        set('log6', '[{{previouschatlog::6}}]'), set('char', '{{previouscharchat}}'), set('roles', '{{history::role}}'),
        set('plain', '{{history::x}}'), set('history', '{{history}}'), set('users', '{{userhistory}}'), set('chars', '{{charhistory}}')])],
})
const readThrows = (id, data, reader) => add(id, {
    chat: { message: stored('u', ...data) }, triggers: [S([set('a', '1'), set('shown', reader), set('after', '1')])],
})
readThrows('message-data-lastmessage-throws', [5], '{{lastmessage}}')
readThrows('message-data-previouschatlog-throws', [[]], '{{previouschatlog::1}}')
readThrows('message-data-previouscharchat-throws', [{ k: 'v' }], '{{previouscharchat}}')
add('message-data-var-object', {
    chat: { message: stored('u', { text: 't', var: { x: '1' } }) },
    triggers: [S([set('shown', '[{{lastmessage}}]'), set('after', '1')])],
    native: 'unsupported-matcher-result-var',
    admitted: true,
})
add('message-data-history-object', {
    chat: { message: stored('u', { k: 'v' }, [1, [{ k: 'v' }]]) },
    triggers: [S([set('roles', '{{history::role}}'), set('after', '1')])],
    native: 'unsupported-message-data-object',
    admitted: true,
})
// The trigger readers return stored data as it is, `?? 'null'` reads only null and undefined, and the chat searches
// join the data as `join(' ')` does.
add('message-data-trigger-reads', {
    chat: { message: stored(0, null, [1, null], false) },
    triggers: [S([out('v2GetMessageAtIndex', { index: '0', indexType: 'value', outputVar: 'at0' }),
        out('v2GetMessageAtIndex', { index: '1', indexType: 'value', outputVar: 'at1' }),
        out('v2GetMessageAtIndex', { index: '2', indexType: 'value', outputVar: 'at2' }),
        out('v2GetLastMessage', { outputVar: 'last' }), e('v2GetLastUserMessage', { outputVar: 'user' }),
        e('v2GetLastCharMessage', { outputVar: 'char' }),
        { ...search('1,', 'strict', '4'), outputVar: 'strictArray' }, { ...search('0  1,', 'loose', '4'), outputVar: 'looseNull' }])],
})
// `v2Impersonate` and `v2ModifyChat` store a variable as it is, and the CBS and trigger readers see it.
const kept = { $kept: '[0,5,null,{"k":"v"},[1,null],false]' }
add('message-data-written-values', {
    chat: { scriptstate: kept },
    triggers: [S([local('v', 'x'),
        get('0', 'v', 'kept'), impersonate('user', 'v', 'var'), out('v2GetLastMessage', { outputVar: 'last' }), set('cbsLast', '[{{lastmessage}}]'),
        get('2', 'v', 'kept'), impersonate('char', 'v', 'var'), out('v2GetLastMessage', { outputVar: 'lastNull' }),
        set('logNull', '{{previouschatlog::5}}'),
        get('4', 'v', 'kept'), modify('5', 'v', 'value', 'var'), out('v2GetMessageAtIndex', { index: '5', indexType: 'value', outputVar: 'at' }),
        set('roles', '{{history::role}}'), set('history', '{{history}}'), { ...search('great', 'loose', '3'), outputVar: 'found' },
        get('5', 'v', 'kept'), modify('0', 'v', 'value', 'var'), e('v2GetLastUserMessage', { outputVar: 'user' }),
        get('1', 'v', 'kept'), impersonate('user', 'v', 'var'), e('v2GetLastUserMessage', { outputVar: 'user5' }),
        get('3', 'v', 'kept'), impersonate('char', 'v', 'var'), e('v2GetLastCharMessage', { outputVar: 'char' }),
        { ...search('object', 'loose', '1'), outputVar: 'foundObject' }, { ...search('5', 'strict', '2'), outputVar: 'strictNumber' },
        set('after', '1')])],
})
// `getVar` skips a local that holds undefined, as it does null, so the message stores the variable read after it.
add('message-data-undefined-local-reads-variable', {
    chat: { scriptstate: kept },
    triggers: [S([local('v', 'x'), get('9', 'v', 'kept'), modify('0', 'v', 'value', 'var'), set('after', '1')])],
})
// A variable read the same way: an object whose `var` holds a value goes on with that `var`, and a null one throws.
const forced = { $forced: '[{"text":"t","var":{"x":"1"}},{"text":"t","var":null}]' }
add('raw-cbs-getvar-var-object', {
    chat: { scriptstate: forced },
    triggers: [S([local('v', 'x'), get('0', 'v', 'forced'), set('shown', '[{{getvar::v}}]'), set('after', '1')])],
    native: 'unsupported-matcher-result-var',
    admitted: true,
})
add('raw-cbs-getvar-null-var-throws', {
    chat: { scriptstate: forced },
    triggers: [S([local('v', 'x'), get('1', 'v', 'forced'), set('shown', '[{{getvar::v}}]'), set('after', '1')])],
})
// The `try` of an effect also catches a `TypeError` while it reads its arguments: here CBS `getvar` of an object.
const boom = '{{getvar::obj}}'
add('try-catches-argument-throws', {
    chat: { scriptstate: { ...raw, $list: '["a","b"]', $list2: '["a"]', $list3: '["a"]', $list4: '["a"]', $d: '{"k":"v"}', $d2: '{"k":"v"}' } },
    triggers: [S([pick(4, 'obj'), replace('abc', 'b', 'g', '$0', boom, 'replaced'), replace('abc', 'b', 'g', boom, 'R', 'formatted'),
        out('v2RegexTest', { value: 'abc', valueType: 'value', regex: boom, regexType: 'value', flags: '', flagsType: 'value', outputVar: 'tested' }),
        { ...join('list', ',', 'joined'), delimiter: boom }, get(boom, 'got', 'list'), arr('v2GetArrayVarLength', { var: boom, outputVar: 'len' }),
        arr('v2SliceArrayVar', { var: 'list', start: boom, startType: 'value', end: '1', endType: 'value', outputVar: 'sliced' }),
        arr('v2GetIndexOfValueInArrayVar', { var: 'list', value: boom, valueType: 'value', outputVar: 'idx' }),
        arr('v2SetArrayVar', { var: boom, index: '0', indexType: 'value', value: 'x', valueType: 'value' }),
        arr('v2PushArrayVar', { var: 'list2', value: boom, valueType: 'value' }),
        arr('v2SpliceArrayVar', { var: 'list3', start: boom, startType: 'value', item: 'x', itemType: 'value' }),
        arr('v2RemoveIndexFromArrayVar', { var: 'list4', index: boom, indexType: 'value' }),
        dict('v2GetDictVar', { var: 'd', key: boom, keyType: 'value', outputVar: 'dget' }), dict('v2HasDictKey', { var: 'd', key: boom, keyType: 'value', outputVar: 'dhas' }),
        dict('v2GetDictSize', { var: boom, varType: 'value', outputVar: 'dsize' }), dict('v2GetDictKeys', { var: boom, varType: 'value', outputVar: 'dkeys' }),
        dict('v2DeleteDictKey', { var: 'd2', key: boom, keyType: 'value' }), set('after', '1')])],
})
add('try-catch-rethrows-output-name', {
    chat: { scriptstate: { ...raw, $list: '["a","b"]' } },
    triggers: [S([pick(4, 'obj'), arr('v2PopArrayVar', { var: 'list', outputVar: boom }), set('never', '1')])],
})
add('try-catch-rethrows-dict-key', {
    chat: { scriptstate: { ...raw, $d: '{"k":"v"}' } },
    triggers: [S([pick(4, 'obj'), dict('v2SetDictVar', { var: 'd', key: boom, keyType: 'value', value: 'x', valueType: 'value' }), set('never', '1')])],
})
// `$&` in a group's text expands again as the match.
add('extract-group-holds-match-pattern', {
    triggers: [S([extract('x$&y', '.(\\$&).', '', '[$1]', 'ex'), replace('x$&y', '.(\\$&).', 'g', '[$1|$2]', '', 'rep')])],
})

// `v2Calculate`: each `$name` becomes `parseFloat(getVar(name))` before `calcString`; `@name` reaches the engine.
const calc = (expression, outputVar, expressionType = 'value') => out('v2Calculate', { expression, expressionType, outputVar })
add('calculate-integer-and-float', {
    chat: { scriptstate: { $hp: '10', $f: ' 2.5e1px', $neg: '-3', $expr: '2*$hp', $dot: '.5' } },
    triggers: [S([calc('1+2*3', 'int'), calc('(1+2)*3', 'paren'), calc('7/2', 'div'), calc('0.1+0.2', 'float'), calc('$hp-$neg', 'vars'),
        calc('$f/5', 'floatVar'), calc('2^10', 'pow'), calc('10%3', 'mod'), calc('-5+2', 'unary'), calc('3>2', 'cmp'), calc('', 'empty'),
        calc('expr', 'fromVar', 'var'), calc('$dot*4', 'dot'), calc('{{getvar::hp}}+1', 'parsed'), calc('1 + 2', 'spaced')])],
})
add('calculate-nan-infinity-undefined', {
    chat: { scriptstate: { ...raw, $word: 'abc', $inf: 'Infinity', $big: '1e400' } },
    triggers: [S([pick(0, 'one'), pick(3, 'pair'), pick(4, 'obj'), calc('1/0', 'inf'), calc('0/0', 'nan'), calc('$word+1', 'nanVar'),
        calc('$inf+1', 'infVar'), calc('$big', 'bigVar'), calc('$missing*2', 'missing'), calc('x+1', 'garbage'), calc('x|x', 'undef'),
        calc('x&x', 'undefAnd'), calc('$one+$pair', 'rawVars'), calc('$obj+1', 'objVar'), calc('one', 'rawExpr', 'var')])],
})
// `result.toString()` of an undefined result throws after the output name is parsed, and the `catch` parses it again.
add('calculate-undefined-parses-output-twice', { random: [0.1, 0.9, 0.1], triggers: [S([calc('x|x', `u${twice}`)])] })
// A substituted `$name` can leave a `$` before a digit, which the engine reads as a chat variable.
add('calculate-substituted-dollar-declines', { triggers: [S([calc('$$missing', 'out')])], native: 'unsupported-calc-variable', admitted: true })
add('calculate-global-variable-declines', { triggers: [S([calc('@g+1', 'out')])], native: 'unsupported-calc-variable', admitted: true })
add('calculate-inexact-power-declines', { triggers: [S([set('a', '1'), calc('3^40', 'out')])], native: 'unsupported-pow-inexact', admitted: true })

// Errors after earlier writes.
add('error-keeps-variables-and-note', {
    triggers: [S([set('kept', '1'), impersonate('user', 'lost'), e('v2SetAuthorNote', { value: 'kept note', valueType: 'value' }), extract('x', '[', '', '$1', 'a')])],
})

// `v2Tokenize` counts through the tokenizer, here the plugin's, in the middle of the run, which waits for the host.
const tokenize = (value, valueType = 'value', fields = {}, indent = 0) => out('v2Tokenize', { value, valueType, ...fields }, indent)
add('tokenize-value', { triggers: [S([tokenize('abcdefgh')])] })
add('tokenize-var-then-reads-the-count', {
    triggers: [S([set('t', 'a longer text to count'), tokenize('t', 'var'), set('after', '{{getvar::out}}')])],
})
add('tokenize-empty', { triggers: [S([tokenize('')])] })
add('tokenize-stored-number-var', { chat: { scriptstate: { $count: 7 } }, triggers: [S([tokenize('count', 'var')])] })
add('tokenize-cbs-output-name', {
    triggers: [S([tokenize('{{getvar::name}} is here', 'value', { outputVar: '{{getvar::name}}count' }), set('after', '{{getvar::Bobcount}}')])],
})
add('tokenize-into-a-local', {
    triggers: [S([local('l', 'x', 'value', 0), tokenize('abcdefghi', 'value', { outputVar: 'l' }), set('seen', '{{getvar::l}}')])],
})
add('tokenize-into-a-local-in-a-nested-run', {
    triggers: [S([local('l', 'outer', 'value', 0), run('sub'), set('outerSeen', '{{getvar::l}}')]),
        T('sub', 'manual', [local('l', 'x', 'value', 0), tokenize('abcdefghi', 'value', { outputVar: 'l' }), set('innerSeen', '{{getvar::l}}')])],
})
add('tokenize-in-a-loop', {
    triggers: [S([e('v2LoopNTimes', { value: '3', valueType: 'value' }), tokenize('{{getvar::acc}}abcd', 'value', {}, 1),
        set('acc', '{{getvar::acc}}{{getvar::out}}', '=', 'value', 1), end(1, true), set('after', 'done')])],
})
add('tokenize-in-a-nested-run', {
    triggers: [S([set('a', '1'), run('sub'), set('b', '{{getvar::out}}')]), T('sub', 'manual', [tokenize('nested text'), set('c', '2')])],
})
add('tokenize-then-impersonate-and-system-prompt', {
    triggers: [S([tokenize('first'), impersonate('user', 'hi'), e('v2SystemPrompt', { location: 'start', value: 'sys {{getvar::out}}', valueType: 'value' }),
        tokenize('second', 'value', { outputVar: 'out2' }), set('roles', '{{history::role}}')])],
})
add('tokenize-then-error-keeps-variables', {
    triggers: [S([set('kept', '1'), tokenize('abcd'), impersonate('user', 'lost'), extract('x', '[', '', '$1', 'a')])],
})
add('tokenize-then-decline', { triggers: [S([set('a', '1'), tokenize('abcd'), calc('@g+1', 'res')])], native: 'unsupported-calc-variable', admitted: true })
add('tokenize-object-value-declines', {
    chat: { message: [msg('user', 'a', 'm0'), msg('char', { nested: 1 }, 'm1')] },
    triggers: [S([out('v2GetLastMessage', { outputVar: 'last' }), tokenize('last', 'var')])],
    native: 'unsupported-trigger-tokenize-value', admitted: true,
})

// `v2RunLLM` and `v2CheckSimilarity` with low-level access: the run waits for the model request or the search. The
// oracle's `requestChatData` gives each case's scripted `llm` replies and its `HypaProcesser` returns the added texts in
// reverse; the calls are recorded.
const low = { lowLevelAccess: true }
const llm = (value, fields = {}, indent = 0) => e('v2RunLLM', { value, valueType: 'value', model: 'model', outputVar: 'llm', ...fields }, indent)
const ok = (result) => ({ type: 'success', result })
add('llm-value', { character: low, triggers: [S([set('a', '1'), llm('hello {{getvar::name}}'), set('after', '[{{getvar::llm}}]')])], llm: [ok('reply one')] })
add('llm-var-value', { character: low, triggers: [S([set('q', 'from a var'), llm('q', { valueType: 'var' })])], llm: [ok('r')] })
add('llm-chat-ml', {
    character: low,
    chat: { scriptstate: { $prompt: ' <|im_start|>system\nBe {{getvar::name}}<|im_end|><|im_start|>user<|im_sep|>Hi {{char}}<Thoughts>think\nmore</Thoughts> there<|im_end|><|im_start|>assistant ok<|im_start|>other words ' } },
    triggers: [S([llm('prompt', { valueType: 'var' })])], llm: [ok('x')],
})
add('llm-plain-text-is-not-parsed-again', {
    character: low, chat: { scriptstate: { $raw: 'plain {{char}} {{getvar::name}}' } }, triggers: [S([llm('raw', { valueType: 'var' })])], llm: [ok('x')],
})
add('llm-submodel-streaming', { character: low, triggers: [S([llm('q', { model: 'submodel', streaming: true })])], llm: [{ type: 'streaming', chunks: ['par', 'partial reply'] }] })
add('llm-null-streaming', { character: low, triggers: [S([llm('q', { streaming: null })])], llm: [ok('r')] })
add('llm-fail-stores-null', { character: low, triggers: [S([llm('q'), set('after', '{{getvar::llm}}')])], llm: [{ type: 'fail', result: 'boom' }] })
add('llm-multiline-stores-null', { character: low, triggers: [S([llm('q')])], llm: [{ type: 'multiline', result: [['char', 'a'], ['char', 'b']] }] })
add('llm-multiline-one-pair-stores-null', { character: low, triggers: [S([llm('q')])], llm: [{ type: 'multiline', result: [['char', 'only one']] }] })
add('llm-cbs-output-name', { character: low, triggers: [S([llm('q', { outputVar: '{{getvar::name}}llm' }), set('after', '{{getvar::Bobllm}}')])], llm: [ok('named')] })
add('llm-into-a-local', { character: low, triggers: [S([local('l', 'x'), llm('q', { outputVar: 'l' }), set('seen', '{{getvar::l}}')])], llm: [ok('kept local')] })
add('llm-twice-in-a-loop', {
    character: low,
    triggers: [S([e('v2LoopNTimes', { value: '2', valueType: 'value' }), llm('{{getvar::acc}}q', {}, 1),
        set('acc', '{{getvar::acc}}{{getvar::llm}}', '=', 'value', 1), end(1, true), set('after', 'done')])],
    llm: [ok('a'), ok('b')],
})
add('llm-in-a-nested-run', {
    character: low, triggers: [S([set('a', '1'), run('sub'), set('b', '{{getvar::llm}}')]), T('sub', 'manual', [llm('nested'), set('c', '2')])],
    llm: [ok('inner')],
})
add('llm-then-messages-and-system-prompt', {
    character: low,
    triggers: [S([impersonate('user', 'before'), llm('q'), impersonate('char', '{{getvar::llm}}'),
        e('v2SystemPrompt', { location: 'start', value: 'sys {{getvar::llm}}', valueType: 'value' })])],
    llm: [ok('said')],
})
add('llm-throw-keeps-earlier-writes', { character: low, triggers: [S([set('a', '1'), impersonate('user', 'lost'), llm('q'), set('b', '2')])], llm: [{ type: 'throw', message: 'network down' }] })
add('llm-object-value-declines', {
    character: low, chat: { message: [msg('user', 'a', 'm0'), msg('char', { nested: 1 }, 'm1')] },
    triggers: [S([out('v2GetLastMessage', { outputVar: 'last' }), llm('last', { valueType: 'var' })])],
    native: 'unsupported-trigger-llm-value', admitted: true,
})
const similar = (source, value, fields = {}, indent = 0) =>
    e('v2CheckSimilarity', { source, sourceType: 'value', value, valueType: 'value', outputVar: 'sim', ...fields }, indent)
add('similarity-values', { character: low, triggers: [S([set('a', '1'), similar('a {{getvar::name}}', 'one§two§{{getvar::hp}}'), set('after', '[{{getvar::sim}}]')])] })
add('similarity-vars', {
    character: low,
    triggers: [S([set('src', 'query'), set('docs', 'x§y'), similar('src', 'docs', { sourceType: 'var', valueType: 'var' })])],
})
add('similarity-one-text', { character: low, triggers: [S([similar('q', 'single')])] })
add('similarity-empty-value', { character: low, triggers: [S([similar('q', ''), set('after', '[{{getvar::sim}}]')])] })
add('similarity-cbs-output-name', { character: low, triggers: [S([similar('q', 'a§b', { outputVar: '{{getvar::name}}sim' }), set('after', '{{getvar::Bobsim}}')])] })
add('similarity-then-llm', { character: low, triggers: [S([similar('q', 'a§b'), llm('{{getvar::sim}}')])], llm: [ok('after search')] })
add('similarity-object-value-declines', {
    character: low, chat: { message: [msg('user', 'a', 'm0'), msg('char', { nested: 1 }, 'm1')] },
    triggers: [S([out('v2GetLastMessage', { outputVar: 'last' }), similar('q', 'last', { valueType: 'var' })])],
    native: 'unsupported-trigger-similarity-value', admitted: true,
})

// v1 `runLLM` and `checkSimilarity` with low-level access: every value is parsed, `inputVar` names the variable as
// written, `runLLM` asks the `model` mode without streaming and stores `'Error: ' + result.result` for anything but a
// success, and `checkSimilarity` parses `value` before `source`.
const llm1 = (value, inputVar = 'llm') => v1('runLLM', { value, inputVar })
const similar1 = (source, value, inputVar = 'sim') => v1('checkSimilarity', { source, value, inputVar })
add('v1-llm-value', { character: low, triggers: [S([set('a', '1'), llm1('hello {{getvar::name}}'), set('after', '[{{getvar::llm}}]')])], llm: [ok('reply one')] })
add('v1-llm-chat-ml', { character: low, triggers: [S([llm1('<|im_start|>system\nBe {{getvar::name}}<|im_end|><|im_start|>user\nHi {{char}}<|im_end|>')])], llm: [ok('x')] })
add('v1-llm-input-var-as-written', { character: low, triggers: [S([llm1('q', '{{getvar::name}}llm'), set('after', '[{{getvar::Bobllm}}]')])], llm: [ok('named')] })
add('v1-llm-fail', { character: low, triggers: [S([llm1('q'), set('after', '{{getvar::llm}}')])], llm: [{ type: 'fail', result: 'boom' }] })
add('v1-llm-streaming', { character: low, triggers: [S([llm1('q')])], llm: [{ type: 'streaming', chunks: ['par', 'partial reply'] }] })
add('v1-llm-multiline', { character: low, triggers: [S([llm1('q')])], llm: [{ type: 'multiline', result: [['char', 'a, b'], ['user', 'c']] }] })
add('v1-llm-multiline-one-pair', { character: low, triggers: [S([llm1('q')])], llm: [{ type: 'multiline', result: [['user', 'only one']] }] })
add('v1-llm-multiline-empty', { character: low, triggers: [S([llm1('q')])], llm: [{ type: 'multiline', result: [] }] })
add('v1-llm-throw-keeps-earlier-writes', { character: low, triggers: [S([set('a', '1'), llm1('{{setvar::b::2}}q'), set('c', '3')])], llm: [{ type: 'throw', message: 'network down' }] })
add('v1-llm-in-a-v2-loop', {
    character: low,
    triggers: [S([e('v2LoopNTimes', { value: '2', valueType: 'value' }), { ...llm1('{{getvar::acc}}q'), indent: 1 },
        set('acc', '{{getvar::acc}}{{getvar::llm}}', '=', 'value', 1), end(1, true)])],
    llm: [ok('a'), ok('b')],
})
add('v1-similarity-values', { character: low, triggers: [S([set('a', '1'), similar1('a {{getvar::name}}', 'one§two§{{getvar::hp}}'), set('after', '[{{getvar::sim}}]')])] })
// `Math.random` draws in parse order: the first parsed of `value` and `source` takes 0.1.
add('v1-similarity-parses-value-first', { character: low, random: [0.1, 0.9], triggers: [S([similar1('q {{random::s1::s2}}', '{{random::v1::v2}}§two')])] })
add('v2-similarity-parses-source-first', { character: low, random: [0.1, 0.9], triggers: [S([similar('q {{random::s1::s2}}', '{{random::v1::v2}}§two')])] })
add('v1-similarity-input-var-as-written', { character: low, triggers: [S([similar1('q', 'a§b', '{{getvar::name}}sim')])] })
add('v1-similarity-then-llm', { character: low, triggers: [S([similar1('q', 'a§b'), llm1('{{getvar::sim}}')])], llm: [ok('after search')] })

// Declines: the native runner refuses before any effect. Unless a case is `admitted`, the refusal is admission's, which
// an owner can ask for before its own first request.
add('decline-v1-command', { triggers: [S([v1('command', { value: '/pass x' })])], native: 'unsupported-trigger-effect-command' })
add('decline-v1-image-low-level', { character: { lowLevelAccess: true }, triggers: [S([v1('runImgGen', { value: 'q', negValue: '', inputVar: 'img' })])], native: 'unsupported-trigger-effect-runImgGen' })
add('decline-lua', { triggers: [T('lua', 'output', [{ type: 'triggerlua', code: 'x' }])], native: 'unsupported-trigger-effect-triggerlua' })
add('decline-split-regex', { triggers: [S([split('a1b2c', '/\\d/', 'regex', 'out')])], native: 'unsupported-trigger-split-regex' })
add('decline-wait', { triggers: [S([e('v2Wait', { value: '0', valueType: 'value' })])], native: 'unsupported-trigger-effect-v2Wait' })
add('decline-update-gui', { triggers: [S([e('v2UpdateGUI')])], native: 'unsupported-trigger-effect-v2UpdateGUI' })
add('decline-command', { triggers: [S([e('v2Command', { value: '/pass x', valueType: 'value' })])], native: 'unsupported-trigger-effect-v2Command' })
add('decline-show-alert', { triggers: [S([e('v2ShowAlert', { value: 'hi', valueType: 'value' })])], native: 'unsupported-trigger-effect-v2ShowAlert' })
add('decline-lorebook-count', { triggers: [S([out('v2GetLorebookCountNew')])], native: 'unsupported-trigger-effect-v2GetLorebookCountNew' })
add('decline-set-character-desc', { triggers: [S([e('v2SetCharacterDesc', { value: 'new', valueType: 'value' })])], native: 'unsupported-trigger-effect-v2SetCharacterDesc' })
add('decline-get-character-desc', { triggers: [S([out('v2GetCharacterDesc')])], native: 'unsupported-trigger-effect-v2GetCharacterDesc' })
add('decline-display-state-outside-display', { triggers: [S([set('a', '1'), out('v2GetDisplayState')])], native: 'unsupported-trigger-early-return' })
add('decline-nested-unsupported', { triggers: [S([run('sub')]), T('sub', 'manual', [e('v2UpdateGUI')])], native: 'unsupported-trigger-effect-v2UpdateGUI' })
add('unreachable-unsupported-ignored', { triggers: [S([set('a', '1')]), T('other', 'manual', [e('v2UpdateGUI')])] })
add('decline-skipped-branch-still-declines', { triggers: [S([iff('hp', '=', '0'), e('v2UpdateGUI', {}, 1), end(1)])], native: 'unsupported-trigger-effect-v2UpdateGUI' })
add('decline-display-mode', { mode: 'display', triggers: [T('d', 'display', [set('a', '1')])], native: 'unsupported-trigger-display-mode' })
add('decline-request-mode', { mode: 'request', triggers: [T('r', 'request', [set('a', '1')])], native: 'unsupported-trigger-display-mode' })
add('decline-indent-not-integer', { triggers: [S([iff('hp', '=', '10'), set('x', '1', '=', 'value', 1.5), end(1)])], native: 'unsupported-trigger-indent' })
add('ifadv-approx-case', { triggers: [S([adv('value', 'ABC', '≒', 'value', 'abc'), set('pass', '1', '=', 'value', 1), end(1)])] })

// Admission for each `runTrigger` caller's mode: it compiles what that mode reaches, through `v2RunTrigger` and module
// triggers included, and not the triggers of other modes.
const declining = (indent = 0) => e('v2UpdateGUI', {}, indent)
add('admit-output-skips-a-start-decline', { mode: 'output', triggers: [S([declining()]), T('o', 'output', [set('ran', '1')])] })
add('admit-input-skips-an-output-decline', { mode: 'input', triggers: [T('o', 'output', [declining()]), T('i', 'input', [set('ran', '1')])] })
add('admit-manual-skips-a-mode-decline', {
    mode: 'manual', manualName: 'go', triggers: [S([declining()]), T('go', 'manual', [set('ran', '1')])],
})
add('admit-output-declines-a-module-trigger', {
    mode: 'output', triggers: [T('o', 'output', [set('ran', '1')])],
    root: { modules: [module('m1', [T('mo', 'output', [declining()])])], enabledModules: ['m1'] },
    native: 'unsupported-trigger-effect-v2UpdateGUI',
})
// TS never makes the call, but admission compiles the trigger it names.
add('admit-output-declines-an-unrun-call', {
    mode: 'output', triggers: [T('o', 'output', [iff('hp', '=', '0'), run('sub', 1), end(1), set('ran', '1')]), T('sub', 'manual', [declining()])],
    native: 'unsupported-trigger-effect-v2UpdateGUI',
})
// The output owner admits before its provider request, so a decline while running comes after that request.
add('admit-output-then-declines-while-running', {
    mode: 'output', triggers: [T('o', 'output', [set('a', '1'), calc('$$missing', 'out')])], native: 'unsupported-calc-variable', admitted: true,
})

// Conversation records admission declines whenever a trigger runs. A `message` that is not an array has no case: an
// active session refuses that conversation before any trigger runs.
add('admit-declines-a-message-without-a-text-role', {
    chat: { message: [msg('user', 'a', 'm0'), msg(5, 'numeric role', 'm1')] }, triggers: [S([set('a', '1')])],
    native: 'unsupported-trigger-conversation-shape',
})
add('admit-declines-scriptstate-not-an-object', { chat: { scriptstate: 'state' }, triggers: [S([set('a', '1')])], native: 'unsupported-trigger-conversation-shape' })
// The greeting records `v2GetFirstMessage` cannot read decline in admission when a reachable trigger has one.
add('admit-declines-greetings-not-an-array', {
    character: { alternateGreetings: 'Alt' }, chat: { fmIndex: 0 }, triggers: [S([set('a', '1'), out('v2GetFirstMessage')])],
    native: 'unsupported-trigger-character-shape',
})
add('admit-declines-an-object-greeting-index', {
    chat: { fmIndex: {} }, triggers: [S([set('a', '1'), out('v2GetFirstMessage')])], native: 'unsupported-trigger-conversation-shape',
})
add('first-message-default-index-skips-the-greetings', { character: { alternateGreetings: 'Alt' }, triggers: [S([out('v2GetFirstMessage')])] })
add('greetings-unread-without-first-message', { character: { alternateGreetings: 'Alt' }, chat: { fmIndex: {} }, triggers: [S([set('a', '1')])] })
// Declines the run meets after admission passed, with the writes before them dropped.
add('running-declines-a-made-array-name-from-cbs', {
    chat: { scriptstate: { $n: '[x]' } }, triggers: [S([set('a', '1'), e('v2MakeArrayVar', { var: '{{getvar::n}}' })])],
    native: 'unsupported-trigger-early-return', admitted: true,
})
add('running-declines-a-prototype-local', { triggers: [S([set('a', '1'), local('constructor', 'x')])], native: 'unsupported-trigger-local-key', admitted: true })
add('running-declines-a-dict-write-on-an-array', {
    triggers: [S([set('a', '1'), dict('v2SetDictVar', { var: 'arr', key: 'k', keyType: 'value', value: 'v', valueType: 'value' })])],
    native: 'unsupported-trigger-json-shape', admitted: true,
})
// A stored object reads as its string, so CBS sees text.
add('stored-object-variable-reads-as-text', { chat: { scriptstate: { $x: { a: 1 } } }, triggers: [S([set('shown', '[{{getvar::x}}]')])] })

// Variable writes an earlier stage committed before the run (`earlier`, through the session as the current-chat pass
// commits its writes): the run reads them as stored values, which the native runner takes through `TriggerRunner::apply`.
add('earlier-write-read-by-a-condition', { earlier: [['gate', 'open']], triggers: guarded([varCond('gate', '=', 'open')]) })
add('earlier-write-replaces-a-stored-value', { earlier: [['hp', '3'], ['fresh', '4']], triggers: [S([set('copy', '{{getvar::hp}}-{{getvar::fresh}}')])] })
add('earlier-write-equal-to-a-later-set-is-unchanged', { earlier: [['a', 'x']], triggers: [S([set('a', 'x'), set('b', 'a', '=', 'var')])] })
add('earlier-number-write-reads-as-text', { earlier: [['n', 5]], triggers: [S([set('m', '[{{getvar::n}}]'), set('n', '5')])] })

// Chat edits an earlier Lua run left in the stored chat before the run (`earlierEdits`: `removeChat`, `insertChat`,
// `setChatRole`, `setFullChatMain`): the run's CBS reads them through its processor and its message effects through its
// input, both of which `TriggerRunner::apply` gives them.
const newId = n => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`
const chatReads = () => [set('h', '{{history}}'), set('roles', '{{history::role}}'), set('lm', '{{lastmessage}}|{{lastmessageid}}'),
    set('log', '{{previouschatlog::1}}|{{userhistory}}'), out('v2GetMessageCount', { outputVar: 'count' }),
    out('v2GetLastMessage', { outputVar: 'last' }), out('v2GetMessageAtIndex', { index: '1', indexType: 'value', outputVar: 'second' }),
    out('v2GetLastUserMessage', { outputVar: 'lastUser' })]
add('earlier-remove-chat', { earlierEdits: [{ remove: 1 }, { remove: -1.5 }, { remove: 9 }], triggers: [S(chatReads())] })
add('earlier-insert-chat', {
    earlierEdits: [{ insert: [1, 'char', 'inserted {{getvar::name}}', newId(1)] }, { insert: [-1, 'user', 'before last', newId(2)] },
        { insert: [99, 'char', 'at the end', newId(3)] }, { insert: [-99, 'user', 'at the start', newId(4)] }],
    triggers: [S(chatReads())],
})
add('earlier-set-chat-role', {
    earlierEdits: [{ setRole: [-2.7, 'char'] }, { setRole: [0, 'char'] }, { setRole: [9, 'user'] }, { setRole: [-9, 'user'] }],
    triggers: [S(chatReads())],
})
add('earlier-set-full-chat', {
    earlierEdits: [{ replace: [msg('user', 'r0', 'm2'), { role: 'char', data: 'r1 {{getvar::name}}', chatId: 'm0', time: 5, saying: 'kept' },
        msg('char', 'r2', newId(5))] }],
    triggers: [S(chatReads())],
})
add('earlier-set-full-chat-then-edits', {
    earlierEdits: [{ replace: [msg('char', 'only', newId(6))] }, { insert: [0, 'user', 'first', newId(7)] }, { setRole: [1, 'user'] },
        { insert: [1, 'char', 'middle', newId(8)] }, { remove: 0 }],
    triggers: [S(chatReads())],
})
add('earlier-set-full-chat-empty', {
    earlierEdits: [{ replace: [] }],
    triggers: [S([set('lm', '[{{lastmessage}}|{{lastmessageid}}]'), out('v2GetMessageCount', { outputVar: 'count' })])],
})

const file = { schema: 1, base, cases }
const casesPath = fileURLToPath(new URL('../../tests/fixtures/rust-core/triggers/run.cases.json', import.meta.url))
writeFileSync(casesPath, `${JSON.stringify(file, null, 1)}\n`)
console.log(`${cases.length} cases`)

// The route contract (route-declines.cases.json): triggers TS runs that the native Send route declines, since it answers
// no model request, acts on neither `stopSending` nor `sendAIprompt` yet, and reproduces only the case tables it has.
// No TS runs for these. `route` turns the owner's switches off (`llm`, `stopSending`, `sendAiPrompt`) or names the
// engine's probed case tables (`caseTables`: 'foreign' or 'unicode16'); with them on, every case runs to the end. A case
// without `native` stays admitted with its switch off. `admitted` marks a decline the run meets while running.
const routeCases = []
function route(id, spec) {
    const entry = { id, mode: spec.mode ?? 'start', route: spec.route }
    for (const key of ['root', 'chat', 'native', 'admitted']) {
        if (spec[key] !== undefined) entry[key] = spec[key]
    }
    const character = { ...(spec.character ?? {}) }
    if (spec.triggers) character.triggerscript = spec.triggers
    if (Object.keys(character).length) entry.character = character
    routeCases.push(entry)
}
// The count the run would request first, so a decline after it would follow a host request.
const countFirst = () => tokenize('abcd')
const noLlm = { llm: false }
route('route-llm-v2', { character: low, triggers: [S([set('a', '1'), countFirst(), llm('q')])], route: noLlm, native: 'unsupported-trigger-effect-v2RunLLM' })
route('route-llm-v1', { character: low, triggers: [S([set('a', '1'), countFirst(), llm1('q')])], route: noLlm, native: 'unsupported-trigger-effect-runLLM' })
route('route-llm-in-a-called-trigger', {
    character: low, triggers: [S([set('a', '1'), run('sub')]), T('sub', 'manual', [llm('q')])], route: noLlm,
    native: 'unsupported-trigger-effect-v2RunLLM',
})
route('route-llm-in-a-module', {
    triggers: [S([set('a', '1')])], root: { modules: [module('m1', [S([llm1('q')])], low)], enabledModules: ['m1'] }, route: noLlm,
    native: 'unsupported-trigger-effect-runLLM',
})
route('route-llm-without-low-level-is-a-no-op', { triggers: [S([llm('q'), llm1('q'), set('a', '1')])], route: noLlm })
const noStop = { stopSending: false }
route('route-stop-v2', { triggers: [S([set('a', '1'), countFirst(), e('v2StopPromptSending')])], route: noStop, native: 'unsupported-trigger-stop-sending' })
route('route-stop-v1', { triggers: [S([set('a', '1'), v1('stop', {})])], route: noStop, native: 'unsupported-trigger-stop-sending' })
route('route-stop-in-a-called-trigger', {
    triggers: [S([set('a', '1'), run('sub')]), T('sub', 'manual', [e('v2StopPromptSending')])], route: noStop,
    native: 'unsupported-trigger-stop-sending',
})
route('route-stop-behind-a-false-condition', {
    triggers: [S([iff('hp', '=', '0'), e('v2StopPromptSending', {}, 1), end(1), set('a', '1')])], route: noStop,
    native: 'unsupported-trigger-stop-sending',
})
const noSend = { sendAiPrompt: false }
route('route-send-ai-v2', {
    mode: 'output', character: low, triggers: [T('o', 'output', [set('a', '1'), e('v2SendAIprompt')])], route: noSend,
    native: 'unsupported-trigger-send-ai-prompt',
})
route('route-send-ai-v1', {
    mode: 'output', character: low, triggers: [T('o', 'output', [set('a', '1'), v1('sendAIprompt', {})])], route: noSend,
    native: 'unsupported-trigger-send-ai-prompt',
})
route('route-send-ai-without-low-level-is-a-no-op', {
    mode: 'output', triggers: [T('o', 'output', [e('v2SendAIprompt'), v1('sendAIprompt', {}), set('a', '1')])], route: noSend,
})
// Case mapping: text the probed tables could map other than Rust's declines where the run maps it.
const foreign = { caseTables: 'foreign' }
const unicode16 = { caseTables: 'unicode16' }
route('route-case-foreign-upper', {
    triggers: [S([set('a', '1'), src('v2ToUpperCase', 'àb')])], route: foreign, native: 'unsupported-trigger-case-table', admitted: true,
})
route('route-case-foreign-ascii', { triggers: [S([src('v2ToUpperCase', 'ab'), src('v2ToLowerCase', 'AB', { outputVar: 'o2' })])], route: foreign })
route('route-case-foreign-approximate', {
    triggers: [S([set('a', '1'), adv('value', 'Àb', '≒', 'value', 'à B'), set('pass', '1', '=', 'value', 1), end(1)])], route: foreign,
    native: 'unsupported-trigger-case-table', admitted: true,
})
route('route-case-foreign-loose-search', {
    triggers: [S([set('a', '1'), search('É', 'loose', '2')])], route: foreign, native: 'unsupported-trigger-case-table', admitted: true,
})
// U+A7CE maps in Unicode 17 and not in the Unicode 16 tables.
route('route-case-unicode16-differs', {
    triggers: [S([set('a', '1'), src('v2ToLowerCase', 'x꟎')])], route: unicode16, native: 'unsupported-trigger-case-table', admitted: true,
})
route('route-case-unicode16-alike', { triggers: [S([src('v2ToLowerCase', 'ÀBÇ ΟΔΟΣ'), src('v2ToUpperCase', 'straße', { outputVar: 'o2' })])], route: unicode16 })

const routeFile = { schema: 1, base, cases: routeCases }
const routePath = fileURLToPath(new URL('../../tests/fixtures/rust-core/triggers/route-declines.cases.json', import.meta.url))
writeFileSync(routePath, `${JSON.stringify(routeFile, null, 1)}\n`)
console.log(`${routeCases.length} route cases`)
