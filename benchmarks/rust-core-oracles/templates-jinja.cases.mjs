// Writes the synthetic case file of the jinja engine oracle (tests/fixtures/rust-core/templates/jinja.cases.json). Add
// cases here, run `node benchmarks/rust-core-oracles/templates-jinja.cases.mjs`, then regenerate the expected file with
// RUST_CORE_ORACLE_WRITE=1.
//
// A case is { id, template, context?, now?, decline? }. `template` is a string, or its UTF-16 units when not well-formed.
// `context` is JSON source for the render variables; without it the template renders with no variables. `now` is the
// local time `strftime_now` reads. `decline` names the `Unsupported` the Rust engine must answer instead of a result.
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/templates/', import.meta.url))
mkdirSync(dir, { recursive: true })

const str = (value) => value.isWellFormed() ? value : { u: [...Array(value.length).keys()].map(i => value.charCodeAt(i)) }
const cases = []
const ids = new Set()
function c(id, template, context, extra = {}) {
    if (ids.has(id)) throw new Error(`duplicate ${id}`)
    ids.add(id)
    const entry = { id, template: str(template) }
    if (context !== undefined) entry.context = JSON.stringify(context)
    Object.assign(entry, extra)
    cases.push(entry)
}
// One template over several contexts.
function each(prefix, template, contexts) {
    for (const [name, context] of Object.entries(contexts)) c(`${prefix}-${name}`, template, context)
}

// --- text, whitespace control and comments ---
c('text-plain', 'Hello, world')
c('text-empty', '')
c('text-trailing-newline', 'line\n')
c('text-two-trailing-newlines', 'line\n\n')
c('text-crlf', 'a\r\nb\r\n')
c('text-lone-braces', 'a { b } c }} d {')
c('text-brace-percent-in-string', '{{ "{% raw %}" }}')
c('text-unicode', '한국어 😀 \u2028 é')
c('text-lone-surrogate', 'a\ud800b{{ "\udc00" }}')
c('output-trim-left', 'a   {{- "x" }}   b')
c('output-trim-right', 'a   {{ "x" -}}   b')
c('output-trim-both', 'a \n\t {{- "x" -}} \n b')
c('block-trim', 'a  {%- if true -%}  b  {%- endif -%}  c')
c('comment', 'a{# note #}b')
c('comment-trim', 'a  {#- note -#}  b')
c('comment-multiline', 'a{# one\ntwo #}b')
c('comment-unterminated', 'a{# note')
c('lstrip-blocks', 'x\n    {% if true %}\n    y\n    {% endif %}\nz')
c('lstrip-blocks-tabs', 'x\n\t \t{% if true %}y{% endif %}')
c('lstrip-blocks-comment', 'x\n   {# c #}\ny')
c('lstrip-not-output', 'x\n   {{ "y" }}\n')
c('trim-blocks-newline', '{% if true %}\nyes\n{% endif %}\nafter')
c('trim-blocks-comment-newline', '{# c #}\nafter')
c('trim-blocks-crlf', '{% if true %}\r\nyes{% endif %}')
c('lstrip-after-cr', 'x\r   {% if true %}y{% endif %}')
c('lstrip-after-line-separator', 'x\u2028   {% if true %}y{% endif %}')
c('generation-tag', 'a{% generation %}b{% endgeneration %}c')
c('generation-tag-spaces', 'a  {%  generation  %}  b  {%  endgeneration  %}  c')
c('whitespace-only-output', '{{ "  " }}|{{ "\t" }}|')
c('unclosed-output', 'a {{ "x"')
c('unclosed-block', 'a {% if true')
c('missing-endif', '{% if true %}x')
c('missing-endfor', '{% for x in [1] %}x')
c('stray-endif', 'x{% endif %}')
c('unknown-statement', '{% frobnicate %}')
c('empty-output', '{{ }}')
c('raw-block', '{% raw %}{{ x }}{% endraw %}')

// --- literals and number formatting ---
c('int', '{{ 42 }}')
c('int-negative', '{{ -42 }}')
c('int-big', '{{ 12345678901234567890 }}')
c('float', '{{ 1.5 }}')
c('float-integral', '{{ 3.0 }}')
c('float-negative-zero', '{{ -0.0 }}')
c('int-negative-zero', '{{ -0 }}')
c('float-long', '{{ 0.1 + 0.2 }}')
c('float-small', '{{ 1.0 / 3 }}')
c('float-tiny', '{{ 0.0000001 }}')
c('float-huge', '{{ 10.0 ** 21 }}')
c('float-huge-integral-small', '{{ 10.0 ** 20 }}')
c('int-power-big', '{{ 10 ** 21 }}')
c('number-exponent-word', '{{ 1e3 }}')
c('number-dot-member', '{{ [1, 2].1 }}')
c('number-trailing-dot', '{{ 1. }}')
c('string-single', "{{ 'single' }}")
c('string-double', '{{ "double" }}')
c('string-escapes', String.raw`{{ "a\nb\tc\\d\'e\"f\rg\bh\fi\vj" }}`)
c('string-unknown-escape', String.raw`{{ "a\qb" }}`)
c('string-unicode-escape', String.raw`{{ "\u0041" }}`)
c('string-adjacent', `{{ "a" 'b' "c" }}`)
c('string-unterminated', '{{ "abc }}')
c('string-newline-inside', '{{ "a\nb" }}')
c('bool-literals', '{{ true }} {{ false }} {{ True }} {{ False }}')
c('none-literals', '[{{ none }}][{{ None }}]')
c('array-literal', '{{ [1, "a", 2.5, true, none] }}')
c('array-empty', '{{ [] }}')
c('array-trailing-comma', '{{ [1, 2,] }}')
c('tuple-literal', '{{ (1, 2) }}')
c('tuple-paren-single', '{{ (1) }}')
c('object-literal', '{{ {"a": 1, "b": [true, none], "c": {"d": "e"}} }}')
c('object-empty', '{{ {} }}')
c('object-duplicate-key', '{{ {"a": 1, "b": 2, "a": 3} }}')
c('object-non-string-key', '{{ {1: 2} }}')
c('object-identifier-key', '{% set k = "z" %}{{ {k: 1} }}')
c('nested-render', '{{ [[1, [2]], {"a": [{}]}] }}')

// --- variables ---
c('undefined-render', '[{{ missing }}]')
c('undefined-member', '[{{ missing.attr }}]')
c('undefined-member-chain', '[{{ missing.a.b }}]')
c('undefined-index', '[{{ missing[0] }}]')
c('undefined-string-index', '[{{ missing["a"] }}]')
c('null-member', '[{{ none.attr }}]')
c('number-member', '[{{ (5).attr }}]')
c('context-values', '{{ s }}|{{ i }}|{{ f }}|{{ b }}|{{ n }}|{{ a }}|{{ o }}', { s: 'str', i: 3, f: 2.5, b: true, n: null, a: [1, 'x'], o: { k: 'v' } })
c('context-negative-zero', '{{ z }}|{{ z is integer }}|{{ -z }}', JSON.parse('{"z": -0}'))
c('context-integral-float', '{{ x }}|{{ x is float }}|{{ x is integer }}', { x: 2.0 })
c('context-unicode-keys', '{{ 한 }}', { '한': 'ok' })
c('context-redeclare-true', '{{ x }}', { true: 1 })
c('context-redeclare-range', '{{ x }}', { range: 1 })
c('context-redeclare-namespace', '{{ x }}', { namespace: 1 })
c('context-lone-surrogate', '{{ s|length }}:{{ s }}', { s: '\ud800x' })
c('context-proto-key', '{{ o["__proto__"] }}', JSON.parse('{"o": {"__proto__": "own"}}'))
c('set-simple', '{% set x = 1 %}{{ x }}')
c('set-overwrite-context', '{% set x = x + 1 %}{{ x }}', { x: 1 })
c('set-tuple', '{% set a, b = [1, 2] %}{{ a }}{{ b }}')
c('set-tuple-paren', '{% set (a, b) = (3, 4) %}{{ a }}{{ b }}')
c('set-tuple-mismatch', '{% set a, b = [1, 2, 3] %}')
c('set-tuple-few', '{% set a, b, c = [1, 2] %}')
c('set-tuple-not-array', '{% set a, b = "ab" %}')
c('set-block', '{% set x %}inner {{ 1 + 1 }}{% endset %}[{{ x }}]')
c('set-block-trim', '{% set x -%}  inner  {%- endset %}[{{ x }}]')
c('set-member-object', '{% set o = {"a": 1} %}{% set o.a = 2 %}')
c('set-invalid-target', '{% set 1 = 2 %}')
c('set-scope-in-if', '{% set x = 1 %}{% if true %}{% set x = 2 %}{% endif %}{{ x }}')
c('set-scope-in-for', '{% set x = 1 %}{% for i in [1] %}{% set x = 2 %}{% endfor %}{{ x }}')
c('global-names', '{{ true }}{{ none is none }}{{ range is callable }}{{ namespace is callable }}{{ raise_exception is callable }}')

// --- arithmetic and operators ---
c('arith-int', '{{ 7 + 3 }} {{ 7 - 3 }} {{ 7 * 3 }} {{ 7 / 2 }} {{ 7 // 2 }} {{ 7 % 3 }}')
c('arith-float', '{{ 7.0 + 3 }} {{ 7 - 3.0 }} {{ 2.5 * 2 }} {{ 6 / 3 }} {{ 7.5 // 2 }} {{ 7.5 % 2 }}')
c('arith-negative', '{{ -7 // 2 }} {{ -7 % 3 }} {{ 7 % -3 }} {{ -7.5 // 2 }}')
c('arith-divide-zero', '{{ 1 / 0 }} {{ -1 / 0 }} {{ 0 / 0 }}')
c('arith-floor-divide-zero', '{{ 1 // 0 }}')
c('arith-modulo-zero', '{{ 1 % 0 }}')
c('arith-bool', '{{ true + 1 }} {{ true * 2.5 }}')
c('arith-bool-bool', '{{ true + true }}')
c('arith-precedence', '{{ 1 + 2 * 3 - 4 / 2 }} {{ (1 + 2) * 3 }} {{ 2 ** 3 ** 2 }} {{ -2 ** 2 }}')
c('power', '{{ 2 ** 10 }} {{ 2 ** -1 }} {{ 2.0 ** 2 }} {{ 4 ** 0.5 }} {{ true ** 2 }}')
c('power-zero-negative', '{{ 0 ** -1 }}')
c('power-overflow', '{{ 10 ** 400 }}')
c('power-nan', '{{ (-8) ** 0.5 }}')
c('power-one-infinite', '{% set inf = 1 / 0 %}{{ 1 ** inf }}')
// `**` renders only an exact result; V8 computes `pow` itself, so an inexact one can differ in its last bit.
c('power-exact-near-2-53', '{{ 2 ** 53 }} {{ 2.0 ** 53 }} {{ 3 ** 33 }} {{ (-3) ** 33 }} {{ 94906265 ** 2 }} {{ (-94906265) ** 2 }}')
c('power-exact-past-2-53', '{{ 2 ** 60 }} {{ 12 ** 20 }} {{ (-2) ** 63 }} {{ 2 ** 1023 }}')
c('power-exact-negative-exponent', '{{ 0.5 ** -3 }} {{ (-2) ** -3 }} {{ 2 ** -1022 }} {{ 1 ** -7 }} {{ (-1) ** -7 }} {{ 0.25 ** -1 }}')
c('power-exact-root', '{{ 9 ** 0.5 }} {{ 2.25 ** 0.5 }} {{ 0 ** 0.5 }} {{ 1 ** 0.5 }} {{ 4503599761588225 ** 0.5 }}')
c('power-exact-trivial', '{{ 1.1 ** 0 }} {{ 1.1 ** 1 }} {{ 0 ** 0 }} {{ 0 ** 3 }} {{ 7 ** 1 }} {{ (-0.0) ** 3 }} {{ (-0.0) ** 2 }}')
c('power-special-operands', '{% set inf = 1 / 0 %}{% set nan = 0 / 0 %}{{ 0.5 ** inf }} {{ 2 ** -inf }} {{ inf ** -1 }} {{ (-inf) ** -3 }} {{ nan ** 0 }}')
c('power-infinite-result', '{% set inf = 1 / 0 %}{{ 2 ** inf }}')
c('power-nan-base', '{% set nan = 0 / 0 %}{{ nan ** 2 }}')
c('power-overflow-power-of-two', '{{ 2 ** 2000 }}')
c('power-inexact-base', '{{ 1.1 ** 10 }}', undefined, { decline: 'pow-inexact' })
c('power-inexact-fraction', '{{ 2 ** 0.1 }}', undefined, { decline: 'pow-inexact' })
c('power-inexact-root', '{{ 2 ** 0.5 }}', undefined, { decline: 'pow-inexact' })
c('power-inexact-negative-exponent', '{{ 10 ** -5 }}', undefined, { decline: 'pow-inexact' })
c('power-inexact-big', '{{ 3 ** 40 }}', undefined, { decline: 'pow-inexact' })
c('power-inexact-past-2-53', '{{ 3 ** 34 }}', undefined, { decline: 'pow-inexact' })
c('power-inexact-square-past-2-53', '{{ 94906267 ** 2 }}', undefined, { decline: 'pow-inexact' })
c('power-fraction-base', '{{ 0.5 ** 2 }}', undefined, { decline: 'pow-inexact' })
c('power-subnormal', '{{ 2 ** -1074 }}', undefined, { decline: 'pow-inexact' })
c('power-near-overflow', '{{ 2 ** 1024 }}', undefined, { decline: 'pow-inexact' })
c('unary-plus-minus', '{{ +3 }} {{ -3.5 }} {{ -true }} {{ +false }} {{ - -2 }}')
c('unary-minus-string', '{{ -"a" }}')
c('compare-numbers', '{{ 1 < 2 }} {{ 2 <= 2 }} {{ 3 > 4 }} {{ 3 >= 3.0 }} {{ 1 == 1.0 }} {{ 1 != 2 }}')
c('compare-strings', '{{ "a" < "b" }}')
c('compare-mixed', '{{ 1 < "2" }}')
c('loose-equality', '{{ 1 == true }} {{ "1" == 1 }} {{ "" == 0 }} {{ none == none }} {{ "a" == "a" }} {{ [1] == [1] }} {{ "1" == true }} {{ " 1 " == 1 }}')
c('loose-equality-undefined', '{{ missing == none }} {{ missing == missing }} {{ missing != 0 }} {{ none == 0 }} {{ none == "" }}')
c('loose-equality-array-string', '{{ [1, 2] == "1,2" }} {{ [] == "" }} {{ [none] == "" }} {{ [[1], 2] == "1,2" }}')
c('loose-equality-array-number', '{{ [] == 0 }} {{ [5] == 5 }} {{ [1, 2] == 1 }} {{ [true] == 1 }}')
c('loose-equality-object', '{% set o = {"a": 1} %}{{ o == o }} {{ o == {"a": 1} }} {{ o == "[object Map]" }}')
c('loose-equality-identity', '{% set a = [1] %}{% set b = a %}{{ a == b }} {{ a == [1] }}')
c('and-or', '{{ true and "x" }}|{{ false and "x" }}|{{ "" or "y" }}|{{ "z" or "y" }}|{{ 0 or none }}|{{ [] or "e" }}')
c('and-or-short-circuit', '{{ false and raise_exception("no") }}{{ true or raise_exception("no") }}ok')
c('not', '{{ not true }} {{ not 0 }} {{ not "" }} {{ not "a" }} {{ not none }} {{ not [] }} {{ not {} }}')
c('not-in', '{{ 1 not in [1, 2] }} {{ 3 not in [1, 2] }} {{ "a" not in "abc" }}')
c('in-array', '{{ 1 in [1, 2] }} {{ "1" in [1, 2] }} {{ 1.0 in [1] }} {{ none in [none] }} {{ [1] in [[1]] }}')
c('in-string', '{{ "b" in "abc" }} {{ "" in "abc" }} {{ "d" in "abc" }}')
c('in-object', '{{ "a" in {"a": 1} }} {{ "b" in {"a": 1} }}')
c('in-object-non-string', '{{ 1 in {"a": 1} }}')
c('in-undefined', '{{ 1 in missing }} {{ 1 not in missing }}')
c('in-undefined-left', '{{ missing in [1] }}')
c('in-null', '{{ 1 in none }}')
c('in-number', '{{ 1 in 5 }}')
c('concat-tilde', '{{ "a" ~ 1 ~ 2.0 ~ true ~ [1, 2] }}')
c('concat-tilde-none', '{{ "a" ~ none }}')
c('concat-tilde-undefined', '{{ "a" ~ missing }}')
c('concat-tilde-numbers', '{{ 1 ~ 2 }}')
c('concat-plus', '{{ "a" + "b" }} {{ "a" + 1 }} {{ 1.0 + "b" }} {{ "x" + true }}')
c('concat-plus-array-string', '{{ "a" + [1] }}')
c('concat-plus-object', '{{ "a" + {"k": 1} }}')
c('array-plus', '{{ [1] + [2, 3] }}')
c('array-minus', '{{ [1] - [1] }}')
c('string-times', '{{ "ab" * 3 }}')
c('string-minus', '{{ "a" - "b" }}')
c('null-arith', '{{ none + 1 }}')
c('undefined-arith', '{{ missing + 1 }}')
c('ternary', '{{ "y" if true else "n" }}{{ "y" if false else "n" }}{{ "y" if 0 else "n" }}')
c('ternary-nested', '{{ 1 if false else 2 if false else 3 }}')
c('select-expression', '[{{ "y" if false }}][{{ "y" if true }}]')
c('ternary-precedence', '{{ 1 + 1 if true else 0 }}')

// --- member access, indexing and slicing ---
const people = { people: [{ name: 'Ann', age: 31, tags: ['a', 'b'] }, { name: 'bob', age: 25, tags: [] }, { name: 'Cy', age: 40, tags: ['c'] }] }
c('member-dot', '{{ people[0].name }} {{ people[1]["age"] }} {{ people.0.name }}', people)
c('member-negative-index', '{{ people[-1].name }} {{ people[-3].name }}', people)
c('member-out-of-range', '[{{ people[5] }}][{{ people[-9] }}]', people)
c('member-float-index', '{{ [1, 2, 3][1.5] }}')
c('member-bool-index', '{{ [1, 2][true] }}')
c('member-array-length', '{{ people.length }} {{ [].length }}', people)
c('member-array-unknown', '[{{ [1].foo }}]')
c('member-object-missing', '[{{ {"a": 1}.b }}]')
c('member-object-number-key', '{{ {"a": 1}[0] }}')
c('member-object-builtin-shadow', '{{ {"items": 5}.items }}')
c('member-string-index', '{{ "abc"[0] }}{{ "abc"[-1] }}{{ "abc".1 }}')
c('member-string-index-out-of-range', '[{{ "abc"[5] }}]', undefined, { decline: 'string-index-out-of-range' })
c('member-string-surrogate-index', '{{ "😀"[0]|length }}')
c('member-string-unknown', '[{{ "abc".foo }}]')
c('member-string-length', '{{ "abc".length }} {{ "😀".length }}')
c('member-bool-property', '[{{ true.x }}]')
c('member-function-property', '[{{ range.x }}]')
c('member-computed-string', '{% set k = "name" %}{{ people[0][k] }}', people)
c('member-object-index-key', '{{ {"0": "zero"}["0"] }}')
c('slice-basic', '{{ [1, 2, 3, 4, 5][1:3] }} {{ [1, 2, 3, 4, 5][:2] }} {{ [1, 2, 3, 4, 5][3:] }} {{ [1, 2, 3, 4, 5][:] }}')
c('slice-negative', '{{ [1, 2, 3, 4, 5][-2:] }} {{ [1, 2, 3, 4, 5][:-2] }} {{ [1, 2, 3, 4, 5][-9:2] }}')
c('slice-step', '{{ [1, 2, 3, 4, 5][::2] }} {{ [1, 2, 3, 4, 5][::-1] }} {{ [1, 2, 3, 4, 5][4:1:-1] }} {{ [1, 2, 3, 4, 5][1::-1] }}')
c('slice-step-negative-bounds', '{{ [1, 2, 3, 4, 5][-1:-4:-2] }} {{ [1, 2, 3][5:0:-1] }} {{ [1, 2, 3][:-5:-1] }}')
c('slice-string', '{{ "hello"[1:4] }} {{ "hello"[::-1] }} {{ "😀ab"[0:1] }} {{ "a😀b"[::-1] }}')
c('slice-empty', '[{{ [1, 2][2:1] }}][{{ "ab"[5:] }}]')
c('slice-zero-step', '{{ [1, 2][::0] }}')
c('slice-float-bound', '{{ [1, 2][0.5:] }}')
c('slice-object', '{{ {"a": 1}[0:1] }}')
c('slice-undefined-bound', '{{ [1, 2, 3][missing:2] }}')
c('slice-messages', '{{ messages[1:]|length }}', { messages: [{ role: 'system' }, { role: 'user' }, { role: 'assistant' }] })
c('slice-identity', '{% set a = [1, 2] %}{{ a[:] == a }}')

// --- string methods ---
c('str-upper-lower', '{{ "aBc".upper() }} {{ "AbC".lower() }} {{ "straße".upper() }} {{ "İ".lower()|length }} {{ "ǅ".lower() }}')
c('str-strip', '[{{ "  a b  ".strip() }}][{{ "  a ".lstrip() }}][{{ " a  ".rstrip() }}][{{ "\u3000x\u00a0".strip() }}]')
c('str-strip-chars', '[{{ "xxaxx".strip("x") }}]')
c('str-title', '{{ "hello wORLD foo-bar o\'neil 1st éa".title() }}')
c('str-capitalize', '{{ "hELLO".capitalize() }} {{ "".capitalize() }} {{ "ßa".capitalize() }} {{ "1a".capitalize() }}')
c('str-startswith', '{{ "hello".startswith("he") }} {{ "hello".startswith("lo") }} {{ "hello".endswith("lo") }} {{ "hello".startswith(["x", "h"]) }} {{ "hello".endswith(("x", "y")) }}')
c('str-startswith-bad', '{{ "hello".startswith(1) }}')
c('str-startswith-none', '{{ "hello".startswith() }}')
c('str-split', '{{ "a,b,,c".split(",") }} {{ "a b  c ".split() }} {{ "  a  b ".split(none) }} {{ "abc".split("x") }}')
c('str-split-maxsplit', '{{ "a,b,c,d".split(",", 1) }} {{ "a b c d".split(none, 2) }} {{ "a,b".split(",", 0) }} {{ " a b c".split(none, 0) }} {{ "a,b,c".split(",", -2) }}')
c('str-split-empty-separator', '{{ "abc".split("") }}')
c('str-split-bad-separator', '{{ "abc".split(1) }}')
c('str-split-bad-maxsplit', '{{ "abc".split(",", "1") }}')
c('str-split-empty-string', '{{ "".split(",") }} {{ "".split() }}')
c('str-replace', '{{ "aaa".replace("a", "b") }} {{ "aaa".replace("a", "b", 2) }} {{ "aaa".replace("a", "b", 0) }} {{ "aaa".replace("a", "b", -1) }} {{ "aaa".replace("a", "b", none) }}')
c('str-replace-kwarg', '{{ "aaa".replace("a", "b", count=1) }}')
c('str-replace-empty-pattern', '{{ "ab".replace("", "-") }} {{ "😀".replace("", "-") }} {{ "".replace("", "-") }} {{ "abc".replace("", "-", 2) }}')
c('str-replace-special', '{{ "a.b".replace(".", "$&") }} {{ "a+b".replace("+", "$1") }}')
c('str-replace-surrogates', '{{ "😀😀".replace("\ud83d", "x") }}')
c('str-replace-too-few', '{{ "a".replace("a") }}')
c('str-replace-bad-count', '{{ "a".replace("a", "b", "1") }}')
c('str-method-unknown', '{{ "a".zfill(3) }}')
c('str-method-as-value', '[{{ "a".upper }}]', undefined, { decline: 'function-to-string' })
c('str-method-chain', '{{ "  Hello World  ".strip().lower().split(" ")[1] }}')

// --- object methods ---
const obj = { obj: { b: 2, a: 1, C: 3, d: [1, 2] } }
c('obj-items', '{% for k, v in obj.items() %}{{ k }}={{ v }};{% endfor %}', obj)
c('obj-keys-values', '{{ obj.keys() }} {{ obj.values() }}', obj)
c('obj-get', '{{ obj.get("a") }} {{ obj.get("zz") }} {{ obj.get("zz", "dflt") }} {{ obj.get("zz", default="kw") }}', obj)
c('obj-get-non-string', '{{ obj.get(1) }}', obj)
c('obj-get-none', '{{ obj.get() }}', obj)
c('obj-dictsort', '{{ obj.dictsort() }}', { obj: { b: 2, a: 1, c: 3 } })
c('obj-dictsort-case', '{{ obj.dictsort() }} {{ obj.dictsort(true) }}', { obj: { b: 2, a: 1, B: 3, A: 4 } })
c('obj-dictsort-value', '{{ obj.dictsort(by="value") }} {{ obj.dictsort(false, "value", true) }}', { obj: { x: 3, y: 1, z: 2 } })
c('obj-dictsort-reverse', '{{ obj.dictsort(reverse=true) }}', { obj: { b: 2, a: 1, c: 3 } })
c('obj-dictsort-mixed', '{{ obj.dictsort(by="value") }}', { obj: { x: 3, y: 'a' } })
c('obj-dictsort-bad-by', '{{ obj.dictsort(by="other") }}', { obj: { x: 3 } })
c('obj-dictsort-ties', '{{ obj.dictsort(by="value") }}', { obj: { a: 1, b: 0, c: 1, d: 0 } })
c('obj-dictsort-filter', '{{ obj|dictsort }}', { obj: { b: 2, a: 1 } })
c('obj-length', '{{ obj|length }} {{ {}|length }}', obj)
c('obj-method-unknown', '{{ obj.pop("a") }}', obj)

// --- filters ---
c('filter-safe', '{{ "<b>"|safe }} {{ 1|safe }}')
c('filter-length', '{{ "abc"|length }} {{ [1, 2]|length }} {{ {"a": 1}|length }} {{ "😀"|length }}')
c('filter-length-number', '{{ 5|length }}')
c('filter-upper-lower', '{{ "Ab"|upper }} {{ "Ab"|lower }} {{ "ab cd"|title }} {{ "ab cd"|capitalize }}')
c('filter-trim', '[{{ "  a  "|trim }}]')
c('filter-trim-number', '{{ 5|trim }}')
c('filter-first-last', '{{ [1, 2, 3]|first }} {{ [1, 2, 3]|last }}')
c('filter-first-empty', '[{{ []|first }}]', undefined, { decline: 'first-of-empty' })
c('filter-last-empty', '[{{ []|last }}]', undefined, { decline: 'last-of-empty' })
c('filter-first-string', '{{ "abc"|first }}')
c('filter-reverse', '{{ [1, 2, 3]|reverse }}')
c('filter-reverse-string', '{{ "abc"|reverse }}')
c('filter-list', '{{ [1, 2]|list }}')
c('filter-list-string', '{{ "ab"|list }}')
c('filter-join', '{{ [1, "a", 2.5, true]|join }} {{ ["a", "b"]|join(", ") }} {{ "abc"|join("-") }} {{ "😀x"|join("-") }} {{ [1, 2]|join(separator="+") }}')
c('filter-join-nested', '{{ [[1, 2], [3]]|join("|") }} {{ [none, missing]|join("|") }} {{ [{"a": 1}]|join }}')
c('filter-join-bad-separator', '{{ [1]|join(1) }}')
c('filter-join-number', '{{ 5|join }}')
c('filter-string', '{{ 1|string }} {{ 1.0|string }} {{ true|string }} {{ "s"|string }} {{ [1, "a"]|string }}')
c('filter-string-object', '{{ {"a": 1}|string }}')
c('filter-int-float', '{{ "42"|int }} {{ "4.7"|int }} {{ "x"|int }} {{ "x"|int(7) }} {{ 4.7|int }} {{ -4.7|int }} {{ "4.7"|float }} {{ "x"|float }} {{ "x"|float(1.5) }} {{ 3|float }} {{ true|int }} {{ false|float }}')
c('filter-int-call-number', '{{ 4.7|int(0) }} {{ 4|float(0) }} {{ true|int(5) }}')
c('filter-int-string-forms', '{{ " 12abc"|int }} {{ "0x1A"|int }} {{ "1e3"|int }} {{ "1e3"|float }} {{ "-0"|int }} {{ ".5"|float }} {{ "Infinity"|float }}')
c('filter-abs', '{{ -3|abs }} {{ (-3.5)|abs }} {{ 3|abs }}')
c('filter-abs-string', '{{ "a"|abs }}')
c('filter-bool', '{{ true|bool }} {{ false|bool }}')
c('filter-bool-number', '{{ 1|bool }}')
c('filter-unique', '{{ [1, 2, 1, "1", 2.0, none, none]|unique }}')
c('filter-sort', '{{ [3, 1, 2]|sort }} {{ ["b", "A", "a", "B"]|sort }} {{ [true, false, 2]|sort }}')
c('filter-sort-call', '{{ [3, 1, 2]|sort(true) }} {{ ["b", "A", "a", "B"]|sort(case_sensitive=true) }} {{ ["b", "A", "a", "B"]|sort(reverse=true) }}')
c('filter-sort-attribute', '{{ people|sort(attribute="age")|map(attribute="name")|join(",") }} {{ people|sort(attribute="name")|map(attribute="name")|join(",") }}', people)
c('filter-sort-attribute-path', '{{ items|sort(attribute="a.b")|map(attribute="id")|join }}', { items: [{ id: 'x', a: { b: 2 } }, { id: 'y', a: { b: 1 } }] })
c('filter-sort-attribute-index', '{{ pairs|sort(attribute=1) }}', { pairs: [[1, 'b'], [2, 'a']] })
c('filter-sort-mixed', '{{ [1, "a"]|sort }}')
c('filter-sort-none', '{{ [none, none]|sort }} {{ [none, 1]|sort }}')
c('filter-sort-single-mixed', '{{ [{"a": 1}]|sort }}')
c('filter-sort-objects', '{{ [{"a": 1}, {"a": 2}]|sort }}')
c('filter-sort-nan', '{{ [1, 0 / 0, 0]|sort }}', undefined, { decline: 'sort-nan' })
c('filter-sort-stable-ties', '{{ ["b", "B", "a", "A"]|sort }} {{ ["b", "B", "a", "A"]|sort(reverse=true) }}')
c('filter-tojson', '{{ {"a": [1, 2.5, "x", true, none], "b": {}}|tojson }}')
c('filter-tojson-string-escapes', '{{ "quote \\" back \\\\ nl \n tab \t ctl \u0001 한 😀 \u2028"|tojson }}')
c('filter-tojson-lone-surrogate', '{{ s|tojson }}', { s: 'a\udc00b' })
c('filter-tojson-numbers', '{{ [1, 1.0, -0.0, 0.5, 10.0 ** 21, 12345678901234567890, 1 / 0]|tojson }}')
c('filter-tojson-undefined', '{{ [missing, 1]|tojson }} [{{ missing|tojson }}] {{ {"a": missing}|tojson }}')
c('filter-tojson-indent', '{{ {"a": [1, {"b": 2}], "c": [], "d": {}}|tojson(indent=2) }}')
c('filter-tojson-indent-zero', '{{ {"a": [1]}|tojson(indent=0) }}')
c('filter-tojson-indent-negative', '{{ {"a": [1]}|tojson(indent=-1) }}')
c('filter-tojson-indent-bad', '{{ {"a": 1}|tojson(indent="  ") }}')
c('filter-tojson-separators', '{{ {"a": [1, 2]}|tojson(separators=(",", ":")) }} {{ {"a": [1, 2]}|tojson(separators=[";", "="], indent=1) }}')
c('filter-tojson-separators-bad', '{{ {"a": 1}|tojson(separators=(",")) }}')
c('filter-tojson-ensure-ascii', '{{ "한😀é\u007f~"|tojson(ensure_ascii=true) }}')
c('filter-tojson-sort-keys', '{{ {"b": 1, "a": {"d": 1, "c": 2}, "_x": 0, "1": 2, "a_b": 3, "ab": 4, "a1": 5}|tojson(sort_keys=true) }}')
c('filter-tojson-sort-keys-upper', '{{ {"b": 1, "B": 2}|tojson(sort_keys=true) }}', undefined, { decline: 'tojson-sort-keys-collation' })
c('filter-tojson-tuple', '{{ (1, 2)|tojson }}')
c('filter-tojson-kwargs-default', '{{ 1|tojson() }}')
c('filter-tojson-function', '{{ range|tojson }}')
c('filter-tojson-namespace', '{% set ns = namespace(a=1) %}{{ ns|tojson }}{{ ns }}')
c('filter-tojson-nested-tuple', '{{ [(1, 2)]|tojson }}')
c('filter-indent', '{{ "a\nb\n\nc"|indent }}|{{ "a\nb"|indent(2) }}|{{ "a\nb"|indent(2, true) }}|{{ "a\n\nb"|indent(2, false, true) }}|{{ "a\nb"|indent(width=1, first=true) }}')
c('filter-indent-bad-width', '{{ "a"|indent("x") }}')
c('filter-indent-truthy-first', '{{ "a\nb"|indent(1, "yes") }}|{{ "a\nb"|indent(1, 0) }}')
c('filter-replace', '{{ "aaa"|replace("a", "b") }} {{ "aaa"|replace("a", "b", 1) }} {{ "aaa"|replace("a", "b", count=2) }}')
c('filter-default', '[{{ missing|default("d") }}][{{ none|default("d") }}][{{ ""|default("d") }}][{{ ""|default("d", true) }}][{{ 0|default("d", boolean=true) }}][{{ missing|default }}][{{ "x"|default("d") }}]')
c('filter-default-bad-flag', '{{ missing|default("d", 1) }}')
c('filter-selectattr', '{{ people|selectattr("age")|map(attribute="name")|join }} {{ people|selectattr("tags")|map(attribute="name")|join }} {{ people|rejectattr("tags")|map(attribute="name")|join }}', people)
c('filter-selectattr-test', '{{ people|selectattr("name", "equalto", "Cy")|map(attribute="age")|join }} {{ people|rejectattr("name", "eq", "Cy")|map(attribute="name")|join }} {{ people|selectattr("age", "odd")|map(attribute="name")|join }}', people)
c('filter-selectattr-missing', '{{ people|selectattr("zzz")|list }} {{ people|rejectattr("zzz")|length }}', people)
c('filter-selectattr-defined', '{{ items|selectattr("x", "defined")|length }} {{ items|selectattr("x", "none")|length }}', { items: [{ x: 1 }, { y: 2 }, { x: null }] })
c('filter-selectattr-unknown-test', '{{ people|selectattr("name", "frob")|list }}', people)
c('filter-selectattr-non-object', '{{ [1, 2]|selectattr("a")|list }}')
c('filter-selectattr-non-literal', '{% set k = "name" %}{{ people|selectattr(k)|list }}', people)
c('filter-selectattr-empty', '{{ []|selectattr()|list }}|{{ []|selectattr("a")|list }}')
c('filter-selectattr-no-args', '{{ people|selectattr()|list }}', people)
c('filter-selectattr-eq-one-arg', '{{ people|selectattr("name", "eq")|list }}', people)
c('filter-map', '{{ people|map(attribute="name")|list }} {{ people|map(attribute="zzz")|list }} {{ people|map(attribute="zzz", default="?")|join }}', people)
c('filter-map-path', '{{ items|map(attribute="a.b")|list }} {{ items|map(attribute="l.0")|list }}', { items: [{ a: { b: 1 }, l: ['x'] }, { a: {}, l: [] }] })
c('filter-map-filter-name', '{{ ["a", "b"]|map("upper")|list }}')
c('filter-map-non-object', '{{ [1]|map(attribute="a")|list }}')
c('filter-map-tojson', '{{ messages|map(attribute="content")|list|tojson }}', { messages: [{ content: 'a' }, { content: 'b' }] })
c('filter-chain', '{{ "  b,a,c "|trim|replace(",", " ")|split(" ")|sort|join("-") }}')
c('filter-unknown', '{{ 1|frobnicate }}')
c('filter-unknown-call', '{{ "a"|frobnicate(1) }}')
c('filter-unknown-array', '{{ [1]|sum }}')
c('filter-on-undefined', '{{ missing|upper }}')
c('filter-on-none', '{{ none|length }}')
c('filter-items', '{{ {"a": 1}|items|list }}')
c('filter-object-builtin', '{{ {"a": 1}|keys }} {{ {"a": 1}|values }} {{ {"a": 1}|get("a") }}')
c('filter-object-unknown', '{{ {"a": 1}|upper }}')
c('filter-statement', '{% filter upper %}hello {{ "world" }}{% endfilter %}')
c('filter-statement-call', '{% filter replace("a", "o") %}banana{% endfilter %}')
c('filter-number-string', '{{ 2.50|string }} {{ -0.0|string }}')
c('filter-precedence', '{{ -1|abs }} {{ 1 + 2|string }} {{ "a" ~ 1|string }}')

// --- tests ---
c('test-basic', '{{ 1 is number }} {{ 1.5 is number }} {{ "a" is string }} {{ none is none }} {{ true is boolean }} {{ [] is iterable }} {{ "a" is iterable }} {{ {} is mapping }} {{ [] is mapping }}')
c('test-integer-float', '{{ 1 is integer }} {{ 1.5 is integer }} {{ 1.0 is integer }} {{ 1.0 is float }}')
c('test-defined', '{{ x is defined }} {{ y is defined }} {{ y is undefined }} {{ x is not defined }} {{ none is defined }}', { x: 1 })
c('test-odd-even', '{{ 3 is odd }} {{ 3 is even }} {{ -2 is even }} {{ 0 is odd }}')
c('test-odd-float', '{{ 3.0 is odd }}')
c('test-true-false', '{{ true is true }} {{ 1 is true }} {{ false is false }} {{ 0 is false }}')
c('test-callable', '{{ range is callable }} {{ "a".upper is callable }} {{ 1 is callable }}')
c('test-sequence', '{{ [] is sequence }} {{ "" is sequence }} {{ {} is sequence }} {{ 1 is sequence }} {{ (1, 2) is sequence }} {{ namespace() is sequence }} {{ namespace() is mapping }}')
c('test-tuple-iterable', '{{ (1, 2) is iterable }}')
c('test-lower-upper', '{{ "abc" is lower }} {{ "aBc" is lower }} {{ "ABC" is upper }} {{ "123" is upper }} {{ 1 is lower }}')
c('test-equalto', '{{ 1 is equalto 1 }} {{ 1 is eq 1.0 }} {{ "a" is eq "b" }} {{ 1 is equalto "1" }}')
c('test-equalto-args', '{{ 1 is equalto(1) }}')
c('test-unknown', '{{ 1 is frob }}')
c('test-not-precedence', '{{ not 1 is odd }} {{ 2 is not odd }}')
c('test-mapping-kwargs-like', '{{ obj is mapping }} {{ obj.d is iterable }}', obj)

// --- if ---
c('if-elif-else', '{% for x in [1, 2, 3] %}{% if x == 1 %}one{% elif x == 2 %}two{% else %}other{% endif %};{% endfor %}')
c('if-truthiness', '{% for v in [0, 1, "", "a", [], [0], {}, none] %}{% if v %}T{% else %}F{% endif %}{% endfor %}', undefined)
c('if-object-truthiness', '{% if {"a": 1} %}T{% endif %}{% if {} %}T{% else %}F{% endif %}{% if namespace() %}N{% endif %}{% if range %}R{% endif %}')
c('if-undefined', '{% if missing %}T{% else %}F{% endif %}{% if missing.x %}T{% else %}F{% endif %}')
c('if-nested', '{% if true %}{% if false %}a{% elif true %}b{% endif %}{% endif %}')
c('if-elif-chain-trim', '{%- if false -%} a {%- elif false -%} b {%- else -%} c {%- endif -%}')
// JS renders nesting until its stack runs out; past the Rust depth bounds (max_depth 128) the template is declined.
const elifs = (count) => `{% if false %}a${'{% elif false %}a'.repeat(count)}{% else %}b{% endif %}`
c('if-elif-chain-120', elifs(120))
c('if-elif-chain-130', elifs(130), undefined, { decline: 'jinja-depth' })
c('depth-filter-chain', `{{ 1${'|string'.repeat(600)} }}`, undefined, { decline: 'jinja-depth' })
const countdown = (count) => `{% macro f(n) %}{% if n > 0 %}{{ f(n - 1) }}{% else %}done{% endif %}{% endmacro %}{{ f(${count}) }}`
c('depth-macro-recursion-200', countdown(200))
c('depth-macro-recursion-300', countdown(300), undefined, { decline: 'jinja-depth' })
c('depth-tojson', '{% set ns = namespace(a=[]) %}{% for i in range(200) %}{% set ns.a = [ns.a] %}{% endfor %}{{ ns.a|tojson }}', undefined, { decline: 'jinja-depth' })
c('if-missing-condition', '{% if %}x{% endif %}')
c('if-else-twice', '{% if true %}a{% else %}b{% else %}c{% endif %}')

// --- for ---
c('for-basic', '{% for x in [1, 2, 3] %}{{ x }},{% endfor %}')
c('for-loop-vars', '{% for x in ["a", "b", "c"] %}{{ loop.index }}/{{ loop.index0 }}/{{ loop.revindex }}/{{ loop.revindex0 }}/{{ loop.first }}/{{ loop.last }}/{{ loop.length }}/{{ loop.previtem }}/{{ loop.nextitem }};{% endfor %}')
c('for-loop-object', '{% for x in [1] %}{{ loop }}{% endfor %}')
c('for-else', '{% for x in [] %}x{% else %}empty{% endfor %}')
c('for-else-filtered', '{% for x in [1, 2] if x > 5 %}x{% else %}none{% endfor %}')
c('for-if-filter', '{% for x in [1, 2, 3, 4] if x is even %}{{ x }}{{ loop.index }}{{ loop.length }},{% endfor %}')
c('for-if-filter-uses-loop', '{% for x in [1, 2] if loop %}{{ x }}{% endfor %}')
c('for-tuple', '{% for a, b in [[1, 2], [3, 4]] %}{{ a }}{{ b }};{% endfor %}')
c('for-tuple-paren', '{% for (a, b) in [[1, 2]] %}{{ a }}{{ b }}{% endfor %}')
c('for-tuple-mismatch', '{% for a, b in [[1, 2, 3]] %}{% endfor %}')
c('for-tuple-not-array', '{% for a, b in ["ab"] %}{{ a }}{% endfor %}')
c('for-tuple-tuple-item', '{% for a, b in [(1, 2)] %}{{ a }}{% endfor %}')
c('for-object-keys', '{% for k in {"b": 1, "a": 2} %}{{ k }}{% endfor %}')
c('for-items', '{% for k, v in {"b": 1, "a": 2}.items() %}{{ k }}{{ v }}{% endfor %}')
c('for-string', '{% for ch in "abc" %}{{ ch }}{% endfor %}')
c('for-number', '{% for x in 3 %}{{ x }}{% endfor %}')
c('for-undefined', '{% for x in missing %}{{ x }}{% endfor %}')
c('for-none', '{% for x in none %}{{ x }}{% endfor %}')
c('for-break-continue', '{% for x in range(10) %}{% if x == 2 %}{% continue %}{% endif %}{% if x == 5 %}{% break %}{% endif %}{{ x }}{% endfor %}')
c('for-break-partial-output', '{% for x in [1, 2, 3] %}a{{ x }}{% if x == 2 %}{% break %}{% endif %}b{% endfor %}')
c('for-continue-partial-output', '{% for x in [1, 2] %}a{% continue %}b{% endfor %}c')
c('for-break-all-else', '{% for x in [1] %}{% break %}{% else %}else{% endfor %}')
c('for-continue-all-else', '{% for x in [1, 2] %}{% continue %}{% else %}else{% endfor %}')
c('for-break-outside', '{% break %}')
c('for-continue-outside', 'a{% if true %}{% continue %}{% endif %}')
c('for-nested', '{% for a in [1, 2] %}{% for b in ["x", "y"] %}{{ a }}{{ b }}{{ loop.index }}{% endfor %}|{{ loop.index }};{% endfor %}')
c('for-nested-break', '{% for a in [1, 2] %}{% for b in [1, 2, 3] %}{% if b == 2 %}{% break %}{% endif %}{{ a }}{{ b }}{% endfor %}{% endfor %}')
c('for-scope-leak', '{% for x in [1, 2] %}{% set y = x %}{% endfor %}[{{ x }}][{{ y }}]')
c('for-loop-outside', '[{{ loop }}]')
c('for-shadow', '{% set x = "outer" %}{% for x in [1] %}{{ x }}{% endfor %}{{ x }}')
c('for-recursive-flag', '{% for x in [1] recursive %}{{ x }}{% endfor %}')
c('for-range', '{% for i in range(3) %}{{ i }}{% endfor %}|{% for i in range(1, 4) %}{{ i }}{% endfor %}|{% for i in range(10, 0, -3) %}{{ i }}{% endfor %}|{% for i in range(0, 1, 0.25) %}{{ i }},{% endfor %}')
c('for-select-break', '{% for x in [1, 2, 3, 4] if x != 2 %}{% if x == 4 %}{% break %}{% endif %}{{ x }}:{{ loop.last }};{% endfor %}')
c('for-loop-cycle', '{% for x in [1, 2] %}{{ loop.cycle("a", "b") }}{% endfor %}')
c('for-messages-roles', '{% for m in messages %}{{ m.role }}{% if not loop.last %},{% endif %}{% endfor %}', { messages: [{ role: 'user' }, { role: 'assistant' }] })
c('for-mutate-namespace', '{% set ns = namespace(n=0) %}{% for x in [1, 2, 3] %}{% set ns.n = ns.n + x %}{% endfor %}{{ ns.n }}')
c('for-in-keyword-missing', '{% for x [1] %}{% endfor %}')

// --- globals ---
c('range-forms', '{{ range(5) }} {{ range(2, 5) }} {{ range(5, 2) }} {{ range(5, 2, -1) }} {{ range(0) }} {{ range(-2) }} {{ range(1.5) }}')
c('range-zero-step', '{{ range(1, 5, 0) }}')
c('range-no-args', '{{ range() }}')
c('range-bool', '{{ range(true) }}', undefined, { decline: 'range-argument' })
c('range-string', '{{ range("3") }}', undefined, { decline: 'range-argument' })
c('range-many-args', '{{ range(0, 4, 2, 9) }}')
c('raise-exception', '{{ raise_exception("custom failure") }}')
c('raise-exception-no-args', '{{ raise_exception() }}')
c('raise-exception-number', '{{ raise_exception(42) }}')
c('raise-exception-in-loop', '{% for x in [1, 2] %}{{ x }}{% if x == 2 %}{{ raise_exception("at " ~ x) }}{% endif %}{% endfor %}')
c('raise-exception-unicode', '{{ raise_exception("오류 😀") }}')
const now = { year: 2026, month: 10, day: 11, hour: 7, minute: 5 }
c('strftime', '{{ strftime_now("%Y-%m-%d %H:%M %% %y %S") }}', undefined, { now })
c('strftime-plain', '{{ strftime_now("date") }} {{ strftime_now("%") }} {{ strftime_now("%%Y") }}', undefined, { now })
c('strftime-non-string', '{{ strftime_now(1) }}', undefined, { now })
c('strftime-without-clock', '{{ strftime_now("%Y") }}', undefined, { decline: 'strftime-now' })
c('namespace-basic', '{% set ns = namespace(a=1, b="x") %}{{ ns.a }}{{ ns.b }}[{{ ns.c }}]')
c('namespace-object', '{% set ns = namespace({"a": 1}, b=2) %}{{ ns.a }}{{ ns.b }}')
c('namespace-pairs', '{% set ns = namespace([["a", 1], "bc"]) %}{{ ns.a }}{{ ns.b }}')
c('namespace-bad-pair', '{% set ns = namespace([["a", 1, 2]]) %}')
c('namespace-non-string-key', '{% set ns = namespace([[1, 2]]) %}')
c('namespace-two-args', '{% set ns = namespace({}, {}) %}')
c('namespace-non-iterable', '{% set ns = namespace(5) %}')
c('namespace-empty', '{% set ns = namespace() %}{% set ns.x = 3 %}{{ ns.x }}')
c('namespace-override', '{% set ns = namespace({"a": 1}, a=2) %}{{ ns.a }}')
c('namespace-in-macro', '{% macro m() %}{% set ns = namespace(v=1) %}{{ ns.v }}{% endmacro %}{{ m() }}')
c('namespace-identity', '{% set a = namespace(x=1) %}{% set b = a %}{% set b.x = 2 %}{{ a.x }}{{ a == b }}{{ a == namespace(x=2) }}')
c('namespace-computed-set', '{% set ns = namespace() %}{% set ns["k"] = 1 %}')
c('namespace-member-test', '{% set ns = namespace(a=none) %}{{ ns.a is none }} {{ ns.z is defined }} {{ "a" in ns }}')
c('namespace-string-pair-surrogate', '{% set ns = namespace(["a😀"]) %}')
c('namespace-shadow', '{% set namespace = 1 %}{{ namespace }}')
c('call-non-function', '{{ "a"() }}')
c('call-undefined', '{{ missing() }}')
c('call-spread', '{{ range(*[1, 4]) }}')
c('call-spread-non-array', '{{ range(*5) }}')
c('call-keyword-spread', '{% set ns = namespace(**{"a": 1}) %}{{ ns.a }}')
c('call-keyword-spread-non-object', '{{ namespace(**[1]) }}')
c('call-duplicate-keyword', '{{ namespace(a=1, a=2) }}')
c('call-duplicate-spread-keyword', '{{ namespace(a=1, **{"a": 2}) }}')
c('call-positional-after-keyword', '{{ range(a=1, 2) }}')

// --- macros and call blocks ---
c('macro-basic', '{% macro greet(name) %}Hi {{ name }}!{% endmacro %}{{ greet("Ann") }}{{ greet() }}')
c('macro-defaults', '{% macro m(a, b="B", c=a ~ "!") %}{{ a }}{{ b }}{{ c }}{% endmacro %}{{ m("x") }}|{{ m("x", "y") }}|{{ m("x", c="z") }}|{{ m(b="q", a="p") }}')
c('macro-default-sees-earlier', '{% macro m(a, b=a + 1) %}{{ b }}{% endmacro %}{{ m(1) }}{{ m(1, 5) }}')
c('macro-too-many', '{% macro m(a) %}{{ a }}{% endmacro %}{{ m(1, 2) }}')
c('macro-unknown-keyword', '{% macro m(a) %}{{ a }}{% endmacro %}{{ m(b=1) }}')
c('macro-varargs', '{% macro m(a) %}{{ a }}{{ varargs }}{% endmacro %}{{ m(1, 2, 3) }}|{{ m(1) }}')
c('macro-kwargs', '{% macro m(a) %}{{ a }}{{ kwargs }}{% endmacro %}{{ m(1, b=2, c=3) }}|{{ m(a=1) }}')
c('macro-kwargs-nested-access', '{% macro m() %}{% if true %}{{ kwargs.x }}{% endif %}{% endmacro %}{{ m(x=5) }}')
c('macro-kwargs-in-call-arg', '{% macro m() %}{{ "x"|default(kwargs) }}{% endmacro %}{{ m(y=1) }}')
c('macro-kwargs-in-nested-macro', '{% macro outer() %}{% macro inner() %}{{ kwargs }}{% endmacro %}{{ inner(z=1) }}{% endmacro %}{{ outer() }}')
c('macro-varargs-as-param', '{% macro m(varargs) %}{{ varargs }}{% endmacro %}{{ m(1) }}')
c('macro-recursion', '{% macro fact(n) %}{% if n <= 1 %}1{% else %}{{ n * (fact(n - 1)|int) }}{% endif %}{% endmacro %}{{ fact(5) }}')
c('macro-dynamic-scope', '{% macro show() %}{{ v }}{% endmacro %}{% set v = "top" %}{{ show() }}{% for v in ["loop"] %}{{ show() }}{% endfor %}')
c('macro-set-local', '{% set x = 1 %}{% macro m() %}{% set x = 2 %}{{ x }}{% endmacro %}{{ m() }}{{ x }}')
c('macro-returns-string', '{% macro m() %}5{% endmacro %}{{ m() + 1 }}|{{ m() is string }}')
c('macro-as-value', '{% macro m() %}x{% endmacro %}{{ m is callable }}')
c('macro-render-function', '{% macro m() %}x{% endmacro %}{{ m }}', undefined, { decline: 'function-to-string' })
c('macro-redefine', '{% macro m() %}1{% endmacro %}{% macro m() %}2{% endmacro %}{{ m() }}')
c('macro-in-loop-break', '{% macro m() %}{% break %}{% endmacro %}{% for x in [1, 2] %}{{ x }}{{ m() }}{% endfor %}')
c('macro-bad-param', '{% macro m(1) %}{% endmacro %}')
c('macro-param-order', '{% macro m(a=1, b) %}{{ a }}{{ b }}{% endmacro %}{{ m() }}')
c('macro-positional-and-keyword', '{% macro m(a, b) %}{{ a }}{{ b }}{% endmacro %}{{ m(1, a=2) }}')
c('macro-kwargs-keyword-collision', '{% macro m(a) %}{{ a }}{{ kwargs }}{% endmacro %}{{ m(1, a=2) }}')
c('call-block', '{% macro wrap(tag) %}<{{ tag }}>{{ caller() }}</{{ tag }}>{% endmacro %}{% call wrap("b") %}inner{% endcall %}')
c('call-block-args', '{% macro each(items) %}{% for i in items %}{{ caller(i, loop.index) }}{% endfor %}{% endmacro %}{% call(item, n) each(["a", "b"]) %}{{ n }}={{ item }};{% endcall %}')
c('call-block-scope', '{% set v = "outer" %}{% macro m() %}{% set v = "macro" %}{{ caller() }}{% endmacro %}{% call m() %}{{ v }}{% endcall %}')
c('call-block-kwargs', '{% macro m() %}{{ kwargs }}|{{ caller() }}{% endmacro %}{% call m(a=1) %}c{% endcall %}')
c('call-block-no-kwargs', '{% macro m() %}{{ caller() }}{% endmacro %}{% call m() %}c{% endcall %}')
c('call-block-caller-defaults', '{% macro m() %}{{ caller() }}|{{ caller(9) }}{% endmacro %}{% call(x=3) m() %}{{ x }}{% endcall %}')
c('call-block-caller-too-many', '{% macro m() %}{{ caller(1) }}{% endmacro %}{% call m() %}c{% endcall %}')
c('call-block-nested', '{% macro a() %}[{{ caller() }}]{% endmacro %}{% call a() %}{% call a() %}x{% endcall %}{% endcall %}')
c('call-block-non-macro', '{% call range(3) %}x{% endcall %}', undefined, { decline: 'range-argument' })
c('call-block-caller-outside', '{{ caller() }}')

// --- misc runtime ---
c('loop-scoped-namespace', '{% set ns = namespace(found=false) %}{% for m in messages %}{% if m.role == "system" %}{% set ns.found = true %}{% endif %}{% endfor %}{{ ns.found }}', { messages: [{ role: 'user' }, { role: 'system' }] })
c('array-identity-in', '{% set a = [1] %}{{ a in [a] }} {{ [1] in [[1]] }}')
c('kwargs-value-render', '{% macro m() %}{{ kwargs is mapping }}{{ kwargs|length }}{% endmacro %}{{ m(a=1, b=2) }}')
c('deep-nesting', '{{ ' + '('.repeat(40) + '1' + ')'.repeat(40) + ' }}')
c('deep-if-nesting', '{% if true %}'.repeat(30) + 'deep' + '{% endif %}'.repeat(30))
c('long-loop', '{% for i in range(500) %}{{ i % 10 }}{% endfor %}')
c('unicode-case', '{{ "ǈ"|upper }} {{ "ﬁ"|upper }} {{ "ΣΑΣ"|lower }} {{ "İstanbul"|lower }} {{ "ß"|title }}')
c('unicode-title-nonascii', '{{ "éa àb"|title }} {{ "a_b c"|title }}')
c('whitespace-in-tags', '{%if true%}{{1}}{%endif%}')
c('newline-in-tags', '{%\nif\ntrue\n%}{{\n1\n}}{%\nendif\n%}')
c('keyword-as-identifier', '{% set if_ = 1 %}{{ if_ }}{% set in2 = 2 %}{{ in2 }}')
c('operator-word-boundaries', '{{ "a" in "abc" and not false or none }}')
c('is-defined-chain', '{{ messages[0].tool_calls is defined }}', { messages: [{ role: 'user' }] })
c('object-in-message-content', '{% for part in content %}{% if part.type == "text" %}{{ part.text }}{% endif %}{% endfor %}', { content: [{ type: 'text', text: 'hi' }, { type: 'image' }, { type: 'text', text: '!' }] })
c('mixed-content-types', '{% if content is string %}{{ content }}{% else %}{{ content|map(attribute="text")|join }}{% endif %}', { content: [{ text: 'a' }, { text: 'b' }] })

c('context-big-numbers', '{{ big }} {{ small }} {{ big is integer }} {{ neg }} {{ big + 0.5 }} {{ max }}', { big: 1e21, small: 1e-7, neg: -1.5, max: Number.MAX_SAFE_INTEGER + 2 })
c('for-kwargs', '{% macro m() %}{% for k in kwargs %}{{ k }}={{ kwargs[k] }};{% endfor %}{{ kwargs is mapping }}{% endmacro %}{{ m(b=1, a=2) }}')
c('string-in-surrogate', '{{ "\ud83d" in "😀" }} {{ "😀"|length }} {{ "😀"[0:1]|length }}')
c('set-namespace-nested', '{% set ns = namespace(o={"a": 1}) %}{{ ns.o.a }}')
c('set-namespace-deep-target', '{% set ns = namespace(o=namespace()) %}{% set ns.o.x = 1 %}{{ ns.o.x }}')
c('plus-whitespace-control', '{%+ if true %}x{% endif %}')
c('length-comparison', '{{ messages|length > 0 }} {{ messages|length == 0 }}', { messages: [] })
c('tojson-in-concat', '{{ "x" + {"a": 1}|tojson }}')
c('macro-call-filter', '{% macro m() %}abc{% endmacro %}{{ m()|upper }}{{ m()|length }}')
c('items-pairs', '{% for pair in {"a": 1}.items() %}{{ pair[0] }}{{ pair[1] }}{{ pair|length }}{% endfor %}{{ {"a": 1}.items()|list|length }}')
c('is-none-chain', '{{ x is none or x is not defined }} {{ y is none or y is not defined }}', { x: null })
c('filter-default-chain', '{{ x|default(y)|default("z") }}')
c('ternary-filter', '{{ (x if x else "no")|upper }}')
c('filter-format', '{{ "%s"|format(1) }}')
c('filter-escape', '{{ "<"|e }}')
c('test-string-and-length', '{% if x is string and x|length %}yes{% else %}no{% endif %}', { x: 'a' })
c('nested-macro-closure', '{% macro outer(a) %}{% macro inner() %}{{ a }}{% endmacro %}{{ inner() }}{% endmacro %}{{ outer(7) }}{{ inner is defined }}')
c('loop-index-math', '{% for m in messages %}{{ loop.index0 % 2 == 0 }}{{ (loop.revindex - 1) * 2 }}{% endfor %}', { messages: [1, 2, 3] })
c('last-message', '{{ messages[-1]["content"] }}{{ messages[-1].role == "user" }}', { messages: [{ role: 'user', content: 'end' }] })
c('array-mutation-free', '{% set a = [1] %}{% set b = a + [2] %}{{ a }}{{ b }}')
c('object-get-in-loop', '{% for m in messages %}{{ m.get("name", "anon") }};{% endfor %}', { messages: [{ name: 'n1' }, {}] })
c('string-index-loop', '{% for i in range(3) %}{{ "abc"[i] }}{% endfor %}')
c('split-index', '{{ "a:b:c".split(":")[1] }} {{ "a:b:c".split(":")[-1] }} {{ "a:b".split(":", 1)|length }}')
c('replace-newlines', '{{ content.replace("\n", " ").replace("  ", " ") }}', { content: 'a\n\nb\nc' })
c('strip-trim-unicode', '[{{ "\ufeff x \u2029"|trim }}][{{ "\u200b x".strip() }}]')
c('deep-object-tojson-indent', '{{ {"a": {"b": {"c": [1, [2, []], {}]}}}|tojson(indent=1) }}')
c('tojson-negative-zero-float', '{{ -0.0|tojson }} {{ (0 * -1)|tojson }} {{ [0.5, -2.0]|tojson }}')

// --- synthetic chat templates in the shape of published model templates ---
const chats = {
    basic: { messages: [{ role: 'system', content: 'You are terse.' }, { role: 'user', content: 'Hi' }, { role: 'assistant', content: 'Hello.' }, { role: 'user', content: '  Tell me more  ' }], add_generation_prompt: true, bos_token: '<s>', eos_token: '</s>' },
    nosystem: { messages: [{ role: 'user', content: 'Q1' }, { role: 'assistant', content: 'A1' }, { role: 'user', content: 'Q2' }], add_generation_prompt: false, bos_token: '<s>', eos_token: '</s>' },
    broken: { messages: [{ role: 'user', content: 'a' }, { role: 'user', content: 'b' }], add_generation_prompt: true, bos_token: '', eos_token: '' },
    unicode: { messages: [{ role: 'user', content: '한국어 질문 😀\n두 번째 줄' }, { role: 'assistant', content: 'Réponse « ok »' }, { role: 'user', content: '"quoted" \\ back' }], add_generation_prompt: true, bos_token: '', eos_token: '' },
    empty: { messages: [], add_generation_prompt: true, bos_token: '<s>', eos_token: '</s>' },
}
const chatml = "{% for message in messages %}{{'<|im_start|>' + message['role'] + '\\n' + message['content'] + '<|im_end|>' + '\\n'}}{% endfor %}{% if add_generation_prompt %}{{ '<|im_start|>assistant\\n' }}{% endif %}"
each('chatml', chatml, chats)
const llama3 = "{% set loop_messages = messages %}{% for message in loop_messages %}{% set content = '<|start_header_id|>' + message['role'] + '<|end_header_id|>\\n\\n'+ message['content'] | trim + '<|eot_id|>' %}{% if loop.index0 == 0 %}{% set content = bos_token + content %}{% endif %}{{ content }}{% endfor %}{% if add_generation_prompt %}{{ '<|start_header_id|>assistant<|end_header_id|>\\n\\n' }}{% endif %}"
each('llama3', llama3, chats)
const llama2 = "{% if messages[0]['role'] == 'system' %}{% set loop_messages = messages[1:] %}{% set system_message = messages[0]['content'] %}{% else %}{% set loop_messages = messages %}{% set system_message = false %}{% endif %}{% for message in loop_messages %}{% if (message['role'] == 'user') != (loop.index0 % 2 == 0) %}{{ raise_exception('Conversation roles must alternate user/assistant/user/assistant/...') }}{% endif %}{% if loop.index0 == 0 and system_message != false %}{% set content = '<<SYS>>\\n' + system_message + '\\n<</SYS>>\\n\\n' + message['content'] %}{% else %}{% set content = message['content'] %}{% endif %}{% if message['role'] == 'user' %}{{ bos_token + '[INST] ' + content.strip() + ' [/INST]' }}{% elif message['role'] == 'assistant' %}{{ ' '  + content.strip() + ' ' + eos_token }}{% endif %}{% endfor %}"
each('llama2', llama2, chats)
const mistral = "{{ bos_token }}{% for message in messages %}{% if (message['role'] == 'user') != (loop.index0 % 2 == 0) %}{{ raise_exception('Conversation roles must alternate user/assistant/user/assistant/...') }}{% endif %}{% if message['role'] == 'user' %}{{ '[INST] ' + message['content'] + ' [/INST]' }}{% elif message['role'] == 'assistant' %}{{ message['content'] + eos_token}}{% else %}{{ raise_exception('Only user and assistant roles are supported!') }}{% endif %}{% endfor %}"
each('mistral', mistral, chats)
const gemma = "{{ bos_token }}{% if messages[0]['role'] == 'system' %}{{ raise_exception('System role not supported') }}{% endif %}{% for message in messages %}{% if (message['role'] == 'user') != (loop.index0 % 2 == 0) %}{{ raise_exception('Conversation roles must alternate user/assistant/user/assistant/...') }}{% endif %}{% if (message['role'] == 'assistant') %}{% set role = 'model' %}{% else %}{% set role = message['role'] %}{% endif %}{{ '<start_of_turn>' + role + '\\n' + message['content'] | trim + '<end_of_turn>\\n' }}{% endfor %}{% if add_generation_prompt %}{{'<start_of_turn>model\\n'}}{% endif %}"
each('gemma', gemma, chats)
const phi3 = "{% for message in messages %}{% if message['role'] == 'system' %}{{'<|system|>\\n' + message['content'] + '<|end|>\\n'}}{% elif message['role'] == 'user' %}{{'<|user|>\\n' + message['content'] + '<|end|>\\n'}}{% elif message['role'] == 'assistant' %}{{'<|assistant|>\\n' + message['content'] + '<|end|>\\n'}}{% endif %}{% endfor %}{% if add_generation_prompt %}{{ '<|assistant|>\\n' }}{% else %}{{ eos_token }}{% endif %}"
each('phi3', phi3, chats)
const reasoning = "{% if not add_generation_prompt is defined %}{% set add_generation_prompt = false %}{% endif %}{% set ns = namespace(is_first=false, is_tool=false, system_prompt='') %}{%- for message in messages %}{%- if message['role'] == 'system' %}{% set ns.system_prompt = message['content'] %}{%- endif %}{%- endfor %}{{ bos_token }}{{ ns.system_prompt }}{%- for message in messages %}{%- if message['role'] == 'user' %}{{'<｜User｜>' + message['content']}}{%- endif %}{%- if message['role'] == 'assistant' and message['content'] is not none %}{% set content = message['content'] %}{% if '</think>' in content %}{% set content = content.split('</think>')[-1] %}{% endif %}{{'<｜Assistant｜>' + content + '<｜end▁of▁sentence｜>'}}{%- endif %}{%- endfor %}{% if add_generation_prompt %}{{'<｜Assistant｜><think>\\n'}}{% endif %}"
each('reasoning', reasoning, { ...chats, think: { messages: [{ role: 'user', content: 'q' }, { role: 'assistant', content: '<think>hidden</think>shown' }, { role: 'assistant', content: null }], bos_token: '<B>' } })

const toolTemplate = String.raw`{%- if tools %}
    {{- '<|im_start|>system\n' }}
    {%- if messages[0]['role'] == 'system' %}
        {{- messages[0]['content'] }}
    {%- else %}
        {{- 'You are a helpful assistant.' }}
    {%- endif %}
    {{- "\n\n# Tools\n\nYou may call one or more functions to assist with the user query.\n\nYou are provided with function signatures within <tools></tools> XML tags:\n<tools>" }}
    {%- for tool in tools %}
        {{- "\n" }}
        {{- tool | tojson }}
    {%- endfor %}
    {{- "\n</tools>\n\nFor each function call, return a json object with function name and arguments within <tool_call></tool_call> XML tags:\n<tool_call>\n{\"name\": <function-name>, \"arguments\": <args-json-object>}\n</tool_call><|im_end|>\n" }}
{%- else %}
    {%- if messages[0]['role'] == 'system' %}
        {{- '<|im_start|>system\n' + messages[0]['content'] + '<|im_end|>\n' }}
    {%- else %}
        {{- '<|im_start|>system\nYou are a helpful assistant.<|im_end|>\n' }}
    {%- endif %}
{%- endif %}
{%- for message in messages %}
    {%- if (message.role == "user") or (message.role == "system" and not loop.first) or (message.role == "assistant" and not message.tool_calls) %}
        {{- '<|im_start|>' + message.role + '\n' + message.content + '<|im_end|>' + '\n' }}
    {%- elif message.role == "assistant" %}
        {{- '<|im_start|>' + message.role }}
        {%- if message.content %}
            {{- '\n' + message.content }}
        {%- endif %}
        {%- for tool_call in message.tool_calls %}
            {%- if tool_call.function is defined %}
                {%- set tool_call = tool_call.function %}
            {%- endif %}
            {{- '\n<tool_call>\n{"name": "' }}
            {{- tool_call.name }}
            {{- '", "arguments": ' }}
            {{- tool_call.arguments | tojson }}
            {{- '}\n</tool_call>' }}
        {%- endfor %}
        {{- '<|im_end|>\n' }}
    {%- elif message.role == "tool" %}
        {%- if (loop.index0 == 0) or (messages[loop.index0 - 1].role != "tool") %}
            {{- '<|im_start|>user' }}
        {%- endif %}
        {{- '\n<tool_response>\n' }}
        {{- message.content }}
        {{- '\n</tool_response>' }}
        {%- if loop.last or (messages[loop.index0 + 1].role != "tool") %}
            {{- '<|im_end|>\n' }}
        {%- endif %}
    {%- endif %}
{%- endfor %}
{%- if add_generation_prompt %}
    {{- '<|im_start|>assistant\n' }}
{%- endif %}
`
const weatherTool = { type: 'function', function: { name: 'get_weather', description: 'Weather for a city', parameters: { type: 'object', properties: { city: { type: 'string', description: 'City name' }, unit: { type: 'string', enum: ['c', 'f'] } }, required: ['city'] } } }
const toolChats = {
    tools: { tools: [weatherTool], messages: [{ role: 'user', content: 'Weather in Seoul?' }, { role: 'assistant', content: '', tool_calls: [{ type: 'function', function: { name: 'get_weather', arguments: { city: 'Seoul', unit: 'c' } } }] }, { role: 'tool', content: '{"temp": 21}' }, { role: 'tool', content: 'second' }, { role: 'assistant', content: 'It is 21C.' }], add_generation_prompt: true },
    'tools-system': { tools: [weatherTool, { type: 'function', function: { name: 'noop', parameters: {} } }], messages: [{ role: 'system', content: 'Sys' }, { role: 'user', content: 'u' }], add_generation_prompt: false },
    'no-tools': { messages: [{ role: 'system', content: 'Sys' }, { role: 'user', content: 'u' }, { role: 'assistant', content: 'a' }, { role: 'system', content: 'late' }], add_generation_prompt: true },
    'no-tools-no-system': { messages: [{ role: 'user', content: 'u' }], tools: [], add_generation_prompt: true },
    'tool-call-without-function': { tools: [weatherTool], messages: [{ role: 'user', content: 'x' }, { role: 'assistant', content: 'calling', tool_calls: [{ name: 'direct', arguments: '{"a":1}' }] }], add_generation_prompt: false },
}
each('qwen-tools', toolTemplate, toolChats)

const llama31 = String.raw`{{- bos_token }}
{%- if custom_tools is defined %}
    {%- set tools = custom_tools %}
{%- endif %}
{%- if not tools_in_user_message is defined %}
    {%- set tools_in_user_message = true %}
{%- endif %}
{%- if not date_string is defined %}
    {%- if strftime_now is defined %}
        {%- set date_string = strftime_now("%d %m %Y") %}
    {%- else %}
        {%- set date_string = "26 Jul 2024" %}
    {%- endif %}
{%- endif %}
{%- if not tools is defined %}
    {%- set tools = none %}
{%- endif %}
{%- if messages[0]['role'] == 'system' %}
    {%- set system_message = messages[0]['content']|trim %}
    {%- set messages = messages[1:] %}
{%- else %}
    {%- set system_message = "" %}
{%- endif %}
{{- "<|start_header_id|>system<|end_header_id|>\n\n" }}
{%- if tools is not none %}
    {{- "Environment: ipython\n" }}
{%- endif %}
{{- "Cutting Knowledge Date: December 2023\n" }}
{{- "Today Date: " + date_string + "\n\n" }}
{%- if tools is not none and not tools_in_user_message %}
    {{- "You have access to the following functions.\n\n" }}
    {%- for t in tools %}
        {{- t | tojson(indent=4) }}
        {{- "\n\n" }}
    {%- endfor %}
{%- endif %}
{{- system_message }}
{{- "<|eot_id|>" }}
{%- if tools_in_user_message and not tools is none %}
    {%- if messages | length != 0 %}
        {%- set first_user_message = messages[0]['content']|trim %}
        {%- set messages = messages[1:] %}
    {%- else %}
        {{- raise_exception("Cannot put tools in the first user message when there's no first user message!") }}
    {%- endif %}
    {{- '<|start_header_id|>user<|end_header_id|>\n\n' -}}
    {{- "Given the following functions, please respond with a JSON for a function call " }}
    {{- "with its proper arguments that best answers the given prompt.\n\n" }}
    {%- for t in tools %}
        {{- t | tojson(indent=4) }}
        {{- "\n\n" }}
    {%- endfor %}
    {{- first_user_message + "<|eot_id|>"}}
{%- endif %}
{%- for message in messages %}
    {%- if not (message.role == 'ipython' or message.role == 'tool' or 'tool_calls' in message) %}
        {{- '<|start_header_id|>' + message['role'] + '<|end_header_id|>\n\n'+ message['content'] | trim + '<|eot_id|>' }}
    {%- elif 'tool_calls' in message %}
        {%- if not message.tool_calls|length == 1 %}
            {{- raise_exception("This model only supports single tool-calls at once!") }}
        {%- endif %}
        {%- set tool_call = message.tool_calls[0].function %}
        {{- '<|start_header_id|>assistant<|end_header_id|>\n\n' -}}
        {{- '{"name": "' + tool_call.name + '", ' }}
        {{- '"parameters": ' }}
        {{- tool_call.arguments | tojson }}
        {{- "}" }}
        {{- "<|eot_id|>" }}
    {%- elif message.role == "tool" or message.role == "ipython" %}
        {{- "<|start_header_id|>ipython<|end_header_id|>\n\n" }}
        {%- if message.content is mapping or message.content is iterable %}
            {{- message.content | tojson }}
        {%- else %}
            {{- message.content }}
        {%- endif %}
        {{- "<|eot_id|>" }}
    {%- endif %}
{%- endfor %}
{%- if add_generation_prompt %}
    {{- '<|start_header_id|>assistant<|end_header_id|>\n\n' }}
{%- endif %}
`
for (const [name, context] of Object.entries({
    basic: { ...chats.basic, date_string: '11 Oct 2026' },
    tools: { ...toolChats.tools, messages: [{ role: 'system', content: ' S ' }, { role: 'user', content: 'Weather?' }, { role: 'assistant', tool_calls: [{ function: { name: 'get_weather', arguments: { city: 'Seoul' } } }] }, { role: 'tool', content: { temp: 21 } }, { role: 'ipython', content: 'plain' }], date_string: '1 Jan 2026' },
    'tools-system-prompt': { ...toolChats['tools-system'], tools_in_user_message: false, date_string: 'd' },
    'tools-no-user': { tools: [weatherTool], messages: [{ role: 'system', content: 'only system' }], date_string: 'd' },
    'two-tool-calls': { messages: [{ role: 'user', content: 'u' }, { role: 'assistant', tool_calls: [{ function: { name: 'a', arguments: {} } }, { function: { name: 'b', arguments: {} } }] }], date_string: 'd' },
    clock: { messages: [{ role: 'user', content: 'u' }], add_generation_prompt: true },
})) c(`llama31-${name}`, llama31, context, name === 'clock' ? { now } : {})

writeFileSync(join(dir, 'jinja.cases.json'), `{"schema": 1, "cases": [\n${cases.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`)
console.log('jinja', cases.length)
