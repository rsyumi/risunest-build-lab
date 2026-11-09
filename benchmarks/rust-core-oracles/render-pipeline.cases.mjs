// Writes the synthetic case file of the display pipeline oracle (tests/fixtures/rust-core/render/pipeline.cases.json).
// Add cases here, run `node benchmarks/rust-core-oracles/render-pipeline.cases.mjs`, then regenerate the expected file
// with RUST_CORE_ORACLE_WRITE=1.
//
// Every case has an `id` and a `kind`:
// - `display`: `ParseMarkdown(source, character, mode, chatID, {}, context)` with the real scripts, against a synthetic
//   database. `character` is { chaId, assets, emotions, scripts? } (absent: no character); `context` is the
//   `renderContext` subset { assetWidth, hideAllImages, characterImageSource, userImageSource, legacyMediaFindings,
//   assetMaxDifference }; `platform: 'tauri'` resolves the paths in `images` to image sources; `settings` is
//   `markdownSettings`; `inlays`, `reservation` and `thoughtMode` are as in the display oracle; `styleHistory` passes a
//   script context whose `prepareHistory` records the style history call (only without a character); `prior` is
//   { source, mode } of a render made first on the same inlay registry, whose host calls are not recorded.
//   `selected: false` renders with no character selected.
// - `post-translation`: `postTranslationParse(source, settings)`, `settings` absent for the database's.
// - `markdown-safe`: `parseMarkdownSafe(source)` with the database's quote settings.
// - `style`: `trimMarkdown(source, { returnCSSError: true })` on authored `<risu-style>` elements.
// - `hex`: what the app's `Buffer` polyfill decodes from `hex`.
// - `lowercase-ascii`: every code point whose lowercase in the root, Turkish, Azeri or Lithuanian locale is ASCII
//   while it is not, or differs from its ASCII lowercase while it is ASCII.
// - `cbs-syntax`: `hasCbsSyntax(text)` for each of `texts`.
// `database` sets database fields for the case. `decline` names the `Unsupported` the Rust side must answer.
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/render/', import.meta.url))
mkdirSync(dir, { recursive: true })

const str = (value) => value.isWellFormed() ? value : { u: [...Array(value.length).keys()].map(i => value.charCodeAt(i)) }
const cases = []
const ids = new Set()
function c(id, kind, fields = {}) {
    if (ids.has(id)) throw new Error(`duplicate ${id}`)
    ids.add(id)
    const entry = { id, kind, ...fields }
    if (typeof entry.source === 'string') entry.source = str(entry.source)
    cases.push(entry)
}

// --- asset tags ---
const assets = [
    ['Smile', 'assets/smile.png', 'png'], ['smile', 'assets/smile-2.png', 'png'], ['Clip', 'assets/clip.mp4', 'mp4'],
    ['music', 'assets/music.mp3', 'mp3'], ['wall', 'assets/wall.webp', 'webp'], ['dollar', 'assets/d$&.png', 'png'],
    ['행복', 'assets/happy-ko.png', 'png'], ['multi', 'assets/m1.png', 'png'], ['MULTI', 'assets/m2.png', 'png'],
    ['multi', 'assets/m3.png', 'png'], ['vid', 'assets/v.webm', 'webm'], ['upper', 'assets/u.png', 'MP4'], ['noext', 'assets/n', ''],
    ['gone', 'assets/missing-url.png', 'png'], ['blank', '', 'png'], ['curly}', 'assets/curly.png', 'png'], ['a\nb', 'assets/ab.png', 'png'],
]
const emotions = [['happy', 'assets/emo-happy.png'], ['empty', ''], ['Sad', 'assets/emo-sad.png'], ['nourl', 'assets/missing-url.png']]
const character = (extra = {}) => ({ chaId: 'oracle-simple', assets, emotions, ...extra })
const sources = { characterImageSource: 'assets/char.png', userImageSource: 'assets/user.png' }
const image = (id, extra = {}) => ({ id, type: 'image', mime: 'image/png', width: 640, height: 480, ...extra })
const display = (id, source, fields = {}) => c(id, 'display', { source, mode: 'notrim', character: character(), chatID: 0, context: { assetWidth: 2 }, ...fields })

display('assets-img-raw-path', '{{img::Smile}} {{raw::wall}} and {{path::wall}}')
display('assets-every-tag', [
    '{{image::wall}}', '{{video::Clip}}', '{{video-img::Clip}}', '{{audio::music}}', '{{bgm::music}}', '{{asset::Clip}}', '{{asset::wall}}',
    '{{asset::upper}}', '{{asset::vid}}', '{{asset::noext}}', '{{bg::wall}}',
].join('\n\n'))
display('assets-bg-back', 'x {{bg::wall}} y {{img::wall}}', { mode: 'back' })
display('assets-emotion', '{{emotion::happy}} {{emotion::SAD}} {{emotion::empty}} {{emotion::none}} {{emotion::nourl}}')
display('assets-missing-and-fuzzy', '{{img::nothing}} {{img::smile}} {{img::smil}} {{img::smile}} {{raw::wal}}')
display('assets-exact-only', '{{img::smil}} {{raw::wal}}', { context: { assetWidth: 2, legacyMediaFindings: true } })
display('assets-max-difference', '{{img::smi}} {{raw::wl}}', { context: { assetWidth: 2, assetMaxDifference: 2 } })
for (const chatID of [0, 7, 1234, -1, 999, 2.5, 0.5]) display(`assets-pick-chat-${String(chatID).replace('.', '-')}`, '{{img::multi}} {{raw::smile}}', { chatID })
display('assets-pick-global', '{{img::multi}}', { character: character({ chaId: '' }), chatID: 3 })
display('assets-hide-images', '{{img::Smile}} {{video::Clip}} {{emotion::happy}} {{bgm::music}} {{raw::wall}} {{asset::Clip}} {{image::wall}} {{source::char}} {{inlay::i1}}', {
    context: { assetWidth: 2, hideAllImages: true, ...sources }, inlays: [{ id: 'i1', type: 'image', mime: 'image/png', width: 640, height: 480 }],
})
display('assets-source', '{{source::char}} {{source::USER}} {{source::Char}} {{source::other}} {{source::smile}}', { context: { assetWidth: 2, ...sources } })
display('assets-source-patterns', 'a {{source::char}} b {{source::user}} c', {
    context: { characterImageSource: 'assets/d$&.png', userImageSource: 'assets/u$`x$\'y$$.png' },
})
display('assets-source-empty', '{{source::char}}|{{source::user}}', { context: { characterImageSource: '' } })
display('assets-source-database', '{{source::char}}|{{source::user}}')
display('assets-source-literal-placeholder', 'keep CHAR and USER {{source::nobody}}', { context: sources })
// With no character selected and no image source in the context, `getCurrentCharacter().image` throws once a pass
// holds a `source` tag, whatever its name; the Rust side declines there, so the app throws as before. (A render that
// gets to the scripts logs the display trigger's error, which the harness refuses, so no case does.)
display('assets-source-no-selection', 'a {{source::nobody}} {{img::Smile}} b', { selected: false, decline: 'asset-source' })
display('assets-width-zero', '{{img::wall}}', { context: { assetWidth: 0 } })
display('assets-width-minus-one', '{{img::wall}}', { context: { assetWidth: -1 } })
display('assets-width-fraction', '{{img::wall}} {{image::wall}}', { context: { assetWidth: 2.5 } })
display('assets-width-large', '{{asset::wall}}', { context: { assetWidth: 1e21 } })
display('assets-width-database', '{{img::wall}}', { context: {}, database: { assetWidth: 3 } })
display('assets-scan-edges', [
    '{{img::Smile', '{{img::}}}}', '{{img::a\nb}}', '{{IMG::Smile}}', '{{{img::Smile}}', '{{img::Smile}}}', '{{img::curly}}}', '{{raw::}}',
    '{{img::行복}} {{img::행복}}', '{{video-img::Clip}}{{video::Clip}}', '{{bgm::music}}{{bg::wall}}',
].join(' / '), { mode: 'back' })
display('assets-unclosed-tail', 'a {{img::wall}} b {{raw::wall c')
display('assets-url-patterns', '{{img::dollar}} {{raw::dollar}} {{img::gone}} {{emotion::nourl}}')
// On Tauri only `img`, `image`, `raw`, `path`, `asset`, `emotion` and the source paths ask for an image source first; the
// media tags and `bg` take `getFileSrc` even for a path that has one.
const tauriImages = ['assets/smile.png', 'assets/smile-2.png', 'assets/wall.webp', 'assets/emo-happy.png', 'assets/char.png', 'assets/clip.mp4']
display('assets-tauri-images', [
    '{{img::Smile}} {{raw::wall}} {{video::Clip}} {{emotion::happy}} {{asset::wall}} {{image::music}} {{source::char}} {{source::user}}',
    '{{path::wall}} {{path::music}} {{audio::music}} {{bgm::music}} {{bg::wall}} {{video-img::Clip}}',
].join('\n'), { platform: 'tauri', images: tauriImages, context: { assetWidth: 2, ...sources } })
display('assets-tauri-back', '{{bg::wall}} {{img::wall}}', { mode: 'back', platform: 'tauri', images: tauriImages })
// A job's registry serves its earlier renders too (ChatBody's pretranslate parse before the shown one): the `asset:`
// slots that render's `trimMarkdown` registered for the image sources it remembered come before this render's inlays.
display('assets-tauri-inlays-after-prior', '{{img::Smile}} {{inlay::i1}} {{raw::wall}} {{inlayed::i2}}', {
    mode: 'normal', platform: 'tauri', images: tauriImages, inlays: [image('i0'), image('i1'), image('i2', { type: 'video', mime: 'video/mp4' })],
    prior: { source: '{{inlay::i0}} {{img::wall}}', mode: 'pretranslate' },
})
display('assets-script-second-pass', 'SWAP {{img::Smile}}', { character: character({ scripts: [{ in: 'SWAP', out: '{{img::wall}} {{source::char}}' }] }), context: sources })
display('assets-script-identity', '{{img::Smile}} stays', { character: character({ scripts: [{ in: 'NOTHERE', out: '{{img::wall}}' }] }) })
// The first pass writes a tag through a path; scripts that keep the text leave it unread.
display('assets-script-identity-tag-in-path', '{{raw::nested}}', { character: character({ assets: [...assets, ['nested', 'assets/n{{raw::wall}}.png', 'png']] }) })
display('assets-script-rewrites', '{{raw::wall}}', { character: character({ scripts: [{ in: 'assets/wall\\.webp', out: '{{raw::music}}' }] }) })
display('assets-mode-normal', '# Title\n\n{{img::Smile}} "quoted" $$x^2$$\n\n```js\nlet a = 1\n```', { mode: 'normal' })
display('assets-mode-pretranslate', '# Title {{img::Smile}} $$x$$\n\n```js\nb()\n```\n\n<style>.p { color: red; }</style>', { mode: 'pretranslate' })
display('assets-mode-back-markdown', '*x* {{bg::wall}} <style>.q { color: blue; }</style>', { mode: 'back' })

// --- the whole display render ---
c('display-no-character', 'display', { source: '**bold** {{img::Smile}} {{inlay::i1}}', mode: 'normal', inlays: [image('i1')] })
c('display-no-character-notrim', 'display', { source: '"q" $$a$$ `c`', mode: 'notrim' })
c('display-empty', 'display', { source: '', mode: 'normal' })
c('display-everything', 'display', {
    source: [
        '<Thoughts>plan</Thoughts>', '# 장면', '', '"Hi," {{img::Smile}} said. {{inlayed::i1}}', '<style>.x-risu-z { margin: 0; }</style>',
        'Math $$\\frac{1}{2}$$ -- ok', '', '```py\nprint("x")\n```', '', '<tool_call>tsearch{}</tool_call>', '{{source::char}}',
    ].join('\n'),
    mode: 'notrim', inlays: [image('i1')], reservation: true, settings: { blockquoteStyling: true }, thoughtMode: 'collapsed',
    context: { assetWidth: 2, ...sources },
})
c('display-everything-normal', 'display', {
    source: '<Thoughts>p</Thoughts>"Hi" {{img::wall}} {{inlay::i1}}\n\n<style>.x-risu-y { padding: 0; }</style>\n\n```js\nq()\n```',
    mode: 'normal', inlays: [image('i1')], character: character(), chatID: 4, context: { assetWidth: 2 },
})
c('display-style-history', 'display', { source: 'a <style>.h1 { color: red; }</style> b', mode: 'normal', styleHistory: true })
c('display-style-history-authored', 'display', { source: 'a <RISU-STYLE x>00</RISU-STYLE> b', mode: 'normal', styleHistory: true })
c('display-style-history-none', 'display', { source: 'a <risu-styles>00</risu-styles> <risu-style_x> b', mode: 'normal', styleHistory: true })
// Without the Markdown, which escapes a tag name with `_`.
c('display-style-history-underscore', 'display', { source: 'a <risu-style_x> b', mode: 'back', styleHistory: true })
c('display-style-history-back', 'display', { source: '<style>.h2 { color: red; }</style>', mode: 'back', styleHistory: true })

// --- postTranslationParse ---
c('post-translation-lines', 'post-translation', { source: '  <div>a</div>  \n\t<p>b\n  not a tag  \n<\n 　<span>c</span>　\r\n' })
c('post-translation-markdown', 'post-translation', { source: ' <b>x</b>\n"quote" and $$y$$\n\n```js\nz()\n```', settings: {} })
c('post-translation-database', 'post-translation', { source: '"a" \'b\'', database: { customQuotes: true, customQuotesData: ['«', '»', '‹', '›'] } })
c('post-translation-settings', 'post-translation', { source: '"a" \'b\'', settings: { unformatQuotes: true } })

// --- parseMarkdownSafe ---
c('markdown-safe-basic', 'markdown-safe', { source: '# T\n\n"q" <a href="x">l</a> $$z$$\n\n```js\nnot highlighted\n```' })
c('markdown-safe-database', 'markdown-safe', { source: '"a" and \'b\'', database: { blockquoteStyling: true } })

// --- <risu-style> decoding in trimMarkdown ---
const hex = (text) => Buffer.from(text).toString('hex')
// In the harness, a `<risu-style>` that opens the text comes out of the sanitizer as its bare hex, so each case starts
// with a paragraph.
const style = (id, source) => c(id, 'style', { source: `<p>x</p>${source}` })
style('style-decoded', `<risu-style>${hex('.a { color: red; }')}</risu-style>`)
style('style-close-tag', `<risu-style>${hex('.b::after { content: "</style><b>" }')}</risu-style>`)
style('style-close-tag-case', `<risu-style>${hex('.c::after { content: "</STYLE> </Style2 </ style" }')}</risu-style>`)
style('style-unicode', `<risu-style>${hex('.d::before { content: "한글 😀" }')}</risu-style><risu-style>${hex('.e { margin: 0 }')}</risu-style>`)
style('style-malformed-hex', '<risu-style>2e66207b20636f6c6f723a2072656420 7d</risu-style><risu-style>2e677b6d617267696e3a317d1g</risu-style>')
style('style-invalid-utf8', '<risu-style>2e687b636f6e74656e743a2280f09f9822e2827d</risu-style>')
style('style-odd-length', `<risu-style>${hex('.i{top:0}')}7</risu-style>`)

// --- the Buffer polyfill's hex and UTF-8 decoding ---
const hexes = {
    'empty': '', 'ascii': '68656c6c6f', 'upper': '48454C4C4F', 'odd': 'abc', 'stop-at-invalid': '6869zz6869', 'one-digit-pair': '616g61', 'digit-then-sign': '6-61',
    'leading-space': ' 6 9', 'sign': '-1+1', 'prefix': '0x6100', 'prefix-upper': '0X61', 'whitespace-pair': '  61', 'nbsp': ' 6161',
    'newline': '\n6', 'ideographic-space': '　6', 'zero-digit': '0g', 'minus-zero': '-0', 'plus-hex': '+f',
    'utf8-two': 'c3a9', 'utf8-three': 'ed959c', 'utf8-four': 'f09f9880', 'overlong-two': 'c0af', 'overlong-three': 'e080af',
    'overlong-four': 'f08080af', 'surrogate': 'eda080', 'above-max': 'f4908080', 'f5': 'f5808080', 'ff': 'ff41',
    'lone-continuation': '80bf41', 'truncated-two': 'c341', 'truncated-three': 'e28241', 'truncated-four': 'f09f9841',
    'truncated-end': 'f09f98', 'cut-three-end': 'e282', 'max': 'f48fbfbf', 'bom': 'efbbbf61', 'nul': '0041',
}
for (const [name, value] of Object.entries(hexes)) c(`hex-${name}`, 'hex', { hex: str(value) })
c('hex-lone-surrogate-unit', 'hex', { hex: str('6\uD80061') })

c('lowercase-ascii', 'lowercase-ascii')

// --- hasCbsSyntax, which tells whether the CBS pass on a decoded style can change it ---
c('cbs-syntax', 'cbs-syntax', {
    texts: [
        'plain', '{{char}}', '{#if x}', '{ {', '{', '{}', 'a{#', '#{', '<user>', '<USER>', '<Char>', '<bot>', '<bots>', '<user', '< user>', '<bot >',
        '<uſer>', '<cHaR>', '{{', 'x<char>y',
    ],
})

// The language strings the Rust pipeline takes as inputs; the oracle checks them against the language file.
const labels = { cot: 'Chain of Thoughts', toolCalled: "Tool '{{tool}}' Called", error: 'Error' }
writeFileSync(join(dir, 'pipeline.cases.json'), `{"schema": 1, "labels": ${JSON.stringify(labels)}, "cases": [\n${cases.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`)
console.log('pipeline', cases.length)
