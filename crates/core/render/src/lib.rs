//! Markdown to HTML with the output of markdown-it 14.3.2 under the app's configuration (`src/ts/parser/parser.svelte.ts`):
//! the `default` preset with `html: true`, `breaks: true`, `linkify: false`, `typographer: true`, the private-use
//! `quotes` and the `code` rule disabled, rendered either without `highlight` (`md`) or with the `mdHighlight`
//! placeholder. The result is the HTML `md.render` returns, before DOMPurify. A source the port cannot render exactly
//! returns [`RenderError::Unsupported`] before any output.
//!
//! Around it sit the string steps of the app's display path: [`render_display_markdown`] (`renderMarkdown`),
//! [`locate_highlights`] and [`splice_highlights`] (`renderHighlightableMarkdown`), the stages before Markdown in
//! [`parse_additional_assets`], [`parse_inlay_assets`], [`parse_thoughts_and_tools`] and [`encode_style`],
//! [`post_translation_lines`] (`postTranslationParse`), and the string steps of the `<risu-style>` swap after
//! DOMPurify in [`decode_style_hex`] and [`escape_style_close`]. [`DisplaySession`] runs one display render through
//! them in the app's order, asking the host for what they read, and [`render_display`] is its stateless form.
//!
//! Reference labels and the streaming thought preview depend on the Unicode tables of the WebView's JavaScript
//! engine, which the host describes with a [`JsEngine`].

mod assets;
mod block;
mod core_rules;
mod display;
mod entities;
mod error;
mod highlight;
mod html;
mod inline;
mod js_engine;
mod links;
mod meter;
mod pipeline;
mod punycode;
mod renderer;
mod stages;
mod style;
mod tables;
mod token;
mod url;
mod utils;

pub use assets::{parse_additional_assets, AssetAnswers, AssetCharacter, AssetInputs, AssetLookup, AssetMode, AssetPath, AssetStep, AssetTag,
    AssetUrlRequest};
pub use display::{find_math, post_translation_lines, render_display_markdown, QuoteMode, QuoteSettings};
pub use error::RenderError;
pub use highlight::{locate_highlights, splice_highlights, Highlight, HighlightKind};
pub use js_engine::{JsEngine, UnicodeVersion};
pub use pipeline::{
    render_display, DisplayAnswers, DisplayCharacter, DisplayInput, DisplayLabels, DisplayMode, DisplayOutput, DisplayReply, DisplayRequest, DisplaySession,
    DisplayStep,
};
pub use stages::{
    encode_style, find_inlays, parse_inlay_assets, parse_thoughts_and_tools, InlayEntry, InlayInputs, InlayKind, InlayRegistration, InlayResult,
    InlaySource, ThoughtLabels, ThoughtMode,
};
pub use style::{decode_style_hex, escape_style_close, has_cbs_syntax, has_risu_style};

use meter::Meter;
use risunest_ir::{Budget, Text};
use token::Kind;
use utils::{units_of, Units};

/// Which markdown-it instance renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// `md`: no `highlight`.
    Markdown,
    /// `mdHighlight`: a fence with a language becomes `<pre-hljs-placeholder lang="…">` around its raw content.
    Highlight,
}

/// `options.quotes` of the app.
const QUOTES: [u16; 4] = [0xe9b0, 0xe9b1, 0xe9b2, 0xe9b3];

/// `md.render(source)` with a fresh `env`, in an engine with the case tables of `js`.
///
/// Parsing recurses through nested blockquotes, lists, links, images and link label scans. markdown-it's `maxNesting`
/// of 100 holds that recursion near 100 levels, and the port declines past 256 levels with
/// `Unsupported("markdown-depth")`. Call it on a thread with the 8 MiB stack of a stage thread; the deepest nestings
/// the oracle replays fit in 1 MiB in a debug build.
pub fn render_markdown(source: &Text, engine: Engine, js: &JsEngine, budget: &Budget) -> Result<Text, RenderError> {
    let meter = Meter::new(budget);
    meter.alloc(2 * source.len_units())?;
    let input: Vec<u16> = units_of(&source);
    let out = markdown_units(&input, engine, js, &meter)?;
    drop(input);
    meter.finish()?;
    Ok(Text::from_units(&out, budget)?)
}

/// [`render_markdown`] over UTF-16 units, charging `meter` for the normalized copy and the HTML.
pub(crate) fn markdown_units(input: &[u16], engine: Engine, js: &JsEngine, meter: &Meter) -> Result<Units, RenderError> {
    meter.alloc(2 * input.len())?;
    let src = core_rules::normalize(input);
    let mut out = Vec::new();
    if !src.is_empty() {
        let (mut tokens, references) = block::parse(&src, js.case_unicode, meter)?;
        for token in tokens.iter_mut() {
            if token.kind == Kind::Inline { token.children = inline::parse(&token.content, &references, js.case_unicode, meter)?; }
        }
        core_rules::replacements(&mut tokens, meter)?;
        core_rules::smartquotes(&mut tokens, QUOTES, meter)?;
        core_rules::text_join(&mut tokens);
        let mut renderer = renderer::Renderer { engine, meter, out: Vec::new() };
        renderer.render(&tokens)?;
        out = renderer.out;
    }
    meter.alloc(2 * out.len())?;
    Ok(out)
}
