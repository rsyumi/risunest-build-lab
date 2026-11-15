// Writes the synthetic case file of the Markdown render oracle (tests/fixtures/rust-core/render/markdown.cases.json).
// Add cases here, run `node benchmarks/rust-core-oracles/render-markdown.cases.mjs`, then regenerate the expected file
// with RUST_CORE_ORACLE_WRITE=1.
//
// A case is { id, source, decline?, caseUnicode? }. `source` is the Markdown text, or its UTF-16 units when not
// well-formed. The id prefix before the first '-' is the case's category. `decline` names the `Unsupported` the Rust
// renderer must answer for both engines instead of a result. `caseUnicode` (`before16`, `unicode17`, `from18` or
// `unknown`) renders for a JS engine with other case tables than the recording's Unicode 16 ones.
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/render/', import.meta.url))
mkdirSync(dir, { recursive: true })

const str = (value) => value.isWellFormed() ? value : { u: [...Array(value.length).keys()].map(i => value.charCodeAt(i)) }
const cases = []
const ids = new Set()
function c(id, source, decline) {
    if (ids.has(id)) throw new Error(`duplicate ${id}`)
    ids.add(id)
    const entry = { id, source: str(source) }
    if (decline !== undefined) entry.decline = decline
    cases.push(entry)
}

// --- paragraphs, line breaks and blank lines ---
c('block-empty', '')
c('block-spaces-only', '   ')
c('block-newlines-only', '\n\n\n')
c('block-paragraph', 'Hello world')
c('block-two-paragraphs', 'first\n\nsecond')
c('block-soft-break', 'one\ntwo\nthree')
c('block-hard-break-spaces', 'one  \ntwo')
c('block-hard-break-many-spaces', 'one      \ntwo')
c('block-hard-break-backslash', 'one\\\ntwo')
c('block-trailing-spaces-end', 'end  ')
c('block-leading-spaces', '   indented three')
c('block-four-space-indent', '    not code here')
c('block-tab-indent', '\tstill a paragraph')
c('block-crlf', 'a\r\nb\r\n\r\nc')
c('block-cr-only', 'a\rb\r\rc')
c('block-nul', 'a\u0000b')
c('block-trailing-whitespace-line', 'a\n   ')
c('block-blank-line-spaces', 'a\n   \nb')
c('block-backslash-space', 'a\\ b')
c('block-backslash-space-hard', 'a\\  \nb')
c('block-backslash-end', 'ends with \\')

// --- headings ---
c('heading-atx', '# One\n## Two\n### Three\n#### Four\n##### Five\n###### Six')
c('heading-seven', '####### seven')
c('heading-no-space', '#5 bolt')
c('heading-closing', '## Title ##\n# Title #####\n### Title ### b')
c('heading-escaped-hash', '\\## not')
c('heading-empty', '#\n## \n### ###')
c('heading-indented', '   # three spaces')
c('heading-inline', '# *em* and `code`')
c('heading-setext', 'Title\n=====\n\nSub\n---')
c('heading-setext-multiline', 'line one\nline two\n===')
c('heading-setext-lazy', '> quote\n===')
c('heading-setext-indented-underline', 'Title\n   ===')
c('heading-hash-in-middle', 'a # b')

// --- thematic breaks ---
c('hr-basic', '***\n---\n___')
c('hr-spaced', ' - - -\n**  * ** *')
c('hr-two-chars', '--\n**')
c('hr-mixed', '*-*')
c('hr-between', 'para\n***\npara')
c('hr-dash-after-para', 'para\n---\nnext')

// --- fences ---
c('fence-plain', '```\ncode <b>&amp;\n```')
c('fence-lang', '```js\nconst a = 1 < 2\n```')
c('fence-lang-attrs', '```python {.class} extra\nprint("x")\n```')
c('fence-tilde', '~~~\ntilde\n~~~')
c('fence-tilde-lang-backtick', '~~~ a`b\nx\n~~~')
c('fence-backtick-info-backtick', '``` a`b\nx\n```')
c('fence-unclosed', '```\nnever closed\nstill code')
c('fence-longer-close', '```\na\n`````\nafter')
c('fence-shorter-close', '````\na\n```\nb\n````')
c('fence-indented', '  ```\n  indented\n    more\n  ```')
c('fence-in-list', '- item\n\n  ```sh\n  ls -la\n  ```')
c('fence-in-quote', '> ```\n> quoted code\n> ```')
c('fence-empty', '```\n```')
c('fence-info-escape', '```c\\+\\+\nint\n```')
c('fence-info-entity', '```&lt;x&gt;\nx\n```')
c('fence-info-quote', '```a"b\n<x>\n```')
c('fence-interrupts-paragraph', 'text\n```\ncode\n```')
c('fence-lang-unicode', '```한국어\n코드\n```')
c('fence-lang-uppercase', '```JSON\n{"a": 1}\n```')
c('fence-html-content', '```html\n<div class="x">{{char}}</div>\n```')
c('fence-two-backticks', '``\nnot a fence\n``')

// --- blockquotes ---
c('quote-basic', '> quoted\n> lines')
c('quote-lazy', '> first\nlazy continuation')
c('quote-nested', '> a\n>> b\n> > > c')
c('quote-no-space', '>tight')
c('quote-tab', '>\tTabbed')
c('quote-empty', '>\n>\n')
c('quote-with-list', '> - one\n> - two')
c('quote-with-heading', '> # Heading\n> body')
c('quote-break', '> a\n\n> b')
c('quote-deep', '>'.repeat(120) + ' deep')
c('quote-in-list', '1. > quote\n   > more')
c('quote-hr-ends', '> a\n---')

// --- lists ---
c('list-bullet', '- a\n- b\n- c')
c('list-star-plus', '* a\n+ b\n* c')
c('list-ordered', '1. one\n2. two\n3. three')
c('list-ordered-start', '7. seven\n8. eight')
c('list-ordered-paren', '1) a\n2) b')
c('list-ordered-zero', '0. zero\n009. nine')
c('list-ordered-large', '123456789. big\n1234567890. too big')
c('list-loose', '- a\n\n- b')
c('list-tight-nested', '- a\n  - b\n    - c\n- d')
c('list-loose-nested', '- a\n\n  para\n- b')
c('list-continuation', '- item\n  continued\nlazy')
c('list-empty-item', '-\n- b\n-')
c('list-empty-start', '-\n  foo')
c('list-interrupt-paragraph', 'para\n- item')
c('list-ordered-interrupt-non-one', 'para\n2. not list')
c('list-ordered-interrupt-one', 'para\n1. list')
c('list-change-marker', '- a\n* b\n+ c')
c('list-indent-content', '-    four spaces\n-     five')
c('list-tab-after-marker', '-\tTab\n1.\tTab')
c('list-deep', Array.from({ length: 60 }, (_, i) => `${'  '.repeat(i)}- level ${i}`).join('\n'))
c('list-hr-inside', '- a\n- * * *')
c('list-heading-inside', '- # head\n- b')
c('list-blank-first-line', '- \n  a')
c('list-numbered-dialogue', '1. "Hello," she said.\n2. *He nods.*\n3. **End.**')

// --- tables ---
c('table-basic', '| a | b |\n| - | - |\n| 1 | 2 |')
c('table-align', '| l | c | r | n |\n|:--|:-:|--:|---|\n| 1 | 2 | 3 | 4 |')
c('table-no-outer-pipes', 'a | b\n--|--\n1 | 2')
c('table-escaped-pipe', '| a \\| b | c |\n|---|---|\n| `x\\|y` | z |')
c('table-short-row', '| a | b | c |\n|---|---|---|\n| 1 |')
c('table-long-row', '| a |\n|---|\n| 1 | 2 | 3 |')
c('table-header-only', '| a | b |\n|---|---|')
c('table-mismatch', '| a | b |\n|---|\n| 1 | 2 |')
c('table-inline', '| *em* | **st** |\n|---|---|\n| [l](u) | `c` |')
c('table-ends-blank', '| a |\n|---|\n| 1 |\n\nafter')
c('table-ends-quote', '| a |\n|---|\n| 1 |\n> q')
c('table-single-column-no-pipe', 'a\n-\nb')
c('table-indented-four', '    | a |\n    |---|')
c('table-in-paragraph', 'text\n| a |\n|---|\n| 1 |')
c('table-empty-cells', '| | |\n|-|-|\n| | |')
c('table-hangul', '| 이름 | 나이 |\n|:---:|---:|\n| 철수 | 20 |')

// --- HTML blocks ---
c('html-div', '<div>\n*not em*\n</div>\n\n*em*')
c('html-div-inline-md', '<div>*x*</div>')
c('html-script', '<script>\nlet a = "<b>";\n</script>\nafter')
c('html-style', '<style>\n.a { color: red }\n\n.b {}\n</style>\n*after*')
c('html-pre', '<pre>\n  keep *this*\n</pre>')
c('html-textarea', '<textarea>\n*x*\n\n</textarea>')
c('html-comment', '<!-- note\n\nstill comment -->\nafter')
c('html-processing', '<?php echo 1; ?>\ntext')
c('html-declaration', '<!DOCTYPE html>\ntext')
c('html-cdata', '<![CDATA[\nraw\n]]>\ntext')
c('html-block-named-close', '</div>\ntext')
c('html-custom-tag-block', '<custom-tag attr="1">\n\n*md*')
c('html-custom-tag-interrupt', 'para\n<custom-tag>\nmore')
c('html-block-name-interrupt', 'para\n<div>\nmore')
c('html-uppercase', '<DIV CLASS="x">\nhi\n</DIV>')
c('html-table-tags', '<table><tr><td>\ncell\n</td></tr></table>')
c('html-details', '<details>\n<summary>Open</summary>\n\n**inside**\n\n</details>')
c('html-indented', '   <div>\nx\n</div>')
c('html-in-quote', '> <div>\n> x\n> </div>')
c('html-in-list', '- <div>\n  x\n  </div>')
c('html-self-closing', '<br/>\ntext\n<img src="a.png"/>')
c('html-attr-line', '<div\nclass="x">\nbody</div>')
// U+017F and U+212A fold to s and k only under Unicode case folding, which the `i` flag without `u` does not apply.
c('html-unicode-fold-names', '<ſcript>\n*a*\n</ſcript>\n\n<ſtyle>\n*b*\n\n<pre>\nkeep\n</pre>\n\n<Kbd>\n\n<ſection>\n*c*\n</ſection>')
c('html-script-unicode-close', '<script>\n*a*\n</ſcript>\n*b*\n</script>\n*after*')

// --- references ---
c('ref-basic', '[foo]: /url "title"\n\n[foo]')
c('ref-full', '[bar]: /b\n\n[text][bar]')
c('ref-collapsed', '[baz]: /z\n\n[baz][]')
c('ref-case-ascii', '[FoO]: /f\n\n[fOo] and [FOO][]')
c('ref-missing', '[nope]\n\n[nope][x]')
c('ref-title-single', "[a]: /u 'single'\n\n[a]")
c('ref-title-paren', '[a]: /u (paren)\n\n[a]')
c('ref-title-next-line', '[a]: /u\n"next"\n\n[a]')
c('ref-title-multiline', '[a]: /u "one\ntwo"\n\n[a]')
c('ref-angle', '[a]: <with space>\n\n[a]')
c('ref-duplicate', '[a]: /first\n[a]: /second\n\n[a]')
c('ref-whitespace-label', '[  spaced   label ]: /s\n\n[spaced label]')
c('ref-escape', '[a\\]b]: /e\n\n[a\\]b]')
c('ref-not-in-paragraph', 'para\n[a]: /u\n\n[a]')
c('ref-in-quote', '> [q]: /q\n\n[q]')
c('ref-image', '[img]: /i.png "T"\n\n![alt][img]')
c('ref-invalid-title', '[a]: /u "title" junk\n\n[a]')
c('ref-empty-dest', '[a]: <>\n\n[a]')
c('ref-hangul', '[참고]: /ref\n\n[참고] 문서')
c('ref-javascript', '[x]: javascript:alert(1)\n\n[x]')
c('ref-only', '[a]: /u')
c('ref-entity-label', '[&ouml;]: /o\n\n[&ouml;]')

// --- emphasis ---
c('em-star', '*em* and **strong**')
c('em-underscore', '_em_ and __strong__')
c('em-triple', '***both*** and ___both___')
c('em-nested', '*a **b** c*')
c('em-nested-reverse', '**a *b* c**')
c('em-intraword-star', 'foo*bar*baz')
c('em-intraword-underscore', 'foo_bar_baz snake_case_name')
c('em-space-after', '* not em*')
c('em-space-before', '*not em *')
c('em-rule-of-three', '*foo**bar**baz*')
c('em-rule-of-three-b', '*foo**bar*')
c('em-unbalanced', '**open only\n*one')
c('em-many', '*a* *b* *c* *d* *e*')
c('em-punctuation', '*"quoted"*')
c('em-punct-flank', 'a*"b"*c')
c('em-alternating', '*_'.repeat(50))
c('em-long-run', '*'.repeat(80))
c('em-quad', '****bold****')
c('em-mixed-markers', '*a _b* c_')
c('em-across-lines', '*one\ntwo*')
c('em-hangul-adjacent', '**강조**했다 그리고 *기울임*체')
c('em-hangul-intraword', '한*국*어 와 한**국**어')
c('em-cjk-punct', '「*引用*」と**「強調」**')
c('em-emoji', '*😀* **🎉party🎉**')
c('em-escaped', '\\*not em\\*')
c('em-in-code', '`*not*` *yes*')
c('strike-basic', '~~strike~~')
c('strike-single', '~not~')
c('strike-triple', '~~~a~~~')
c('strike-nested-em', '~~*a*~~ and *~~b~~*')
c('strike-unbalanced', '~~open')
c('strike-odd-lone', '~~~a~~')
c('strike-intraword', 'a~~b~~c')
c('strike-triple-inline', 'x ~~~a~~~ y')
c('strike-lone-close', 'x ~~a~~~ y')

// --- code spans ---
c('code-basic', '`code`')
c('code-double', '``a ` b``')
c('code-strip-space', '` a `')
c('code-one-space', '` `')
c('code-two-spaces', '`  `')
c('code-newline', '`a\nb`')
c('code-unmatched', '`a ``b')
c('code-html', '`<b>&amp;</b>`')
c('code-backslash', '`a\\`b`')
c('code-many-unmatched', '`'.repeat(30) + 'x')
c('code-line-separator', '` a b `')
c('code-precedence', '*a `*` b*')

// --- links and images ---
c('link-inline', '[text](/url)')
c('link-title', '[t](/u "title") [t](/u \'single\') [t](/u (paren))')
c('link-angle', '[t](<with space>)')
c('link-empty', '[t]() [](/u)')
c('link-nested-parens', '[t](/a(b)c)')
c('link-unbalanced-parens', '[t](/a(b)')
c('link-escaped', '[t](/a\\)b)')
c('link-entity-dest', '[t](/a&amp;b) [t](&notit;)')
c('link-emphasis-text', '[*em* `c`](/u)')
c('link-nested-link', '[a [b](/b) c](/a)')
c('link-image-inside', '[![alt](/i.png)](/target)')
c('link-spaces-around', '[t](  /u   "t"  )')
c('link-newline-in-dest', '[t](\n/u\n)')
c('link-bad-title', '[t](/u "unterminated)')
c('link-javascript', '[x](javascript:alert(1)) [x](JAVASCRIPT:alert(1)) [x](vbscript:x) [x](file:///etc) [x](data:text/html,x)')
c('link-data-image', '[x](data:image/png;base64,AAAA) [x](data:image/svg+xml,x)')
c('link-unicode-path', '[t](/경로/파일 "제목")')
c('link-percent', '[t](/a%20b%zz%2) [t](/a b)')
c('link-query-hash', '[t](https://example.com/p?q=1&r=<2>#frag)')
c('link-auth-port', '[t](http://user:pass@example.com:8080/x)')
c('link-ipv6', '[t](http://[::1]:3000/x)')
c('link-uppercase-host', '[t](HTTP://EXAMPLE.COM/Path)')
c('link-xn-ascii', '[t](http://xn--hxa.example/)')
c('link-mailto', '[mail](mailto:a@example.com)')
c('link-protocol-relative', '[t](//example.com/a)')
c('link-backslash-host', '[t](http://exa\\mple.com)')
c('link-max-parens', `[t](${'('.repeat(33)}x${')'.repeat(33)})`)
c('link-deep-brackets', '['.repeat(120) + 'a' + ']'.repeat(120) + '(/u)')
c('link-label-bracket-escape', '[a\\[b](/u)')
c('link-surrogate-path', '[t](/😀)')
c('image-basic', '![alt text](/i.png)')
c('image-title', '![a](/i.png "Title")')
c('image-alt-markup', '![*em* **st** `code` [l](u) <b>x</b>](/i.png)')
c('image-alt-entity', '![a &amp; b \\* c](/i.png)')
c('image-nested', '![outer ![inner](/in.png)](/out.png)')
c('image-deep', '!['.repeat(130) + 'x' + '](/u)'.repeat(130))
// Closed nestings past maxNesting (100): markdown-it truncates the label scans, so recursion stays near 100 levels.
c('image-deep-98', '!['.repeat(98) + 'x' + '](/u)'.repeat(98))
c('image-deep-250', '!['.repeat(250) + 'x' + '](/u)'.repeat(250))
c('image-deep-257', '!['.repeat(257) + 'x' + '](/u)'.repeat(257))
c('link-image-deep-257', '[!['.repeat(257) + 'x' + '](/i)](/l)'.repeat(257))
c('image-empty', '![](/i.png) ![a]()')
c('image-no-paren', '![just text]')
c('image-bad-close', '![a](/u "t" x)')
c('image-alt-newline', '![a\nb](/i.png)')
c('image-alt-quotes', '![say "hi"](/i.png)')
c('autolink-url', '<https://example.com/a?b=c>')
c('autolink-email', '<user.name+tag@example.co.kr>')
c('autolink-scheme', '<irc://x> <a+b.c-d:path> <x:>')
c('autolink-invalid', '<not a link> <http://a b> <@x.com> <a@b_c.com>')
c('autolink-javascript', '<javascript:alert(1)>')
c('autolink-percent', '<https://example.com/%7e%zz>')
c('autolink-uppercase', '<HTTPS://EXAMPLE.COM>')
c('autolink-long-scheme', `<${'a'.repeat(33)}:x>`)
c('autolink-typographer', '<https://x.com/(c)--a...> (c)')
c('autolink-mailto-normalize', '<a@b.c>')

// --- inline HTML ---
c('inline-html-span', 'a <span style="color: red">red</span> b')
c('inline-html-attrs', '<a href="x" title=\'y\' data-z=w checked>')
c('inline-html-bad-attr', '<a href="x>no')
c('inline-html-comment', 'a <!-- c --> b <!--> <!---> <!-- a -- b -->')
c('inline-html-processing', 'a <?x y?> b')
c('inline-html-declaration', 'a <!X y> b')
c('inline-html-cdata', 'a <![CDATA[ <b> ]]> b')
c('inline-html-close', 'a </b > </b x>')
c('inline-html-in-emphasis', '*<b>x</b>*')
c('inline-html-img', 'see <img src="a.png" alt="x"> here')
c('inline-html-br', 'line<br>next<br/>end')
c('inline-html-lt', 'a < b and a<b and <3')
c('inline-html-multiline', 'a <span\nclass="x">b</span>')
c('inline-html-unterminated', 'a <b')
c('inline-html-cbs', '<div class="status">{{char}}: HP {{getvar::hp}}</div> {{#if}}x{{/if}}')

// --- entities and escapes ---
c('entity-named', '&copy; &amp; &lt; &gt; &quot; &nbsp; &hellip;')
c('entity-numeric', '&#35; &#1234; &#992; &#0; &#x22; &#XD06; &#xcab;')
c('entity-invalid', '&#87654321; &#abcdef0; &#x110000; &#xD800; &MadeUpEntity; &x;')
c('entity-no-semicolon', '&copy &amp')
c('entity-in-code', '`&amp;`')
c('entity-in-link-title', '[t](/u "&quot;q&quot;")')
c('entity-in-attr', '[t](/u&ouml;)')
c('entity-long-name', '&' + 'a'.repeat(33) + ';')
c('entity-legacy-dest', '[t](/a&copyb) [t](/a&notit;)')
c('entity-legacy-numeric-dest', '[t](/&#128x;) [t](/&#12a;) [t](/&#0a;) [t](/&#x80g;)')
c('entity-strict-prefix', '&notit; &copycat; &ampx; &amp;x')
c('escape-all', '\\!\\"\\#\\$\\%\\&\\\'\\(\\)\\*\\+\\,\\-\\.\\/\\:\\;\\<\\=\\>\\?\\@\\[\\\\\\]\\^\\_\\`\\{\\|\\}\\~')
c('escape-non-punct', '\\a \\é \\😀 \\한')
c('escape-in-link', '[\\*t\\*](/u\\_x)')

// --- typographer replacements ---
c('typo-abbr', '(c) (C) (r) (R) (tm) (TM) (Tm) (x)')
c('typo-abbr-nested', '((c)) (c)(tm)')
c('typo-plusminus', '+- 5 +-+-')
c('typo-ellipsis', 'wait... really.. ok.... done')
c('typo-question-ellipsis', 'what?... no!... yes?..')
c('typo-many-marks', 'what???? no!!!! mix?!?! three??? ?!?!?!?!')
c('typo-commas', 'a,, b,,, c')
c('typo-em-dash', 'a---b and --- alone and ----four')
c('typo-en-dash', 'a -- b and 1--2 and --x and x--')
c('typo-dash-line-start', '-- start\n--- start')
c('typo-dash-line-separator', 'a --b ---c')
c('typo-dash-mixed', 'a----b a-----b --- -- -')
c('typo-in-code', '`(c) -- ...` (c)')
c('typo-in-link', '[(c)...](/u) [a](/x--y)')
c('typo-in-html', '<span title="(c)">(c)</span>')
c('typo-escaped', '\\(c) (c\\) \\-- \\.\\.\\.')
c('typo-across-emphasis', '*a*-- b ..*.*')

// --- smart quotes (to the app's private-use marks) ---
c('quote-double', '"Hello," she said. "Bye."')
c('quote-single', "'single' and it's John's")
c('quote-nested-pairs', '"outer \'inner\' outer"')
c('quote-apostrophes', "don't won't rock'n'roll '90s")
c('quote-digits', 'a 5" screen and 6\'2" tall "1"')
c('quote-digit-double', 'size 5"" and 5"x" and a"" and "open 5"" end')
c('quote-unmatched', '"open only and \'single open')
c('quote-unmatched-close', 'close only" and close\'')
c('quote-emphasis', '*"quoted"* "*em*" **"bold"**')
c('quote-across-tokens', '"a *b* c" and "x `y` z"')
c('quote-across-softbreak', '"line one\nline two"')
c('quote-punct', '("paren") ["bracket"] "end."')
c('quote-hangul', '"안녕," 그녀가 말했다. \'작은\' 따옴표')
c('quote-cjk-punct', '「"日本"」')
c('quote-emoji', '"😀" \'🎉\'')
c('quote-html-adjacent', '<b>"x"</b> "<i>y</i>"')
c('quote-escaped', '\\"not\\" "yes"')
c('quote-curly-input', '“curly” ‘single’')
c('quote-link', '"[link](/u)" [t](/u "title")')
c('quote-image-content', '"![a](/i.png)" after')
c('quote-many-openers', '"a '.repeat(1001) + 'b"')
c('quote-under-limit', '"a '.repeat(999) + 'end"')
c('quote-levels', '"a [b "c" d](/u) e"')

// --- realistic chat text ---
c('chat-dialogue', '"Where are you going?" she asked, *glancing at the door.*\n\n"Out," he said. *He didn\'t look back.*')
c('chat-action-nested', '*She smiles, **really** smiles, for the first time.*')
c('chat-thoughts', '*(I shouldn\'t have said that...)* "Sorry--I mean it."')
c('chat-mixed-asterisks', '**"Stop!"** *he shouted* — *and then* **silence**...')
c('chat-hangul-dialogue', '"오늘 날씨 좋네요." *그녀가 창밖을 바라본다.*\n\n**"정말요?"** 그가 웃었다.')
c('chat-hangul-list', '## 상태\n- 체력: 100\n- 기분: *좋음*\n- 위치: "도서관"')
c('chat-japanese', '「こんにちは」と*彼女*は言った。**驚いた**。')
c('chat-chinese', '他说："你好！" *微笑*')
c('chat-emoji', 'Good morning! ☀️🌸 *waves* 👋🏽 "Ready?" 👩‍👩‍👧')
c('chat-status-block', '```\n[Status]\nHP: 80/100\nMood: "calm"\n```\n\n*She checks the panel.*')
c('chat-status-html', '<div style="border:1px solid #ccc; padding:4px">\n<b>HP</b>: 80\n</div>\n\n*After the fight...*')
c('chat-code-answer', 'Here is the fix:\n\n```ts\nconst x: number = a ?? 0 // "default"\n```\n\nIt\'s *that* simple.')
c('chat-bullets-dialogue', '- "First," he said.\n- *Second*, she thought.\n- **Third** -- nobody spoke.')
c('chat-numbered-steps', '1. Open the door\n2. Say "hi"\n3. ...wait')
c('chat-long-line', ('*He walked* "and talked" -- on and on... ').repeat(60))
c('chat-ooc', '(OOC: let\'s skip ahead -- maybe 2 days?)\n\n---\n\n*Two days later.*')
c('chat-quote-reply', '> "You said this yesterday."\n\nI did. *sigh*')
c('chat-cbs-leftovers', '{{user}} looks at {{char}}. *{{char}} nods.* {{#if {{getvar::met}}}}"Again?"{{/if}}')
c('chat-cbs-html', '<span class="speech">"Hi {{user}}!"</span> *{{char}} waves*')
c('chat-inlay-like', '{{inlay::abc123}}\n\n*She shows you a photo.*')
c('chat-placeholder-chars', 'already "and new"')
c('chat-table-stats', '| Stat | Value |\n|---|---:|\n| STR | 12 |\n| "Luck" | *high* |')
c('chat-tilde-speech', 'Hello~ ~~nope~~ ok~~')
c('chat-dashes-hangul', '그는--잠시 멈췄다--그리고 말했다---"안녕."')
c('chat-ellipsis-hangul', '그래서... 어떻게 됐어?...')
c('chat-asterisk-math', '2*3*4 = 24 and 5 * 6 = 30')
c('chat-underscore-names', 'user_name and __init__ and _private_')
c('chat-url-text', 'Visit https://example.com or <https://example.com/(c)> now')
c('chat-script-like', 'Use <script>alert(1)</script> carefully')
c('chat-math-dollars', 'Price $5 and $$x^2$$ inline')
c('chat-paragraphs', 'One.\n\n\n\nTwo after many blanks.\n \nThree.')
c('chat-hard-breaks', 'Line one  \nLine two  \nLine three')
c('chat-mixed-everything', '# Scene 3\n\n> *Rain.*\n\n"Why?" -- she asked... **(c) 2026**\n\n- [ ] todo\n- [x] done\n\n```\nraw *text*\n```')

// --- long chat messages (2 to 10 KB of UTF-8 each, synthetic) ---
// `long` repeats the message's paragraph builders, numbering each round, until the message reaches `bytes`.
const utf8 = (text) => Buffer.byteLength(text, 'utf8')
function long(id, bytes, builders, separator = '\n\n') {
    const parts = []
    for (let round = 0; utf8(parts.join(separator)) < bytes; round++) {
        for (const build of builders) parts.push(build(round, round + 1))
    }
    const text = parts.join(separator)
    if (utf8(text) < 2048 || utf8(text) > 10240) throw new Error(`${id} is ${utf8(text)} bytes`)
    c(`chat-${id}`, text)
}
const hours = ['아침', '점심', '오후', '저녁', '밤', '새벽']
const at = (list, index) => list[index % list.length]

long('long-korean-rain', 3000, [
    (r) => `“오늘은 비가 올 것 같아.” 그녀가 창밖을 바라보며 중얼거렸다. ${at(hours, r)} 하늘은 잿빛이었고, 골목 끝의 가로등이 일찍 켜져 있었다.`,
    () => `*그는 우산 두 개를 챙기며 고개를 끄덕였다.* “그럼 같이 나가자. 혼자 가면 또 길을 잃을 거잖아.”`,
    () => `‘정말 괜찮을까?’ 속으로 생각했지만, 그녀는 말하지 않았다. 대신 *작게 웃으며* 신발 끈을 다시 묶었다.`,
    (r, n) => `빗방울이 ${n}번째로 창문을 두드렸을 때, 두 사람은 이미 계단을 내려가고 있었다. “천천히 가.” *그가 손을 내밀었다.*`,
])
long('long-korean-cafe', 4000, [
    (r, n) => `**미나:** "여기 ${n}번 테이블 맞죠?" *메뉴판을 펼치며 주위를 둘러본다.* 😊`,
    () => `**준호:** "응, 창가 자리로 예약했어." *의자를 빼 주며* **좋아!**🎉 "오늘은 내가 살게."`,
    () => `*점원이 다가와 주문을 받는다.* "아메리카노 두 잔이랑... 음, 치즈케이크 하나요." **"아, 그리고 시럽은 빼 주세요!"**`,
    () => `미나는 *컵을 두 손으로 감싸 쥐고* 김이 오르는 걸 한참 바라봤다.☕ "따뜻하다... 정말 오랜만이야."`,
    (r) => `"그때 그 일은--" *준호가 말을 꺼내다 멈춘다.* "아니, 나중에 얘기하자." (${r + 1}/3)`,
])
long('long-status-lists', 3500, [
    (r, n) => `## 상태 ${n}\n- 체력: ${80 - r}/100\n- 기분: *${at(['좋음', '보통', '피곤함'], r)}*\n- 소지품\n  - 우산\n  - **지갑** (현금 ${n * 1000}원)\n    - 카드 2장\n    - 영수증\n  - 휴대폰 🔋 ${90 - r * 7}%`,
    (r) => `1. 도서관에 간다\n2. "${at(['책', '노트', '사전'], r)}"을 반납한다\n   1. 연체료 확인\n   2. *사서에게 인사*\n3. 집으로 돌아온다\n\n> 메모: **오늘 안에** 끝낼 것.`,
])
long('long-mixed', 8000, [
    (r, n) => `# 장면 ${n}\n\n> *비가 그친 뒤의 거리.* 젖은 아스팔트 위로 네온이 번졌다.\n\n“여기서 기다려.” 그가 말했다. "금방 올게."`,
    () => `| 이름 | 역할 | 상태 |\n|:---|:---:|---:|\n| 미나 | 탐정 | *긴장* |\n| 준호 | 조수 | **침착** |\n| 고양이 | ??? | 😺 |`,
    (r) => '```js\n// 단서 정리\nconst clues = ["우산", "젖은 발자국", "' + at(hours, r) + '"]\nconsole.log(clues.length > 2 && "충분해")\n```',
    () => `- [ ] 증거 수집\n- [x] 목격자 인터뷰\n  - "검은 코트를 입은 사람이었어요."\n  - *목격자는 손을 떨고 있었다.*\n- [ ] 보고서 작성 -- 내일까지`,
    () => `<span style="color:#888">(OOC: 다음 장면으로 넘어가도 될까요?)</span> 좋아요... 그럼 **(c) 2026** 사건 파일을 열게요!!!!`,
    (r) => `He said "it's fine" -- but it wasn't. *She glanced at the clock: ${r + 9}:30.* ‘Again?’ 그녀는 생각했다.`,
])
long('long-english-adventure', 5000, [
    (r, n) => `*The torch flickers as you step into chamber ${n}.* "Careful," Elara whispers, "the floor here is older than the kingdom."`,
    () => `You notice **three** levers -- red, blue and one covered in moss... *Something skitters in the dark.* "Did you hear that?"`,
    () => `"Don't touch the moss one!" *She grabs your wrist.* 'Too late,' you think, as the wall begins to grind open.`,
    (r) => `> Inventory: rope, ${r + 2} torches, a map marked "(c) Guild of Cartographers", and half a sandwich.`,
])
long('long-japanese', 3000, [
    (r, n) => `「おはよう」と*彼女*は言った。**${n}日目**の朝だった。窓の外では桜が散っている🌸`,
    () => `「今日は図書館に行くの？」*彼は首をかしげる。*「うん、返す本があるから」`,
    () => `- 予定\n  - 図書館\n  - **買い物**（牛乳、パン）\n- メモ：*傘を忘れないこと*`,
])
long('long-diary', 4500, [
    (r, n) => `### 10월 ${n}일 (${at(['월', '화', '수', '목', '금', '토', '일'], r)})\n\n오늘은 *조금* 일찍 일어났다. “일찍 일어나는 새가...” 엄마가 늘 하던 말이 떠올랐다.`,
    () => `- 한 일\n  - 산책 🌳\n  - **보고서** 초안 작성\n- 못 한 일\n  - 답장 보내기 (내일은 꼭!)`,
    () => `---\n\n‘내일은 더 잘할 수 있을 거야.’ 그렇게 생각하며 불을 껐다.`,
])
long('long-thoughts-html', 4000, [
    (r, n) => `<div class="thought">\n*(${n}번째 생각)* 그는 정말 믿을 수 있는 사람일까?\n</div>\n\n"괜찮아요." *그녀는 애써 웃었다.*`,
    () => `<details>\n<summary>속마음</summary>\n\n**사실은 무서웠다.** 하지만 말할 수 없었다.\n\n</details>`,
    () => `<span class="speech">"같이 가요!"</span> <span class="action">*손을 흔든다*</span> 👋`,
])
long('long-nested-quotes', 3000, [
    (r, n) => `"그가 '내일 보자'고 했어." *미나가 말했다.* “그런데 ‘내일’이 ${n}번째야.”`,
    () => `"It's John's \"lucky\" day," she said -- 'or so he claims.' *rolls eyes*`,
    () => `“‘정말?’이라고 물었더니, ‘응’이라고만 하더라.” 그녀는 한숨을 쉬었다... "그게 다야."`,
])
long('long-emoji-emphasis', 3000, [
    (r, n) => `**축하해!**🎉🎉 *박수를 친다*👏 오늘로 ${n}주년이야 💕 _정말_ 고마워😭`,
    () => `😀*웃음*😀 **😎멋져😎** ***✨반짝✨*** ~~😢슬픔~~ 👍🏽👍🏽`,
    () => `"사랑해" ❤️ *안아준다* 🤗 "나도!" **(눈물)** 🥲`,
])
long('long-deep-lists', 4000, [
    (r, n) => `${n}. 준비물\n   - 옷\n     - 겉옷 *두꺼운 것*\n       - 목도리\n     - **양말** ${n}켤레\n   - 세면도구\n\n   계속 이어지는 설명 문단.\n${n + 1}. 출발\n   * 버스 시간 확인\n   * "표는 미리 사 둘 것"`,
    () => `- 느슨한 목록\n\n- 두 번째 항목\n\n  > 인용 안의 *강조*\n\n- 세 번째`,
])
long('long-code-help', 5000, [
    (r, n) => `질문 ${n}: 이 코드에서 \`undefined\`가 나오는 이유가 뭔가요?\n\n\`\`\`ts\nfunction greet(name?: string) {\n  return "안녕, " + name\n}\n\`\`\``,
    () => `*답변:* \`name\`이 선택 인자라서 값을 안 넘기면 \`undefined\`가 됩니다. 기본값을 주세요:\n\n\`\`\`ts\nfunction greet(name = "친구") { return \`안녕, \${name}\` }\n\`\`\``,
    () => `참고로 \`a ?? b\`와 \`a || b\`는 *다르게* 동작합니다 -- \`0\`이나 \`""\`일 때요.`,
])
long('long-ooc-notes', 3000, [
    (r, n) => `(OOC: ${n}일 후로 시간을 넘길게요 -- 괜찮나요?)\n\n---\n\n*${n}일 후.*`,
    () => `> "기다렸어." *문 앞에 서 있던 그가 말했다.*\n>\n> 그녀는 대답하지 않았다.`,
    () => `(OOC: 좋아요! 다음엔 **전투 장면**으로 가요... 주사위는 \`1d20+5\`로!)`,
])
long('long-poem', 2500, [
    (r, n) => `바람이 불어  \n${n}번째 잎이 떨어지고  \n*나는* 그 자리에 서 있었다`,
    () => `<p align="center">✦ ✦ ✦</p>`,
    () => `“가지 마.”  \n“…가야 해.”  \n**침묵.**`,
])
long('long-rpg-log', 4000, [
    (r, n) => `**턴 ${n}** -- 주사위: \`1d20\` = ${(r * 7) % 20 + 1}, 보정 +5\n\n| 캐릭터 | HP | MP |\n|---|---:|---:|\n| 전사 | ${120 - r * 3} | 10 |\n| 마법사 | ${60 - r} | ${80 - r * 4} |`,
    () => `*마법사가 지팡이를 들어 올린다.* "**파이어볼!**" 🔥 데미지 2*3*4 = 24`,
    () => `- 전리품\n  - 금화 x 12\n  - "낡은 지도" (c)\n  - ???`,
])
long('long-letter', 3500, [
    (r, n) => `사랑하는 언니에게,\n\n${n}월이 되었어. “잘 지내?”라는 말로 시작하기엔 너무 늦어버린 것 같지만……`,
    () => `‘언제 돌아와?’ 매일 묻고 싶었어. 그런데 *참았어*. 언니가 그랬잖아 —— “기다리는 것도 용기야.”`,
    () => `추신: **고양이**는 잘 있어 🐈 요즘은 *창가에서만* 자.`,
])
long('long-multi-character', 4500, [
    (r, n) => `**미나:** "${n}번 문이야, 확실해?" *지도를 펼친다.*\n**준호:** "응. 아마도..." *목소리가 작아진다.*`,
    () => `**고양이:** *야옹.* 🐾\n**미나:** "봐, 얘도 맞다잖아!" ***웃음***`,
    () => `**나레이션:** _그 순간, 복도 끝에서 발소리가 들렸다._ "누구세요?" -- 대답은 없었다.`,
])
long('long-markdown-heavy', 7500, [
    (r, n) => `## 기록 ${n}\n\n1) 첫째\n2) 둘째 *강조*\n\n* 별표 목록\n+ 더하기 목록\n\n***\n\n[링크](https://example.com/${n} "제목") 와 ![그림](/img/${n}.png) 그리고 <https://example.com>`,
    () => `> 인용\n>> 중첩 인용 **굵게**\n> 다시 바깥\n\n~~취소선~~ 과 \`코드\` 와 &copy; &amp; &#44032;`,
    (r) => `~~~\n물결 펜스 ${r}\n~~~\n\n<div>\n*HTML 안은 그대로*\n</div>\n\n각주 같은 것[^${r}] 그리고 \\*이스케이프\\*`,
    () => `"큰따옴표" ‘작은따옴표’ “둥근 따옴표” it's -- --- ... (tm) +- ????`,
])

// --- invariants and pathological input ---
c('edge-lone-high-surrogate', 'a\ud800b *c\ud800* \ud800')
c('edge-lone-low-surrogate', '\udc00*a*\udc00 **\udc00**')
c('edge-surrogate-emphasis-flank', '😀*a*😀 *😀*')
c('edge-astral-underscore', '𠀀_a_𠀀 and 😀_b_😀 and 𠀀*c*𠀀 and 𠀀_d_ end')
c('edge-bom', '﻿# Title\n﻿ text')
c('edge-unicode-space-emphasis', 'a *b* c 　*d*　')
c('edge-zero-width', 'a​*b*​c')
c('edge-many-brackets', '['.repeat(200))
c('edge-many-bangs', '!['.repeat(200))
c('edge-many-lt', '<'.repeat(200) + 'a')
c('edge-many-backticks', Array.from({ length: 40 }, (_, i) => '`'.repeat(i + 1)).join(' '))
c('edge-many-ampersands', '&'.repeat(100) + 'amp;')
c('edge-emphasis-pathological', '**a '.repeat(150) + '**')
c('edge-link-pathological', '[a](b '.repeat(100))
c('edge-max-nesting-quote-list', Array.from({ length: 80 }, () => '> - ').join('') + 'x')
c('edge-long-word', 'x'.repeat(5000))
c('edge-many-lines', Array.from({ length: 500 }, (_, i) => `line ${i} *em*`).join('\n'))

// --- punycode: hostnames normalizeLink and normalizeLinkText recode ---
c('punycode-link', '[t](http://例え.jp/パス)')
c('punycode-autolink', '<https://한국.kr>')
c('punycode-xn-text', '<http://xn--hxa.example/> <http://xn--3e0b707e.kr/> [t](http://xn--3e0b707e.kr/)')
c('punycode-image', '![a](https://bücher.example/b.png)')
c('punycode-reference', '[r]: http://éa.example\n\n[r]')
c('punycode-separators', '<http://例え。jp/> [t](http://a．b｡c/)')
c('punycode-auth', '[t](https://user:pw@例え.jp/x) <http://a@한국.kr>')
c('punycode-email', '<a@例え.jp> <mailto:b@bücher.example>')
c('punycode-astral', '<http://😀.example/> [t](http://a😀b.example)')
c('punycode-lone-surrogate', '<http://a\uD800b.example/>')
c('punycode-mixed-labels', '<http://www.bücher.例え.xn--hxa.example/>')
c('punycode-xn-invalid', '<http://xn--abc-.example/> <http://xn--a-ecp.example/> <http://xn--zz9999999999.example/> <http://xn--é-ab.example/>')
c('punycode-xn-overflow', '<http://xn--99999999999999999999.example/> <http://xn--a-9999999.example/>')
c('punycode-xn-uppercase', '<http://XN--hxa.example/> <http://xn--HXA.example/>')
c('punycode-xn-kelvin', '<http://xn--\u212Aa.example/> <http://xn--hx\u0130a.example/>')
c('punycode-xn-empty', '<http://xn--.example/> <http://xn---.example/>')
c('punycode-not-recoded', '<ftp://例え.jp/> [t](//例え.jp/x) [u](例え.jp)')
c('punycode-long-label', '<http://' + 'ü'.repeat(80) + '.example/>')

// --- reference labels: normalizeReference's toLowerCase().toUpperCase() ---
c('ref-case-umlaut', '[Ärger]: /a\n\n[ärger]')
c('ref-case-greek-sigma', '[Σ]: /s\n\n[σ] [ς]')
c('ref-case-final-sigma', '[ΟΔΟΣ]: /o\n\n[οδος] [οδοσ]')
c('ref-case-sharp-s', '[STRASSE]: /s\n\n[straße] [STRAẞE]')
c('ref-case-dotted-i', '[İ]: /i\n\n[i\u0307] [i]')
c('ref-case-kelvin', '[\u212A]: /k\n\n[k] [K]')
c('ref-case-cyrillic', '[Привет]: /p\n\n[ПРИВЕТ]')
c('ref-case-ligature', '[ﬀ]: /f\n\n[FF] [ff]')
c('ref-case-cherokee', '[ꭰ]: /c\n\n[Ꭰ]')
c('ref-case-astral', '[𐐨]: /d\n\n[𐐀]')
c('ref-case-lone-surrogate', '[a\uD800B]: /l\n\n[A\uD800b]')

// --- reference labels for engines with other case tables ---
// A label folds alike in every engine when it is ASCII apart from code points no table maps, so those compare with
// the recording. Older engines keep code points Rust's tables leave alone; newer or unprobed ones decline any other.
function e(id, caseUnicode, source, decline) {
    c(id, source, decline)
    if (caseUnicode !== undefined) cases[cases.length - 1].caseUnicode = caseUnicode
}
e('engine-unicode16-garay', undefined, '[\u{10D50}]: /g\n\n[\u{10D70}]')
e('engine-before16-ascii', 'before16', '[Foo  bar]: /f\n\n[FOO BAR] [foo bar][]')
e('engine-before16-uncased', 'before16', '[참고 漢字 ー]: /r\n\n[참고 漢字 ー]')
e('engine-before16-lone-surrogate', 'before16', '[x\uD800]: /s\n\n[X\uD800]')
e('engine-before16-cased', 'before16', '[Straße]: /a\n\n[STRASSE]', 'markdown-reference-case')
e('engine-before16-garay', 'before16', '[\u{10D50}]: /g\n\n[\u{10D70}]', 'markdown-reference-case')
// Unicode 17 folds as Rust's tables do; these labels fold alike in the recording's Unicode 16 engine.
e('engine-unicode17-uncased', 'unicode17', '[참고]: /r\n\n[참고]')
e('engine-unicode17-cased', 'unicode17', '[Straße]: /a\n\n[STRASSE]')
e('engine-unicode17-garay', 'unicode17', '[\u{10D50}]: /g\n\n[\u{10D70}]')
e('engine-from18-ascii', 'from18', '[Foo]: /f\n\n[fOO]')
e('engine-from18-uncased', 'from18', '[참고]: /r\n\n[참고]', 'markdown-reference-case')
// A definition and a reference label decline on their own, since either one's fold decides which links resolve.
e('engine-from18-definition', 'from18', '[참고]: /r\n\ntext', 'markdown-reference-case')
e('engine-from18-reference', 'from18', '[foo]: /f\n\n[참고] [foo]', 'markdown-reference-case')
e('engine-unknown-ascii', 'unknown', '[Foo]: /f\n\n[fOO]')
e('engine-unknown-lone-surrogate', 'unknown', '[x\uDC00]: /s\n\n[X\uDC00]')
e('engine-unknown-uncased', 'unknown', '[漢字]: /r\n\n[漢字]', 'markdown-reference-case')

// --- declines: behaviors the port does not reproduce ---
// U+A7CF and U+16EBB are lowercase letters in Unicode 17, which Rust's case tables follow and the oracle's Node does not.
c('decline-reference-case-new-letter', '[꟏]: /x\n\n[꟏] [\u{16EBB}]', 'markdown-reference-case')

writeFileSync(join(dir, 'markdown.cases.json'), `{"schema": 1, "cases": [\n${cases.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`)
console.log('markdown', cases.length)
// The case mapping of normalizeReference, recorded for every code point outside the surrogates by the oracle test.
writeFileSync(join(dir, 'reference-case.cases.json'), '{"schema": 1, "cases": [\n{"id": "all-code-points"}\n]}\n')
