//! What the WebView's JavaScript engine decides with its own Unicode tables: the case mapping behind markdown-it's
//! `normalizeReference` and the grapheme clusters of `Intl.Segmenter` behind the streaming thought preview.

/// The Unicode version of one set of the engine's tables, as the probes on [`JsEngine`] classify it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnicodeVersion {
    /// Older than Unicode 16.
    Before16,
    /// Unicode 16, the tables of the recording engine.
    Unicode16,
    /// Unicode 17, the tables of Rust's case mapping.
    Unicode17,
    /// Unicode 18 or newer.
    From18,
}

/// The Unicode tables of the WebView's JavaScript engine. The host runs these probes once in the WebView and passes
/// the result to every render.
///
/// `case_unicode`, the tables of `String.prototype.toLowerCase` and `toUpperCase`, checked in this order:
/// 1. `'\u{10D70}'.toUpperCase() === '\u{10D50}'` is false (GARAY SMALL LETTER A, new in Unicode 16): `Before16`.
/// 2. `'\u{277}'.toUpperCase() !== '\u{277}'` or `'\u{A7DD}'.toLowerCase() !== '\u{A7DD}'` is true (Unicode 18 adds
///    the letter U+A7DD, which lowercases to U+0277): `From18`.
/// 3. `'\u{A7D3}'.toUpperCase() !== '\u{A7D3}'` is true (LATIN SMALL LETTER DOUBLE THORN, whose capital Unicode 17 adds):
///    `Unicode17`.
/// 4. Otherwise `Unicode16`.
///
/// `segmenter`, the tables of `new Intl.Segmenter(undefined, { granularity: 'grapheme' })`, with `first(s)` the first
/// segment it finds in `s`:
/// 1. `typeof Intl.Segmenter !== 'function'`: `None`.
/// 2. The case probes read `From18`: `From18`. No grapheme probe for Unicode 18 is known, and one ICU serves both the
///    case mapping and the segmenter in the WebView engines.
/// 3. `first('\u{113D1}a') !== '\u{113D1}a'` (TULU-TIGALARI REPHA, a prepending mark from Unicode 16): `Before16`.
/// 4. `first('a\u{1ACF}') === 'a\u{1ACF}'` (U+1ACF, an extending mark from Unicode 17): `Unicode17`.
/// 5. Otherwise `Unicode16`.
///
/// The preview builds its segmenter once, when `src/ts/parser/streamingThoughtPreview.ts` loads. The `segmenter` probe
/// therefore runs in that module's realm at that same point, before later code can replace `Intl.Segmenter`: a probe
/// run in another realm or afterwards can describe a segmenter the preview does not use.
///
/// `case_unicode: None` stands for an engine that was not probed and declines the most.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsEngine {
    pub case_unicode: Option<UnicodeVersion>,
    /// `None` when the engine has no `Intl.Segmenter`.
    pub segmenter: Option<UnicodeVersion>,
}

impl JsEngine {
    /// Unicode 16 tables for both, as in Node 24.12 (ICU 77.1), the engine the oracles record.
    pub const UNICODE_16: JsEngine = JsEngine { case_unicode: Some(UnicodeVersion::Unicode16), segmenter: Some(UnicodeVersion::Unicode16) };
}
