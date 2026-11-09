// Writes the synthetic case file of the display render oracle (tests/fixtures/rust-core/render/display.cases.json):
// what `ParseMarkdown(source, null, 'notrim', -1, {}, context)` returns for a message with no character, which runs
// `parseInlayAssets`, `parseThoughtsAndTools`, `encodeStyle` and `renderHighlightableMarkdown`. Add cases here, run
// `node benchmarks/rust-core-oracles/render-display.cases.mjs`, then regenerate the expected file with
// RUST_CORE_ORACLE_WRITE=1.
//
// A case is { id, source, settings?, thoughtMode?, hideAllImages?, reservation?, inlays?, registry?, segmenter?,
// decline? }. `settings` is the `MarkdownRenderSettings` passed as `markdownSettings`; `inlays` are the stored inlays
// the metadata lookup finds; `reservation` is what `CSS.supports` answers for the image reservation properties;
// `registry: 'disposed'` clears the inlay registry first. `segmenter` (`none`, `before16`, `unicode17` or `from18`)
// renders for a JS engine with other `Intl.Segmenter` tables than the recording's Unicode 16 ones, and `none` records
// without `Intl.Segmenter`. `decline` names the `Unsupported` the Rust pipeline must answer instead of a result.
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/render/', import.meta.url))
mkdirSync(dir, { recursive: true })

const str = (value) => value.isWellFormed() ? value : { u: [...Array(value.length).keys()].map(i => value.charCodeAt(i)) }
const cases = []
const ids = new Set()
function c(id, source, options = {}) {
    if (ids.has(id)) throw new Error(`duplicate ${id}`)
    ids.add(id)
    cases.push({ id, source: str(source), ...options })
}

// --- quote placeholders under each settings combination ---
const dialogue = '"Hello \'world\'," she said.\n\n\'It\'s "fine",\' he said -- "really".\n\n> "Quoted \'inner\' line"\n\n- "item one"\n- \'item two\''
const custom = ['«', '»', '‹', '›']
const combos = {
    'mark': {},
    'unformat': { unformatQuotes: true },
    'blockquote': { blockquoteStyling: true },
    'unformat-over-blockquote': { unformatQuotes: true, blockquoteStyling: true },
    'custom-mark': { customQuotes: true, customQuotesData: custom },
    'custom-unformat': { customQuotes: true, customQuotesData: custom, unformatQuotes: true },
    'custom-blockquote': { customQuotes: true, customQuotesData: custom, blockquoteStyling: true },
    'custom-without-data': { customQuotes: true, blockquoteStyling: true },
    'data-without-custom': { customQuotesData: custom },
}
for (const [name, settings] of Object.entries(combos)) c(`quote-${name}`, dialogue, { settings })
const patterns = ['[$&]', '$$', '<$`>', "<$'>", '$1$0$<', '']
for (const mode of ['mark', 'unformat', 'blockquote']) {
    c(`quote-patterns-${mode}`, 'a "b \'c\' d" e', { settings: { ...combos[mode], customQuotes: true, customQuotesData: patterns.slice(0, 4) } })
}
c('quote-patterns-tail', 'x "y" \'z\'', { settings: { customQuotes: true, customQuotesData: ['$1', '$0', '$<n>', ''] } })
c('quote-placeholders-in-quotes', 'a "b" \'c\'', { settings: { customQuotes: true, customQuotesData: ['', '', '', ''] } })
c('quote-placeholders-in-quotes-blockquote', 'a "b" \'c\'', { settings: { customQuotes: true, customQuotesData: ['', '', '', ''], blockquoteStyling: true } })
c('quote-blockquote-lines', '"open\nclose" and \'one\ntwo\'\n\n"" \'\' "a" "\'b\'" \'"c"\'', { settings: { blockquoteStyling: true } })
c('quote-blockquote-unmatched', '"a \'b "c" d\' e\n\n\'x "y\' z"', { settings: { blockquoteStyling: true } })
c('quote-blockquote-astral', '"😀" \'𠀀\' "a😀b" "\uD83D"', { settings: { blockquoteStyling: true } })
c('quote-curly-source', '“curly” and ‘single’ and “mixed\' quotes’', { settings: {} })
c('quote-curly-unformat', '“curly” and ‘single’', { settings: { unformatQuotes: true } })
c('unescape-private-use', 'a b     ""')
c('unescape-in-code', '`code`\n\n```\nblock\n```')

// --- KaTeX spans ---
c('math-inline', 'Before $$x^2$$ after')
c('math-fraction', '$$\\frac{1}{2}$$ and $$\\sqrt{a}$$')
c('math-error', '**b** $$\\notARealCommand{x}$$ after')
c('math-empty', 'a $$$$ b $$$ c')
c('math-multiline', 'first $$a\n\nb$$ second')
c('math-private-use', '$$x + y$$')
c('math-typographer', '$$x -- y (c)$$ and "quoted"')
c('math-unclosed', '$$ alone and $ one')
c('math-in-code', '`$$x$$` and\n\n```\n$$y$$\n```')

// --- thoughts and tool calls ---
c('thought-closed', '<Thoughts>plan *this*</Thoughts>Answer')
c('thought-nested', '<Thoughts>a<Thoughts>b</Thoughts>c</Thoughts>d')
c('thought-unclosed-off', 'x <Thoughts>still thinking <b>bold</b>')
c('thought-unclosed-collapsed', 'x <Thoughts>still thinking', { thoughtMode: 'collapsed' })
c('thought-closed-collapsed', '<Thoughts>done</Thoughts>after', { thoughtMode: 'collapsed' })
c('thought-two', '<Thoughts>one</Thoughts> mid <Thoughts>two</Thoughts> end')
c('thought-close-without-open', 'a </Thoughts> b <Thoughts>c')
c('thought-recent-short', 'before <Thoughts>line one\nline two</Thoughts> after', { thoughtMode: 'recent' })
c('thought-recent-streaming', 'before <Thoughts>' + Array.from({ length: 12 }, (_, i) => `step ${i}`).join('\n'), { thoughtMode: 'recent' })
c('thought-recent-long', '<Thoughts>' + 'abc def '.repeat(150) + '\n\n' + 'x'.repeat(30), { thoughtMode: 'recent' })
c('thought-recent-korean', '<Thoughts>' + '생각을 정리하는 중입니다. '.repeat(70), { thoughtMode: 'recent' })
c('thought-recent-orphan-mark', '<Thoughts>' + 'x'.repeat(1000) + '́' + 'y'.repeat(799), { thoughtMode: 'recent' })
c('thought-recent-combining', '<Thoughts>' + 'x'.repeat(1000) + 'é' + 'y'.repeat(798), { thoughtMode: 'recent', decline: 'thought-preview-grapheme' })
c('thought-recent-emoji', '<Thoughts>' + 'x'.repeat(1000) + '👍🏽' + 'y'.repeat(796), { thoughtMode: 'recent', decline: 'thought-preview-grapheme' })
c('thought-recent-crlf', '<Thoughts>' + 'x'.repeat(1000) + '\r\n' + 'y'.repeat(798), { thoughtMode: 'recent' })
c('thought-recent-cr', '<Thoughts>' + 'x'.repeat(1000) + '\r' + 'y'.repeat(799), { thoughtMode: 'recent' })
c('thought-recent-split-tag', 'a <Thoughts>body</Thou', { thoughtMode: 'recent' })
c('thought-recent-escapes', '<Thoughts>a & b < c > "d"\r\ne</Thoughts>', { thoughtMode: 'recent' })
c('thought-recent-empty', '<Thoughts></Thoughts>tail', { thoughtMode: 'recent' })
c('thought-recent-none', 'no thoughts here', { thoughtMode: 'recent' })
c('thought-recent-prepend-astral', '<Thoughts>' + 'x'.repeat(1000) + '\u{113D1}' + 'y'.repeat(798), { thoughtMode: 'recent', decline: 'thought-preview-grapheme' })
// A truncated window that starts with `start`, for the engines with other `Intl.Segmenter` tables.
const windowFrom = (start) => '<Thoughts>' + 'x'.repeat(1000) + start + 'y'.repeat(800 - start.length)
const short = 'before <Thoughts>line one\nline two</Thoughts> after'
c('thought-nosegmenter-combining', windowFrom('e\u0301'), { thoughtMode: 'recent', segmenter: 'none' })
c('thought-nosegmenter-hangul', windowFrom('가나'), { thoughtMode: 'recent', segmenter: 'none' })
c('thought-nosegmenter-low-surrogate', '<Thoughts>' + 'x'.repeat(1000) + '😀' + 'y'.repeat(799), { thoughtMode: 'recent', segmenter: 'none' })
c('thought-nosegmenter-high-surrogate', windowFrom('😀'), { thoughtMode: 'recent', segmenter: 'none' })
c('thought-nosegmenter-emoji-modifier', windowFrom('👍🏽'), { thoughtMode: 'recent', segmenter: 'none' })
c('thought-nosegmenter-prepend-astral', windowFrom('\u{113D1}'), { thoughtMode: 'recent', segmenter: 'none' })
c('thought-nosegmenter-short', short, { thoughtMode: 'recent', segmenter: 'none' })
c('thought-before16-truncated', windowFrom('가나'), { thoughtMode: 'recent', segmenter: 'before16', decline: 'thought-preview-grapheme' })
c('thought-before16-short', short, { thoughtMode: 'recent', segmenter: 'before16' })
c('thought-unicode17-truncated', windowFrom('ab'), { thoughtMode: 'recent', segmenter: 'unicode17' })
c('thought-unicode17-prepend-astral', windowFrom('\u{113D1}'), { thoughtMode: 'recent', segmenter: 'unicode17', decline: 'thought-preview-grapheme' })
c('thought-from18-truncated', windowFrom('ab'), { thoughtMode: 'recent', segmenter: 'from18', decline: 'thought-preview-grapheme' })
c('thought-from18-short', short, { thoughtMode: 'recent', segmenter: 'from18' })
c('tool-call-named', '<tool_call>idsearch{"q":"x"}</tool_call>after')
c('tool-call-unnamed', '<tool_call>plain</tool_call>')
c('tool-call-patterns', '<tool_call>a$& $` $\' $$b</tool_call>')
c('tool-call-multiline', 'x<tool_call>a\nbmulti\nline</tool_call><tool_call></tool_call>y')
c('tool-call-empty', '<tool_call></tool_call> and <tool_call>')

// --- style blocks ---
c('style-basic', '<style>.a { color: red; }</style>\n\ntext')
c('style-unicode', '<style>.b::after { content: "한글 😀"; }</style>')
c('style-lone-surrogate', '<style>x\uD800y</style>')
c('style-empty-and-unclosed', '<style></style> mid <style>open')
c('style-two', '<style>a</style><style>b</style>')
c('style-case', '<STYLE>c</STYLE> <style >d</style>')
c('style-multiline', 'pre\n<style>\n.c {\n  margin: 0;\n}\n</style>\npost')

// --- inlays ---
const image = (id, extra = {}) => ({ id, type: 'image', mime: 'image/png', width: 640, height: 480, ...extra })
c('inlay-image', 'see {{inlay::img1}} here', { inlays: [image('img1')] })
c('inlay-image-reservation', '{{inlay::img1}}', { inlays: [image('img1')], reservation: true })
c('inlay-image-no-size', '{{inlay::img2}}', { inlays: [image('img2', { width: undefined, height: undefined })], reservation: true })
c('inlay-image-bad-size', '{{inlay::a}} {{inlay::b}} {{inlay::c}}', {
    inlays: [image('a', { width: 1.5 }), image('b', { width: 0 }), image('c', { height: 4294967296 })], reservation: true,
})
c('inlay-inlayed', '{{inlayed::img1}}text {{inlayeddata::img1}}', { inlays: [image('img1')] })
c('inlay-video-audio', '{{inlay::v}} {{inlay::a}} {{inlay::m}}', {
    inlays: [{ id: 'v', type: 'video', mime: 'VIDEO/MP4' }, { id: 'a', type: 'audio', mime: 'audio/mp3' }, { id: 'm', type: 'audio', mime: 'audio/x-unknown' }],
})
c('inlay-signature-missing', '{{inlay::sig}} {{inlay::gone}}', { inlays: [{ id: 'sig', type: 'signature', mime: 'text/plain' }] })
c('inlay-hide-images', '{{inlay::img1}} {{inlayed::v}} {{inlay::img1}}', { inlays: [image('img1'), { id: 'v', type: 'video', mime: 'video/webm' }], hideAllImages: true })
c('inlay-duplicates', '{{inlay::img1}} {{inlay::img1}} {{inlayed::img1}}', { inlays: [image('img1')] })
c('inlay-escaped-id', '{{inlay::a"b\'<c>&$&d}}', { inlays: [image('a"b\'<c>&$&d')] })
c('inlay-patterns-id', 'x {{inlay::p$`q}} y {{inlay::r$\'s}} z', { inlays: [image('p$`q'), image('r$\'s')] })
c('inlay-braces', '{{inlay::}}}} {{inlay::{{inlay::x}} {{inlay::a\nb}}', { inlays: [image('}'), image('{{inlay::x'), image('a\nb')] })
c('inlay-join', '{{inl{{inlay::h}}ay::k}}X{{inlay::k}}', { inlays: [image('h'), image('k')], hideAllImages: true })
c('inlay-inside-skipped', '{{inlay::{{inlay::a}} x {{inlay::a}}', { inlays: [image('a')] })
c('inlay-inside-markup', '{{inlay::a$&}} {{inlay::a$&}}', { inlays: [image('a$&')] })
c('inlay-after-brace', '{{inlay::{{inlay::z}} {{{inlay::a}}', { inlays: [image('a')] })
c('inlay-disposed-registry', '{{inlay::img1}} {{inlayed::vid1}}', { inlays: [image('img1'), { id: 'vid1', type: 'video', mime: 'video/mp4' }], reservation: true, registry: 'disposed' })
c('inlay-many', Array.from({ length: 12 }, (_, i) => `{{inlay::n${i}}}`).join(' '), { inlays: Array.from({ length: 12 }, (_, i) => image(`n${i}`)) })
c('inlay-with-markdown', '*caption* {{inlayed::img1}}\n\n"quote"', { inlays: [image('img1')] })

// --- highlight placeholders ---
const fence = (lang, code) => '```' + lang + '\n' + code + '\n```'
c('highlight-js', fence('js', 'const a = 1 < 2 && "x"'))
c('highlight-languages', ['python', 'c', 'cpp', 'html', 'svg', 'json', 'lua', 'md', 'rust', 'shell', 'tsx', 'txt', 'vtt', 'bash', 'css', 'cs', 'dart', 'java', 'yaml', 'xml'].map(lang => fence(lang, `x = 1 // ${lang}`)).join('\n\n'))
c('highlight-plain-unknown', fence('brainfuck', '+[-->-[>>+>-----<<]<--<---]>-.'))
c('highlight-error', fence('risuerror', 'Something <failed> & stopped'))
c('highlight-identical', fence('js', 'same()') + '\n\n' + fence('js', 'same()') + '\n\n' + fence('zz', 'same()') + '\n\n' + fence('zz', 'same()'))
c('highlight-empty-code', '```js\n```\n\n' + fence('js', '') + '\n\n' + fence('py', 'second'))
c('highlight-lang-quote', fence('js"x', 'quoted()') + '\n\n' + fence('py">y', 'two()'))
c('highlight-code-close-tag', fence('js', 'a </pre-hljs-placeholder> b'))
c('highlight-code-patterns', fence('zz', 'echo $$ $& $\' $` done') + '\n\n' + fence('js', '"$&"'))
c('highlight-raw-placeholder', '<pre-hljs-placeholder lang="js">raw()</pre-hljs-placeholder>\n\ntext <pre-hljs-placeholder lang="a\nb">x</pre-hljs-placeholder>')
c('highlight-with-quotes', 'He said "run it":\n\n' + fence('py', 'print("hi")') + '\n\n\'done\'', { settings: { blockquoteStyling: true } })

// --- one long message through every stage ---
c('mixed-message', [
    '<Thoughts>The user wants a story.</Thoughts>',
    '# 장면 1', '', '"비가 오네," 그녀가 말했다. *창밖을 본다* 😊', '',
    '{{inlayed::img1}}', '<style>.x-risu-a { color: #888; }</style>', '',
    'Math: $$e^{i\\pi} + 1 = 0$$ -- done (c)', '',
    fence('js', 'console.log("hi")'), '',
    '<tool_call>tlookup{}</tool_call>',
    '- \'one\'', '  - "two"', '',
    'escaped',
].join('\n'), { inlays: [image('img1')], settings: { blockquoteStyling: true }, reservation: true })

writeFileSync(join(dir, 'display.cases.json'), `{"schema": 1, "labels": ${JSON.stringify({ cot: 'Chain of Thoughts', toolCalled: "Tool '{{tool}}' Called", error: 'Error' })}, "cases": [\n${cases.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`)
console.log('display', cases.length)

// The grapheme recording (tests/fixtures/rust-core/render/grapheme.cases.json): for every lead code point and each
// stand-in second code point, whether `Intl.Segmenter` keeps them in one cluster; whether each code point extends an
// 'a'; and the engine probes of `JsEngine`. `simple` holds the second code points the Rust rules accept, and every
// one of them must break as its stand-in after each of `classLeads`, one lead per property class UAX #29 reads.
const grapheme = [
    {
        id: 'first-cluster',
        seconds: [0x09, 0x0a, 0x0d, 0x61, 0x3042, 0x30a2, 0x4e00, 0xac00, 0xac01],
        simple: [[0x00, 0x7f], [0x3041, 0x3096], [0x30a1, 0x30fa], [0x3400, 0x4dbf], [0x4e00, 0x9fff], [0xac00, 0xd7a3]],
        classLeads: [0x01, 0x0a, 0x0d, 0x61, 0x0300, 0x0600, 0x0903, 0x0915, 0x094d, 0x1100, 0x1160, 0x11a8, 0x200c, 0x200d, 0xac00, 0xac01,
            0xd800, 0xdc00, 0x113d1, 0x1f1e6, 0x1f3fb, 0x1f600],
    },
    { id: 'extend-after-a' },
    { id: 'probes', case16Small: 0x10d70, case16Capital: 0x10d50, case17: 0xa7d3, segment16: 0x113d1, segment17: 0x1acf },
]
writeFileSync(join(dir, 'grapheme.cases.json'), `{"schema": 1, "cases": [\n${grapheme.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`)
// The same recording in a WebView's engine (render-grapheme-webview.capture.mjs), with the Unicode 18 case markers
// among the probes and the engine's identity.
const graphemeWebview = [grapheme[0], grapheme[1], { ...grapheme[2], case18Small: 0x277, case18Capital: 0xa7dd }, { id: 'engine' }]
writeFileSync(join(dir, 'grapheme-webview.cases.json'), `{"schema": 1, "cases": [\n${graphemeWebview.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`)
