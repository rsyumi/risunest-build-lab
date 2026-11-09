//! The `<pre-hljs-placeholder>` blocks `renderHighlightableMarkdown` (`src/ts/parser/parser.svelte.ts`) finds in the
//! `mdHighlight` output and replaces: the WebView highlights the ones that name a highlight.js language, and
//! [`splice_highlights`] writes every replacement.

use crate::error::Result;
use crate::meter::Meter;
use crate::utils::{escape_html, find, s, units_of, Units};
use risunest_ir::{Budget, Text};

/// What `renderHighlightableMarkdown` does with a placeholder, from its `lang`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    /// `hljs.highlight(code, { language, ignoreIllegals: true })` inside `<pre class="hljs" x-hl-lang="…">`.
    Language { language: &'static str, file_ext: &'static str },
    /// `risuerror`: the code in a `<div class="risu-error">` under the localized `language.error`.
    Error,
    /// Any other language: the escaped code in `<pre><code>`.
    Plain,
}

/// One placeholder `rendered.match(/<pre-hljs-placeholder lang="(.+?)">(.+?)<\/pre-hljs-placeholder>/gms)` returns.
#[derive(Debug)]
pub struct Highlight {
    /// `[start, end)` of the placeholder in UTF-16 units of the rendered HTML.
    pub start: usize,
    pub end: usize,
    /// `placeholder.match(/lang="(.+?)"/)[1]`, before the language switch.
    pub lang: Text,
    /// The text highlight.js receives, as the placeholder holds it.
    pub code: Text,
    pub kind: HighlightKind,
}

/// The language switch of `renderHighlightableMarkdown`, with the extension `x-hl-lang` carries.
fn classify(lang: &[u16]) -> HighlightKind {
    let lang = String::from_utf16_lossy(lang);
    let (language, file_ext): (&'static str, &'static str) = match lang.as_str() {
        "bash" => ("bash", "sh"),
        "c" => ("cpp", "c"),
        "cpp" => ("cpp", "cpp"),
        "cs" | "csharp" => ("csharp", "cs"),
        "css" => ("css", "css"),
        "dart" => ("dart", "dart"),
        "html" => ("xml", "html"),
        "svg" => ("xml", "svg"),
        "xml" => ("xml", "xml"),
        "java" => ("java", "java"),
        "js" | "jsx" | "javascript" => ("javascript", "js"),
        "json" => ("json", "json"),
        "lua" => ("lua", "lua"),
        "markdown" | "md" => ("markdown", "md"),
        "py" | "python" => ("python", "py"),
        "rust" => ("rust", "rs"),
        "shell" => ("shell", "sh"),
        "ts" | "tsx" | "typescript" => ("typescript", "ts"),
        "txt" => ("plaintext", "txt"),
        "vtt" => ("plaintext", "vtt"),
        "yaml" => ("yaml", "yml"),
        "risuerror" => return HighlightKind::Error,
        _ => return HighlightKind::Plain,
    };
    // A lossy decode cannot turn a lone surrogate into one of these ASCII names.
    HighlightKind::Language { language, file_ext }
}

/// `placeholder.match(/lang="(.+?)"/)[1]`, without the `s` flag: one unit or more up to the first quote, with no line
/// terminator before it, at the first `lang="` where that holds.
fn inner_lang(placeholder: &[u16]) -> Option<(usize, usize)> {
    let terminator = |unit: u16| matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029);
    let mut from = 0;
    while let Some(at) = find(placeholder, "lang=\"", from) {
        let content = at + 6;
        if content < placeholder.len() && !terminator(placeholder[content]) {
            for end in content + 1..placeholder.len() {
                if placeholder[end] == 0x22 { return Some((content, end)); }
                if terminator(placeholder[end]) { break; }
            }
        }
        from = at + 1;
    }
    None
}

const OPEN: &str = "<pre-hljs-placeholder lang=\"";
const CLOSE: &str = "</pre-hljs-placeholder>";

/// The placeholders of `rendered` (the `mdHighlight` output after [`crate::render_display_markdown`]), in order.
pub fn locate_highlights(rendered: &Text, budget: &Budget) -> Result<Vec<Highlight>> {
    let meter = Meter::new(budget);
    let html: Units = units_of(&rendered);
    meter.alloc(2 * html.len())?;
    meter.work(html.len())?;
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(start) = find(&html, OPEN, index) {
        let lang_start = start + OPEN.len();
        // `lang="(.+?)">(.+?)<\/pre-hljs-placeholder>`: the first `">` one unit or more in, then the first close tag
        // one unit or more after it. When that close tag is missing, no later `">` can have one either.
        let quote = (lang_start + 1..html.len().saturating_sub(1)).find(|&at| html[at] == 0x22 && html[at + 1] == 0x3e);
        let close = quote.and_then(|quote| find(&html, CLOSE, quote + 3).map(|close| (quote, close)));
        let Some((quote, close)) = close else {
            index = start + 1;
            continue;
        };
        let end = close + CLOSE.len();
        let placeholder = &html[start..end];
        index = end;
        // Without a `lang` the loop skips the placeholder, and so would it for any later copy of the same text.
        let Some((lang_start, lang_end)) = inner_lang(placeholder) else { continue };
        let lang = &placeholder[lang_start..lang_end];
        let code = &html[quote + 2..close];
        found.push(Highlight {
            start,
            end,
            lang: Text::from_units(lang, budget)?,
            code: Text::from_units(code, budget)?,
            kind: classify(lang),
        });
        meter.alloc(2 * (lang.len() + code.len()) + 64)?;
    }
    meter.finish()?;
    Ok(found)
}

/// `renderHighlightableMarkdown`'s replacements. `highlighted` holds `hljs.highlight(...).value` for each
/// [`HighlightKind::Language`] placeholder in order, or `None` where it threw and the placeholder stays; `error`
/// is `language.error`. `rendered.replace(placeholder, ...)` replaces the first occurrence of each placeholder's text,
/// which is the placeholder itself: an earlier copy of the same text would have matched first.
pub fn splice_highlights(rendered: &Text, highlights: &[Highlight], highlighted: &[Option<Text>], error: &Text, budget: &Budget)
    -> Result<Text> {
    let meter = Meter::new(budget);
    let html: Units = units_of(&rendered);
    meter.alloc(4 * html.len())?;
    meter.work(html.len())?;
    let mut out = Units::with_capacity(html.len());
    let mut pos = 0;
    let mut values = highlighted.iter();
    for highlight in highlights {
        let code: Units = units_of(&highlight.code);
        let mut replacement = Units::new();
        match highlight.kind {
            HighlightKind::Plain => {
                replacement.extend(s("<pre><code>"));
                escape_html(&code, &mut replacement);
                replacement.extend(s("</code></pre>"));
            }
            HighlightKind::Error => {
                replacement.extend(s("<div class=\"risu-error\"><h1>"));
                replacement.extend(error.units());
                replacement.extend(s("</h1>"));
                escape_html(&code, &mut replacement);
                replacement.extend(s("</div>"));
            }
            HighlightKind::Language { file_ext, .. } => {
                let Some(Some(value)) = values.next() else { continue };
                replacement.extend(s(&format!("<pre class=\"hljs\" x-hl-lang=\"{file_ext}\"><code>")));
                replacement.extend(value.units());
                replacement.extend(s("</code></pre>"));
            }
        }
        // A replacer function inserts its result as written.
        out.extend_from_slice(&html[pos..highlight.start]);
        out.extend_from_slice(&replacement);
        pos = highlight.end;
        meter.alloc(2 * replacement.len())?;
        meter.text(out.len())?;
    }
    out.extend_from_slice(&html[pos..]);
    meter.finish()?;
    Ok(Text::from_units(&out, budget)?)
}
