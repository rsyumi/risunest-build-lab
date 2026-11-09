// Case definitions for the CBS stream-engine oracle fixtures. `run.mjs generate` evaluates every
// case with the product risuChatParser and writes <set>.json beside this file; the crate tests and
// `run.mjs verify` replay those files.
//
// A case is {id, text, patch?, seed?, visualize?, refusal?, hash?, steps?, expand?}:
// - patch overrides chat variables, seed overrides the LCG seed, visualize is risuChatParser's flag;
// - refusal names the reason the native engine must refuse with (the case is a host-read control);
// - hash stores the output as a SHA-256 of its UTF-16LE units instead of the text;
// - steps chains checkpoints: each step replaces a literal in the previous output and parses it
//   again with the RNG state carried over;
// - expand builds `{{op::<code points from..to>}}` (surrogates skipped) instead of a text.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const ledger = JSON.parse(readFileSync(new URL('../../../tests/fixtures/rust-core/cbs/ledger.json', import.meta.url), 'utf8'));

// Date.now() during evaluation, so the clock families record the same output on every generation.
export const fixtureClock = 1767225600000;

export const context = {
  variables: {
    enabled: '1', off: '0', score: '7', name: 'equal', sep: '::', list: '["x",1.0,null]', next: '{{getvar::score}}',
    surrogate: '\ud800', close: '}}', open: '{{', expr: '#if 1', each: '#each [1,2] as x', when: '#when::var::enabled',
    fn: 'f', dict: '{"a":1,"b":{"c":[1,2]}}', arr: '[1,2,3]', empty: '', slash: '/if', truthy: 'true',
  },
  globals: { toggle_demo: '1', toggle_off: '0', toggle_text: 'yes' },
  user: 'Synthetic',
  index: 7,
  seed: 12345,
  functions: { syntheticCall: '{{random::a::b}}', shared: '[{{arg::1}}|{{arg::2}}]' },
  clock: fixtureClock,
};

const cases = () => {
  const list = [];
  const seen = new Set();
  const add = (id, text, options = {}) => {
    if (seen.has(id)) throw new Error('Duplicate fixture case ' + id);
    seen.add(id);
    list.push({ id, text, ...options });
  };
  return { list, add };
};

function grammar() {
  const { list, add } = cases();
  const texts = (prefix, values) => values.forEach((text, i) => add(prefix + '-' + i, text));

  texts('literal', ['', 'plain', '{', '}', '#', '{}', '{x}', '}}', '{{', '#}', '{#', '{{}}', '{{ }}', '{{:}}', '{{::}}',
    'a{b}c', '{{{x}}}', '{{{{x}}}}', '}}{{', '{{blank}}}}', '{{{{blank}}', 'x#}y', '{#x#}', '{#x}}', '{{x#}}', '{{#}}',
    '{{#}', '{#}}', '{{}', '{}}', '#}}', '{{blank}#}', '{{blank#}}', 'a}}b{{blank}}c}}', '\ud800{{blank}}\udc00',
    '{{\n}}', '{{blank\n}}', '{{ blank }}', '{{\tblank}}', '{{BLANK}}', '{{b_l-a n\nk}}', '{{blank:x}}', '{{blank::x}}']);
  texts('angle', ['<user>', '<USER>', '<UsEr>', '<user', 'user>', '<<user>>', '<users>', '{{<user>}}', '<user>{{blank}}',
    '<İser>', '<user >', 'a<user>b<user>c', '<user><user>']);
  for (const [i, text] of ['<char>', '<Char>', '<bot>', '<BOT>', '{{<bot>}}'].entries()) add('angle-host-' + i, text, { refusal: 'unsupported-operator' });
  texts('nested', ['{{length::{{length::abc}}}}', '{{{{getvar::name}}::a::a}}', '{{equal{{getvar::sep}}a{{getvar::sep}}a}}',
    '{{{{getvar::open}}}}', '{{getvar::open}}blank{{getvar::close}}', '{{length::{{getvar::close}}}}', '{{l{{blank}}ength::abc}}',
    '{{length::a}}{{length::b}}', '{{length::{{#if 1}}abc{{/if}}}}', '{{{{blank}}}}', '{{{{getvar::empty}}}}']);

  texts('if', ['{{#if 1}}a{{/if}}', '{{#if true}}a{{/if}}', '{{#if 0}}a{{/if}}', '{{#if}}a{{/if}}', '{{#if  1}}a{{/if}}',
    '{{#if 1 2}}a{{/if}}', '{{#iffy 1}}a{{/if}}', '{{#if TRUE}}a{{/if}}', '{{#if 01}}a{{/if}}', '{{#if 1}}  a  \n   b \n{{/if}}',
    '{{#if 1}}\n\n a\n\n{{/if}}', '{{#if 1}}{{#if 0}}x{{/if}}y{{/if}}', '{{#if 1}}{{#if 1}}x{{/if}}y{{/if}}', '{{#if 0}}{{#if 1}}x{{/if}}y{{/if}}z',
    '{{#if 1}}a{{/}}', '{{#if 1}}a{{/x}}', '{{#if 1}}a{{//}}', '{{#if 1}}a', '{{#if 0}}a', '{{/if}}', '{{/}}', 'a{{/if}}b',
    '{{#if {{getvar::enabled}}}}a{{/if}}', '{{#if {{getvar::off}}}}a{{/if}}', '{{{{getvar::expr}}}}x{{/if}}', '{{#if 1}}x{{{{getvar::slash}}}}',
    '{{#if 1}}x{{/{{blank}}}}', '{{#if 1}}{{:else}}{{/if}}', '{{#if_pure 1}}  a {{length::x}} \n b {{/if}}', '{{#if_pure 0}}a{{/if}}',
    '{{#if_pure  1}}a{{/if}}', '{{#if_pure1}}a{{/if}}', '{{#if_pure true}}{{#if 0}}x{{/if}}{{/if}}', '{{#if 1}}{{#if_pure 1}} p {{/if}}{{/if}}',
    '{{#if 0}}{{#when 1}}{{random::a::b}}{{/when}}{{/if}}{{random::a::b}}', '{{#if 0}}{{random}}{{/if}}{{random}}']);

  const whenHeads = ['#when 1', '#when 0', '#when true', '#when', '#whenx', '#when  1', '#when::1', '#when::0', '#when::', '#when::true',
    '#when::not::0', '#when::not::1', '#when::not', '#when::a::is::a', '#when::a::is::b', '#when::is::a', '#when::a::isnot::b', '#when::isnot::a',
    '#when::1::and::1', '#when::1::and::0', '#when::and::1', '#when::0::or::1', '#when::or::0', '#when::var::enabled', '#when::var::off',
    '#when::var::missing', '#when::var', '#when::toggle::demo', '#when::toggle::off', '#when::toggle::missing', '#when::toggle',
    '#when::score::vis::7', '#when::score::vis::8', '#when::vis::null', '#when::score::visnot::7', '#when::score::visnot::8', '#when::visnot::x',
    '#when::demo::tis::1', '#when::demo::tis::0', '#when::tis::null', '#when::text::tisnot::yes', '#when::text::tisnot::no', '#when::tisnot::x',
    '#when::7::>::2', '#when::2::>::7', '#when::7::<::2', '#when::2::<::7', '#when::7::>=::7', '#when::7::<=::6', '#when::>::1', '#when::<::1',
    '#when::7a::>::2', '#when::a::>::2', '#when:: 7::>::2', '#when::0x10::>::2', '#when::1e3::>=::1000', '#when::-1::<::-0.5',
    '#when::keep::1', '#when::keep::0', '#when::legacy::1', '#when::legacy::0', '#when::keep::legacy::1', '#when::legacy::keep::1',
    '#when::1::and::not::0', '#when::not::1::and::1', '#when::1::foo::1', '#when::1::foo::0', '#when::a::b::c::d', '#when:::1',
    '#when::var::{{getvar::name}}', '#when::{{getvar::score}}::is::7', '#when::keep::var::enabled', '#when::legacy::toggle::demo'];
  const whenBodies = ['A{{:else}}B', 'A', '\n  A\n{{:else}}\n  B\n', '\n\n A \n\n', '  {{:else}}  \nB', 'A\n {{:else}} \nB\n{{:else}}\nC',
    '\n{{:else}}\n', 'A{{:else}}B{{:else}}C', '{{random::a::b}}{{:else}}{{random::c::d}}'];
  whenHeads.forEach((head, i) => whenBodies.forEach((body, j) => add(`when-${i}-${j}`, `{{${head}}}${body}{{/when}}`)));
  texts('when-misc', ['{{:else}}', 'a{{:else}}b', '{{#when 1}}{{#when 0}}x{{:else}}y{{/when}}{{:else}}z{{/when}}',
    '{{#when 0}}{{#when 1}}x{{:else}}y{{/when}}{{:else}}z{{/when}}', '{{#when::keep::1}}\n x \n{{/when}}', '{{#when 1}}x', '{{#when 0}}x',
    '{{#when 1}}\n{{:else}}{{/when}}', '{{#when 0}}a{{:else}}{{settempvar::x::1}}b{{/when}}{{gettempvar::x}}', '{{{{getvar::when}}}}a{{/when}}']);

  const eachHeads = ['#each [1,2,3] as x', '#each [1,2,3] x', '#each [1,2,3]', '#each ["a","b"] as  x ', '#each::keep [1,2] as x',
    '#each::keep  [1,2] as x', '#each ::keep [1,2] as x', '#each as [1,2] as x', '#each a§b§c as x', '#each [] as x', '#each [null,true,{"a":[1]},[2,"3"]] as x',
    '#each ["$&","$$","$`","$\'","$1"] as x', '#each [1] as x as y', '#each [1,2] as', '#each [1,2] as x y', '#each {{getvar::list}} as x',
    '#each [1,2] as {{getvar::fn}}', '#each [1,2]   as   x', '#each', '#each  ', '#eachx [1] as x', '#each [1.0,1e21,-0] as x'];
  const eachBodies = ['{{slot::x}}', ' {{slot::x}} ', '\n {{slot::x}}\n  -\n', '{{slot::y}}', '{{slot::x}}{{slot::x}}', '{{length::{{slot::x}}}}',
    '{{random::{{slot::x}}::b}}', '[{{slot::f}}]', '{{slot::x}}{{#if 1}}i{{/if}}{{:else}}', '{{#each [7,8] as y}}{{slot::x}}{{slot::y}}{{/each}}'];
  eachHeads.forEach((head, i) => eachBodies.forEach((body, j) => add(`each-${i}-${j}`, `{{${head}}}${body}{{/each}}`)));
  texts('each-misc', ['{{#each [1,2] as x}}{{slot::x}}{{/each}}tail{{length::abc}}', '{{#each [1,2] as x}}{{slot::x}}', '{{#each [1] as x}}{{slot::x}}{{/}}',
    '{{#if 0}}{{#each [1,2] as x}}{{slot::x}}{{/each}}{{/if}}', '{{#each [1,2] as x}}{{length::ab}}{{/each}}{{length::abc}}',
    '{{#each::keep [1,2] as x}}{{upper}}{{/each}}{{upper}}', '{{#each::keep [1,2,3] as x}}{{upper}}{{/each}}{{upper}}{{upper}}',
    '{{#each::keep [1,1,1,1,1,1,1,1,1,1] as x}}{{a}}{{/each}}{{a}}', '{{length::a}}{{#each::keep [1,2] as x}}{{length::a}}{{/each}}{{length::a}}',
    '{{#each [1,2] as x}}{{settempvar::t::{{slot::x}}}}{{/each}}{{tempvar::t}}', '{{#each ["{{length::abc}}"] as x}}{{slot::x}}{{/each}}']);

  texts('func', ['{{#func f}}  F  {{/func}}A{{call::f}}B', '{{#func f}}{{arg::0}}/{{arg::1}}{{/func}}{{call::f::X}}', '{{#func f}}{{arg::1}}{{/func}}{{call::f}}',
    '{{#func f}}{{arg::1}}{{/func}}{{call::f::}}', '{{#func  ignored}} E {{/func}}[{{call::}}]', '{{#func f named}}{{arg::1}}/{{arg::named}}{{/func}}{{call::f::X}}',
    '{{call::missing::x}}', '{{#func f}}F{{/func}}{{CALL::f}}/{{c_a-ll:f}}', '{{#func \ud800}}F\udc00{{/func}}{{call::\ud800}}',
    '{{#func f}}{{arg::1}}{{/func}}{{call::{{getvar::fn}}::{{getvar::score}}}}', '{{#func f}}F{{/func}}{{{{blank}}call::f}}', '{{#func {{getvar::fn}}}}F{{/func}}{{call::f}}',
    '{{#func f}}{{#func g}}G{{/func}}{{call::g}}{{/func}}{{call::f}}/{{call::g}}', '{{#func g}}old{{/func}}{{#func f}}{{#func g}}new{{/func}}{{/func}}{{call::f}}{{call::g}}',
    '{{#func f}}[{{tempvar::t}}]{{settempvar::t::child}}{{tempvar::t}}{{/func}}{{settempvar::t::parent}}{{call::f}}/{{tempvar::t}}',
    '{{#func f}}R{{/func}}' + '{{call::f}}'.repeat(19) + '{{call::missing}}', '{{#func f}}R{{/func}}' + '{{call::f}}'.repeat(20) + '{{call::missing}}',
    '{{#func f}}R{{/func}}' + '{{call::f}}'.repeat(21), '{{#func f}}{{call::f}}{{/func}}{{call::f}}', '{{#func f}}x{{call::f}}{{/func}}{{call::f}}',
    '{{#func f}}before{{return::R}}after{{/func}}A{{call::f}}B', '{{#func f}}A{{arg::1}}B{{/func}}{{call::f::$$}}', '{{#func f}}A{{arg::1}}B{{/func}}{{call::f::$&}}',
    "{{#func f}}A{{arg::1}}B{{/func}}{{call::f::$'}}", '{{#func f}}A{{arg::1}}B{{/func}}{{call::f::$`}}', "{{#func f}}{{arg::1}}{{arg::2}}{{/func}}{{call::f::$'::Z}}",
    '{{call::syntheticCall}}{{call::syntheticCall}}', '{{#if 0}}{{call::syntheticCall}}{{/if}}{{random::a::b}}', '{{#pure}}{{call::syntheticCall}}{{/pure}}',
    '{{call::shared::a::b}}', '{{call::shared::a}}', '{{call::shared::<user>}}', '{{#func shared}}overridden{{/func}}{{call::shared}}',
    '{{#func f}}{{#if 1}}\n  A\n B  \n{{/if}}{{/func}}{{call::f}}', '{{#func f}}{{#when::keep::0}}\n  A\n{{:else}}\n B\n{{/when}}{{/func}}{{call::f}}',
    '{{#func}}x{{/func}}{{call::}}', '{{#func f}}x{{/func}}', '{{#func f}}x', '{{#function f}}y{{/function}}{{call::f}}', '{{#func f g h}}{{arg::2}}{{/func}}{{call::f::1::2}}',
    '{{#func f}}{{random::a::b}}{{/func}}{{#if 0}}{{call::f}}{{/if}}{{call::f}}{{random::a::b}}', '{{#func f}}{{settempvar::__force_return__::1}}x{{/func}}a{{call::f}}b',
    '{{#func f}}{{call::f}}{{/func}}{{#func g}}{{call::g}}{{call::g}}{{/func}}{{call::g}}{{call::f}}']);

  texts('pure', ['{{#pure}}{{length::abc}}{{/pure}}', '{{#pure}} a {{/pure}}', '{{#pure}}{{#if 1}}x{{/if}}{{/pure}}', '{{#pure}}{{:else}}{{/pure}}',
    '{{#pure}}{{#when 1}}a{{:else}}b{{/when}}{{/pure}}', '{{#pure}}{{#pure}}x{{/pure}}{{/pure}}', '{{#pure}}{{/x}}{{/pure}}', '{{#pure}}x',
    '{{#pure}}{{random}}{{/pure}}{{random}}', '{{#pure_display}}{{length::abc}}{{/pure_display}}', '{{#puredisplay}} {{a}}}} {{/puredisplay}}',
    '{{#pure_display}}{{#if 1}}x{{/if}}{{/}}', '{{#pure }}x{{/pure}}', '{{#PURE}}x{{/pure}}', '{{#pure}}{{/pure}}after', '{{#pure}}a{{#if 0}}b{{/pure}}c',
    '{{#code}}  a\\nb\\tc\\u0041\\x\\\\ \n\t d {{/code}}', '{{#code}}\\u00e9\\u{41}\\uZZZZ\\\r{{/code}}', '{{#code}}{{length::ab}}\\n{{/code}}', '{{#code}}\\\\u0041{{/code}}',
    '{{#code}}\\a\\b\\f\\v\\0\\"{{/code}}', '{{#code }}x{{/code}}', '{{#code}}\\ x{{/code}}', '{{#escape}} {a}(b) {{length::x}} {{/escape}}',
    '{{#escape::keep}} {a} {{/escape}}', '{{#escape ::keep }} {a} {{/escape}}', '{{#escape::keepx}} {a} {{/escape}}', '{{#escapex}} ( {{/escape}}',
    '{{#escape}}{{#if 1}}x{{/if}}{{/escape}}', '{{#if 1}}{{#escape}}{x}{{/escape}}{{/if}}']);

  texts('legacy', ['{#if 1\nx#}', '{#if 0\nx#}', '{#if -1\nx#}', '{#if\nx#}', '{#if  1\nx#}', '{#if \nx#}', '{#if 2\n  x  \n#}', '{#unknown\nx#}',
    '{#if 1 x#}', '{#if 1\n{{length::abc}}#}', '{#if 1\nx}}', '{#if 1\n{#if 0\ny#}z#}', '{{#if 1}}{#if 1\nx#}{{/if}}', '{{#pure}}{#if 1\nx#}{{/pure}}',
    '{#if 1\nx', '{#\n#}', '{##}', 'a{#if 1\n#}b']);

  texts('return', ['before{{return::R}}after', 'before{{return}}after', 'before{{return::}}after', '{{equal::{{return::R}}::x}}tail',
    '{{#if 1}}a{{return::R}}b{{/if}}c', '{{#when 0}}{{return::R}}{{:else}}B{{/when}}suffix', '{{settempvar::__force_return__::1}}x',
    '{{settempvar::__return__::V}}{{settempvar::__force_return__::1}}x', '{{settempvar::__force_return__::}}x', '{{settempvar::__force_return__}}x',
    '{{settempvar::x::1}}{{settempvar::x}}{{tempvar::x}}', '{{tempvar}}', '{{settempvar}}{{tempvar::undefined}}', '{{gettempvar::x}}{{settempvar::x::v}}{{gettempvar::x}}',
    '{{#pure}}{{return::R}}{{/pure}}after']);

  texts('editing', ['abc def{{bkspc}}', 'abc def  {{bkspc}}x', 'abc{{bkspc}}', '{{bkspc}}', 'a b c{{bkspc}}{{bkspc}}', 'a\tb{{bkspc}}', 'a\nb{{bkspc}}',
    'One. Two{{erase}}', 'One. Two.{{erase}}', 'One? Two! Three{{erase}}', 'No end{{erase}}', '{{erase}}', 'a\nb{{erase}}', 'x {{length::{{bkspc}}ab}}',
    'p q{{#if 1}}r s{{bkspc}}{{/if}}', 'p q{{#if 1}}{{bkspc}}{{/if}}', 'one. two{{#if 1}}{{erase}}{{/if}}', 'a b {{bkspc}}{{bkspc}}{{bkspc}}']);

  const depth = n => '{{length::'.repeat(n) + 'x' + '}}'.repeat(n);
  for (const n of [509, 510, 511, 512, 513]) add('depth-' + n, depth(n));
  for (const n of [254, 255, 256, 257]) add('block-depth-' + n, '{{#if 1}}'.repeat(n) + 'x' + '{{/if}}'.repeat(n));
  for (const n of [510, 511, 512]) add('brace-depth-' + n, '{{'.repeat(n) + 'blank' + '}}'.repeat(n));
  add('mixed-depth', '{{#if 1}}'.repeat(200) + depth(312) + '{{/if}}'.repeat(200));
  texts('unclosed', ['{{length::x', '{{#if 1}}x', '{{#pure}}x', '{{#each [1] as x}}{{slot::x}}', '{{a{{b', '{{#if 1}}{{length::x', '{{#if 1}}{{#pure}}{{a',
    '{{#when 1}}a{{:else}}b', '{{#func f}}x{{call::f}}', '{{#if 1}}}}', '{{#if 1}}{{/if}}}}', '{{#pure}}{{#if 1}}x', '{{#pure}}{{#if 1}}x{{/each}}', '{#if 1\n{{x']);
  texts('displaying-off', ['{{file::name::YWJj}}', '{{comment::note}}']);
  for (const [i, text] of ['{{file::name::YWJj}}', '{{comment::note}}', '{{comment}}', '{{file}}', '{{#if 1}}{{comment::<b>}}{{/if}}'].entries())
    add('displaying-' + i, text, { visualize: true });
  add('seed-zero', '{{random}}{{randint::1::100}}{{dice::2d6}}{{roll::d20}}', { seed: 0 });
  add('seed-max', '{{random}}{{randint::1::100}}{{dice::2d6}}{{roll::d20}}', { seed: 4294967295 });
  add('patched', '{{getvar::score}}{{#when::var::enabled}}on{{:else}}off{{/when}}', { patch: { score: 'patched', enabled: '0' } });

  add('checkpoint-generated', 'x', { steps: [['x', '{{getvar::next}}'], ['missing', ''], ['7', '{{length::abc}}']] });
  add('checkpoint-cross-boundary', '{{getvar::to', { steps: [['to', 'score}}']] });
  add('checkpoint-random', 'x y', { steps: [['x', '{{#when 0}}{{random::a::b}}{{:else}}b{{/when}}{{random::a::b}}'], ['y', '{{random::a::b}}'], ['missing', '']] });
  add('checkpoint-temp-reset', 'x y', { steps: [['x', '{{settempvar::x::1}}'], ['y', '{{gettempvar::x}}']] });
  add('checkpoint-escaped', 'x', { steps: [['x', '{{arrayelement::["\\u007b\\u007blength\\u003a\\u003aabc\\u007d\\u007d"]::0}}'], ['missing', '']] });
  add('checkpoint-each', 'x', { steps: [['x', '{{#each ["{{random}}","b"] as v}}[{{slot::v}}]{{/each}}'], ['missing', ''], ['[', '{{length::ab}}']] });
  add('checkpoint-pure', 'x', { steps: [['x', '{{#pure_display}}{{length::abc}}{{/pure_display}}'], ['\\', ''], ['missing', '']] });
  add('checkpoint-func', 'x', { steps: [['x', '{{#func f}}{{random}}{{/func}}{{call::f}}'], ['0', '{{call::syntheticCall}}']] });
  return list;
}

// Values that cross a whole family; numbers stay small so loops and repeats stay cheap.
const shapes = [[], [''], ['abc'], ['ABC', 'b'], ['aßİΣŉ'], ['a😀\ud800x\udc00'], ['[1,2,3]'], ['[1,2,3]', '1'], ['[1,2,3]', '5', 'x'],
  ['{"a":1,"b":[2]}', 'a'], ['{"a":0}', 'a', '2'], ['{"a":1}', 'b', '2'], ['3'], ['-1.5'], ['0x1f'], ['NaN'], ['1e400'], ['[[1],["a"],{"x":null}]', '0'],
  ['a§b§c'], ['1', '2', '3', '4'], ['true', 'false'], ['1', '1'], [' x '], ['a,b', ','], ['$&', 'x', 'y'], ['undefined'], ['Hello World. Bye!'],
  ['ab', 'cd'], ['["a","","a",1,1,null]', 'unique'], ['"str"', 'length'], ['5', 'a', 'x'], ['null', 'a', 'x'], ['[0,"x"]', '1'], ['{"a":"{\\"b\\":2}"}', 'a', 'b']];
// `range` loops until its end, so an infinite end would never return in the oracle.
const skip = (key, args) => key === 'range' && args.includes('1e400');

function families() {
  const { list, add } = cases();
  const served = new Set(['user', 'chatindex', 'getvar', 'getglobalvar', 'random', 'randint', 'dice', 'roll']);
  const runtime = ledger.registrations.filter(r => r.callback === 'function' && r.reachableKeys.length);
  const admitted = runtime.filter(r => r.status === 'native' || served.has(r.id));
  const refused = runtime.filter(r => !(r.status === 'native' || served.has(r.id)));
  const macro = (key, args, sep = '::') => '{{' + [key, ...args].join(sep) + '}}';
  for (const entry of admitted) {
    const primary = entry.reachableKeys[0];
    shapes.forEach((args, i) => { if (!skip(primary, args)) add(`${primary}-${i}`, macro(primary, args)) });
    add(`${primary}-colon`, macro(primary, ['abc', 'b'], ':'));
    add(`${primary}-spelling`, '{{ ' + primary.toUpperCase().split('').join('_') + ' ::abc::b}}');
    for (const key of entry.reachableKeys.slice(1)) for (const [i, args] of [[], ['abc'], ['[1,2,3]', '1']].entries()) add(`${key}-alias-${i}`, macro(key, args));
    for (const [i, block] of ['#if 0', '#if 1', '#when 0', '#pure'].entries()) add(`${primary}-block-${i}`, '{{' + block + '}}' + macro(primary, ['abc', 'b']) + '{{/}}');
  }
  for (const entry of refused) for (const key of entry.reachableKeys) add(`refused-${key}`, macro(key, ['x']), { refusal: 'unsupported-operator' });
  for (const key of ['slot', 'position', 'asset', 'img', '//', 'video-img', '__', 'unknownmacro', 'getvarx'])
    add(`preserved-${key}`, macro(key, ['x']));

  const targeted = {
    file: ['{{file::n::YWJj}}', '{{file::n::/w==}}', '{{file::n::7aCA}}', '{{file::n::8J+YgA}}', '{{file::n::_-8}}', '{{file::n::Y}}', '{{file::n::YW=Jj}}', '{{file::n}}'],
    xor: ['{{xor::abc}}', '{{xor}}', '{{xor::😀\ud800x}}', '{{xordecrypt::{{xor::😀\ud800x}}}}', '{{xordecrypt::!!!}}', '{{xordecrypt::}}', '{{xordecrypt}}',
      '{{xordecrypt::EBEQ}}', '{{xordecrypt::EEQ}}', '{{xore::\u0000}}'],
    crypt: ['{{crypt::abc}}', '{{crypt::abc::1}}', '{{crypt::abc::-1}}', '{{crypt::abc::65536}}', '{{crypt::😀::32768}}', '{{crypt::abc::NaN}}', '{{crypt::abc::1.5}}',
      '{{crypt::abc::0}}', '{{crypt::{{crypt::abc}}}}', '{{crypt::￿::1}}', '{{crypt}}', '{{crypt::abc::-70000}}', '{{crypt::abc::1e21}}'],
    hex: ['{{tohex::255}}', '{{tohex::-255}}', '{{tohex::1e21}}', '{{tohex::0.5}}', '{{tohex::abc}}', '{{tohex::9007199254740993}}', '{{tohex}}',
      '{{fromhex::ff}}', '{{fromhex::0xff}}', '{{fromhex::zz}}', '{{fromhex::-f}}', '{{fromhex::}}', '{{fromhex}}', '{{fromhex:: ff}}', '{{fromhex::fffffffffffffffffff}}'],
    filter: ['{{filter::["a","","a",1,1,null,null]::unique}}', '{{filter::["a","","a",1,1,null,null]::nonempty}}', '{{filter::["a","","a",1,1,null,null]::all}}',
      '{{filter::["a","","a"]::bogus}}', '{{filter::a§§a§b}}', '{{filter}}', '{{filter::[[1],[1],{"a":1}]::unique}}'],
    logic: ['{{all::1::1}}', '{{all::[1,"1"]}}', '{{all::["1","1"]}}', '{{any::0::1}}', '{{all::[]}}', '{{any::[]}}', '{{all::1}}', '{{any::1}}', '{{all}}',
      '{{and::1::1}}', '{{and::true::true}}', '{{or::0::1}}', '{{not::0}}', '{{not::1}}', '{{not::true}}'],
    range: ['{{range::[0,5]}}', '{{range::[0,5,2]}}', '{{range::[5,0,-1]}}', '{{range::[0,1,0.25]}}', '{{range::[0.1,0.5,0.1]}}', '{{range::3}}', '{{range::["2","4"]}}',
      '{{range::[-2,2]}}', '{{range::[1e21,1e21]}}', '{{range::0}}', '{{range::-3}}', '{{range}}', '{{range::[0,3,"1"]}}', '{{range::[null,2]}}'],
    dict: ['{{makedict::a=1::b=x=y::c::__proto__=1::=v::a=2}}', '{{makedict::2=a::1=b::x=c}}', '{{makedict}}', '{{dict::constructor=1}}',
      '{{dictelement::{"a":{"b":1}}::a}}', '{{dictelement::{"a":{"b":1}}::missing}}', '{{dictelement::[1,2]::1}}', '{{dictelement::{"a":null}::a}}',
      '{{dictelement::{"a":"text"}::a}}', '{{dictelement::invalid::a}}', '{{dictelement::{"1":"x"}::01}}', '{{dictelement::{"a":false}::a}}',
      '{{objectassert::{"a":0}::a::x}}', '{{objectassert::{"a":1}::a::x}}', '{{objectassert::[1,2]::5::x}}', '{{objectassert::null::a::x}}',
      '{{objectassert::5::a::x}}', '{{objectassert::{"b":1}::a}}', '{{objectassert::invalid::a::x}}', '{{objectassert::{"a":""}::a::x}}', '{{objectassert::[1]::length::x}}'],
    element: ['{{element::{"a":{"b":[1,2]}}::a::b::1}}', '{{element::{"a":"{\\"b\\":2}"}::a::b}}', '{{element::{"a":"{\\"b\\":\\"x\\"}"}::a::b}}', '{{element::[1,2]::0}}',
      '{{element::[1,2]::1}}', '{{element::abc}}', '{{element::{"a":0}::a}}', '{{element::null::a}}', '{{element::"s"::0}}', '{{ele::{"a":"v"}::a}}', '{{element}}'],
    arrays: ['{{arrayassert::[1,2]::5::x}}', '{{arrayassert::[1,2]::1::x}}', '{{arrayassert::[1,2]::-1::x}}', '{{arrayassert::a§b::3::c}}', '{{arrayassert::[1]::1.5::x}}',
      '{{arrayassert::[1]::NaN::x}}', '{{arrayassert}}', '{{makearray::a::b}}', '{{makearray}}', '{{makearray::a::b::c}}', '{{a::x::y}}',
      '{{arraylength::}}', '{{arraylength::[]}}', '{{arrayelement::[1,[2,3],{"a":1}]::1}}', '{{arrayelement::[1,[2,3],{"a":1}]::2}}', '{{arrayelement::[1]::length}}'],
    text: ['{{reverse::ab😀c\ud800d\udc00}}', '{{reverse}}', '{{reverse::}}', '{{iserror::Error: x}}', '{{iserror::ERROR:x}}', '{{iserror::İerror:}}', '{{iserror}}',
      '{{capitalize::ßx}}', '{{capitalize::ŉ}}', '{{capitalize::\ud800x}}', '{{capitalize::😀}}', '{{capitalize::ǰ}}', '{{capitalize::}}',
      '{{lower::Σ}}', '{{lower::AΣ}}', '{{lower::AΣ b}}', '{{lower::AΣ\ud800}}', '{{lower::\ud800Σb}}', '{{lower::AΣ́}}', '{{lower::İI}}',
      '{{upper::ßŉǰΐﬀ}}', '{{upper::\ud83d}}', '{{upper::a\udc00b}}', '{{lower::ẞΩKÅ}}',
      '{{length::😀\ud800}}', '{{trim::\u0085x᠎}}', '{{trim:: x }}', '{{split::a::}}', '{{join::["a",["b",["c"]],{"x":1},null,true]::-}}',
      '{{spread::["a::b","c"]}}', '{{replace::aaa::a::$$}}', '{{startswith::abc}}', '{{endswith::undefined}}', '{{contains::xundefinedx}}'],
    templates: ['{{button::Label::trigger}}', '{{button}}', '{{risu}}', '{{risu::}}', '{{risu::10}}', '{{tex::x^2}}', '{{tex}}', '{{ruby::漢字::かんじ}}', '{{ruby::a}}',
      '{{codeblock::"a\'<b>"}}', '{{codeblock::js::"a\'<b>"}}', '{{codeblock}}', '{{codeblock::a::b::c}}', '{{cbr}}', '{{cbr::2}}', '{{cbr::0}}', '{{cbr::-5}}',
      '{{cbr::NaN}}', '{{cbr::2.7}}', '{{cnl::3}}', '{{cbr::1e400}}', '{{cbr::x::y}}', '{{br}}{{newline}}{{blank}}{{none}}{{hiddenkey}}',
      '{{decbo}}{{decbc}}{{bo}}{{bc}}{{debo}}{{debc}}{{deabo}}{{deabc}}{{dec}}{{(}}{{)}}{{<}}{{>}}{{;}}'],
    rng: ['{{random}}', '{{random::a}}', '{{random::a,b\\,c}}', '{{random::a:b,c}}', '{{random::[1,"x",null]}}', '{{random::[]}}', '{{random::a::b::c}}',
      '{{randint::1::6}}', '{{randint::6::1}}', '{{randint::a::b}}', '{{randint::1.5::2.5}}', '{{randint}}', '{{dice::2d6}}', '{{dice::d6}}', '{{dice::2d}}',
      '{{dice::0d6}}', '{{dice}}', '{{dice::2d6d1}}', '{{roll}}', '{{roll::d20}}', '{{roll::3d}}', '{{roll::0d6}}', '{{roll::20}}', '{{roll::x}}', '{{roll::2d6::x}}'],
    snapshot: ['{{user}}', '{{USER}}', '{{chatindex}}', '{{chat_index}}', '{{getvar::score}}', '{{getvar::missing}}', '{{getvar}}', '{{getvar::surrogate}}',
      '{{getglobalvar::toggle_demo}}', '{{getglobalvar::missing}}', '{{getglobalvar}}'],
    calc: ['{{calc::1+2*3}}', '{{calc::(1+2)*3}}', '{{calc::((2))}}', '{{calc::(1}}', '{{calc::1)}}', '{{calc::2^10}}', '{{calc::(-2)^3}}', '{{calc::2^-1}}',
      '{{calc::0^0}}', '{{calc::NaN^0}}', '{{calc::1/0}}', '{{calc::-1/0}}', '{{calc::0/0}}', '{{calc::1&0}}', '{{calc::1|0}}', '{{calc::& }}', '{{calc::!}}',
      '{{calc::1<2<3}}', '{{calc::3==3}}', '{{calc::3!=3}}', '{{calc::$}}', '{{calc::$ x}}', '{{calc::a@}}', '{{? 1+}}', '{{? +1}}', '{{? 1 2}}', '{{? 1e3}}',
      '{{? 1e400}}', '{{? 1e-400}}', '{{? 0.1+0.2}}', '{{? 1.5%1}}', '{{? -0}}', '{{? (0-0)*-1}}', '{{?  1}}', '{{?1}}', '{{? ()}}', '{{? (()())}}'],
  };
  for (const [family, values] of Object.entries(targeted)) values.forEach((text, i) => add(`${family}-targeted-${i}`, text));
  for (const [i, text] of ['{{getvar::constructor}}', '{{getvar::__proto__}}', '{{getglobalvar::toString}}', '{{#when::var::hasOwnProperty}}x{{/when}}']
    .entries()) add('prototype-variable-' + i, text, { refusal: 'variable-prototype-key' });
  for (const [i, text] of ['{{tempvar::constructor}}', '{{settempvar::__proto__::x}}', '{{gettempvar::toString}}'].entries())
    add('prototype-temp-' + i, text, { refusal: 'temp-prototype-key' });
  for (const [i, text] of ['{{dictelement::{}::constructor}}', '{{dictelement::{"a":1}::__proto__}}', '{{element::{"a":1}::constructor}}',
    '{{dictelement::{"a":1}::hasOwnProperty}}', '{{dictelement::[1]::map}}', '{{dictelement::"s"::charAt}}', '{{dictelement::1::toFixed}}'].entries())
    add('prototype-property-' + i, text, { refusal: 'prototype-property' });
  // Own properties and prototype values that only decide truthiness need no refusal.
  for (const [i, text] of ['{{#when::toggle::valueOf}}x{{/when}}', '{{dictelement::[1]::length}}', '{{dictelement::"s"::length}}', '{{dictelement::"s"::0}}',
    '{{objectassert::{}::toString::x}}', '{{arrayelement::[1]::map}}', '{{objectassert::[1]::length::x}}'].entries()) add('own-property-' + i, text);
  for (const [i, text] of ['{{calc::$score+1}}', '{{? @demo}}', '{{calc::1+$x_1}}'].entries()) add('calc-variable-' + i, text, { refusal: 'calc-variable' });
  for (const [i, text] of ['{{pow::2::3}}', '{{calc::2^0.5}}', '{{? 3^40}}', '{{? 3^-1}}', '{{? 1.5^2}}', '{{? 2^1024}}', '{{? 0.5^1075}}', '{{? 4^0.5}}'].entries())
    add('pow-' + i, text, { refusal: i ? 'pow-inexact' : 'unsupported-operator' });
  for (const [i, text] of ['{{? 2^-1}}', '{{? 2^-1022}}', '{{? 2^1023}}', '{{? (-2)^3}}', '{{? (-2)^-3}}', '{{? 0.5^-3}}', '{{? 3^33}}', '{{? (-3)^33}}', '{{? 10^15}}',
    '{{? 1^5000}}', '{{? (-1)^-7}}', '{{? 0^-1}}', '{{? (0-0)^-1}}', '{{? (0-0)^-2}}', '{{? (-8)^(1/3)}}', '{{? 1/0^2}}', '{{? (1/0)^-1}}'].entries())
    add('pow-exact-' + i, text);

  // Full case-mapping tables for lower and upper above ASCII, chunked so a mismatch names its range.
  for (const op of ['lower', 'upper']) {
    for (let from = 0x80; from < 0x20000; from += 4096) add(`${op}-table-${from.toString(16)}`, '', { expand: { op, from, to: Math.min(from + 4095, 0x1ffff) }, hash: true });
    for (let plane = 2; plane <= 16; plane++) add(`${op}-plane-${plane}`, '', { expand: { op, from: plane * 0x10000, to: plane * 0x10000 + 0xffff }, hash: true });
  }
  return list;
}

export const fixtureSets = { grammar: { context, cases: grammar() }, families: { context, cases: families() } };

// The product sources the oracle executes. `generate` records their SHA-256 (LF form) in each fixture header,
// and the coverage test reports fixtures generated from other sources.
export const oracleSources = [
  'src/ts/cbs.ts', 'src/ts/cbsPure.ts', 'src/ts/parser/parser.svelte.ts', 'src/ts/process/infunctions.ts',
  'src/ts/process/cbsArithmetic.ts', 'src/ts/util.ts',
];
export const sourceDigests = (paths = oracleSources) => Object.fromEntries(paths.map(path => [path,
  createHash('sha256').update(readFileSync(new URL(`../../../${path}`, import.meta.url), 'utf8').replaceAll('\r\n', '\n')).digest('hex')]));

// The text a case evaluates first; expand cases skip the surrogate block.
export function caseText(item) {
  if (!item.expand) return item.text;
  const { op, from, to } = item.expand;
  let body = '';
  for (let code = from; code <= to; code++) if (code < 0xd800 || code > 0xdfff) body += String.fromCodePoint(code);
  return '{{' + op + '::' + body + '}}';
}
