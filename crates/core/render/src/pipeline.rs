//! One display render of `parseMarkdownForDisplay` (`src/ts/parser/parser.svelte.ts`), from the message text to the
//! HTML that `trimMarkdown` sanitizes (or that `notrim` returns), with every stage in the app's order: the asset tags,
//! the `editdisplay` scripts, the asset tags again when the scripts changed the text, inlays, thoughts and tool calls,
//! styles, then `renderHighlightableMarkdown` with its KaTeX spans and highlight placeholders.
//!
//! What the stages read from the host comes back as answers: the asset index lookups and URLs of each asset pass, the
//! `processScriptFull(char, data, 'editdisplay', ...)` result (Lua edit listeners, the display trigger, plugins and
//! regex scripts all run there), the stored inlays, the KaTeX results and the highlight.js results. Each request
//! depends on the answer before it, so a render takes them one at a time.
//!
//! Two forms run the same stages. [`DisplaySession`] keeps what each stage computed and takes one [`DisplayReply`]
//! per request, so every stage runs once and the host sends the message once. [`render_display`] is stateless: it
//! takes every answer given so far and reruns the stages before the first request it cannot answer.

use crate::assets::{parse_additional_assets, AssetAnswers, AssetCharacter, AssetInputs, AssetLookup, AssetMode, AssetPath, AssetStep, AssetUrlRequest};
use crate::display::{find_math, render_display_markdown, QuoteSettings};
use crate::error::{RenderError, Result};
use crate::highlight::{locate_highlights, splice_highlights, Highlight, HighlightKind};
use crate::js_engine::JsEngine;
use crate::stages::{encode_style, find_inlays, parse_inlay_assets, parse_thoughts_and_tools, InlayEntry, InlayInputs, InlayRegistration, ThoughtLabels,
    ThoughtMode};
use crate::style::has_risu_style;
use crate::Engine;
use risunest_ir::{Budget, Text};

/// `parseMarkdownForDisplay`'s `mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    /// Markdown, then `trimMarkdown`.
    Normal,
    /// No Markdown, background asset markup, then `trimMarkdown`.
    Back,
    /// No Markdown, then `trimMarkdown`.
    Pretranslate,
    /// Markdown, returned without `trimMarkdown`.
    Notrim,
}

/// The language strings the render reads.
pub struct DisplayLabels<'a> {
    /// `language.cot`.
    pub cot: &'a Text,
    /// `language.toolCalled`.
    pub tool_called: &'a Text,
    /// `language.error`.
    pub error: &'a Text,
}

pub struct DisplayInput<'a> {
    pub text: &'a Text,
    pub mode: DisplayMode,
    /// The character the message renders for, or `None` when `charArg` names none: then no asset tag is read and no
    /// script runs.
    pub character: Option<&'a AssetCharacter<'a>>,
    /// `renderContext.hideAllImages ?? DBState.db.hideAllImages`.
    pub hide_all_images: bool,
    /// The slot the render's inlay registry hands out next, `None` when it is disposed.
    pub first_slot: Option<u64>,
    /// `CSS.supports` for the image reservation properties in the WebView.
    pub image_reservation: bool,
    /// `renderContext.streamingThoughtMode ?? 'off'`.
    pub thought_mode: ThoughtMode,
    pub labels: DisplayLabels<'a>,
    /// The quotes of `renderContext.markdownSettings`, or of `DBState.db` when that is undefined.
    pub quotes: &'a QuoteSettings,
    pub js: &'a JsEngine,
}

/// The host's answers so far, each `None` until the render has asked for it.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisplayAnswers<'a> {
    /// The first asset pass, then the one after the scripts.
    pub assets: [AssetAnswers<'a>; 2],
    /// `processScriptFull(char, data, 'editdisplay', chatID, cbsConditions, ...).data` for the text of
    /// [`DisplayStep::Scripts`].
    pub scripted: Option<&'a Text>,
    /// The stored inlays found for the ids of [`DisplayStep::Inlays`].
    pub inlays: Option<&'a [InlayEntry]>,
    /// One KaTeX result per TeX string of [`DisplayStep::Math`], `None` where it threw.
    pub math: Option<&'a [Option<Text>]>,
    /// One `hljs.highlight(...).value` per [`HighlightKind::Language`] placeholder of [`DisplayStep::Highlights`],
    /// `None` where it threw.
    pub highlights: Option<&'a [Option<Text>]>,
}

/// What the render needs next, or its result.
#[derive(Debug)]
pub enum DisplayStep {
    /// Index lookups of asset pass `pass` (0 before the scripts, 1 after).
    AssetLookups { pass: usize, lookups: Vec<AssetLookup> },
    /// The URLs of asset pass `pass`, one answer per request in this order. TS starts every lookup of a pass in match
    /// order and lets the URL calls interleave; this list is not that call order, which no answer depends on, since
    /// `getFileSrc` and `getFileImageSource` answer per path.
    AssetUrls { pass: usize, requests: Vec<AssetUrlRequest> },
    /// The text the scripts run on.
    Scripts(Text),
    Inlays(Vec<Text>),
    Math(Vec<Text>),
    /// Every placeholder; the host highlights the [`HighlightKind::Language`] ones.
    Highlights(Vec<Highlight>),
    Done(DisplayOutput),
}

#[derive(Debug)]
pub struct DisplayOutput {
    pub html: Text,
    /// Registrations to make in this order on the render's inlay registry.
    pub registrations: Vec<InlayRegistration>,
    /// The TeX strings and placeholders the answers were for.
    pub math: Vec<Text>,
    pub highlights: Vec<Highlight>,
    /// `false` for `notrim`, whose result is `html`. Otherwise the host runs `trimMarkdown` on `html`, first calling
    /// `prepareStyleHistory` when it has one and `style_history` holds.
    pub trim: bool,
    /// `/<risu-style\b/i.test(html)`.
    pub style_history: bool,
}

/// The text after the inlays, thoughts and tool calls, and styles.
struct Styled { text: Text, registrations: Vec<InlayRegistration>, tex: Vec<Text> }

/// What a render has computed, so that a call with one more answer goes on from the stage that waited for it. Each
/// field holds until the next stage has used it.
#[derive(Default)]
struct Progress {
    /// The first asset pass's result, which the scripts run on.
    first: Option<Text>,
    /// The text the inlays read: after the scripts and the second asset pass.
    data: Option<Text>,
    styled: Option<Styled>,
    /// The Markdown with its highlight placeholders.
    rendered: Option<(Text, Vec<Highlight>)>,
}

fn asset_pass(data: &Text, inputs: &AssetInputs, answers: &AssetAnswers, pass: usize, budget: &Budget) -> Result<std::result::Result<Text, DisplayStep>> {
    Ok(match parse_additional_assets(data, inputs, answers, budget)? {
        AssetStep::Lookups(lookups) => Err(DisplayStep::AssetLookups { pass, lookups }),
        AssetStep::Urls(requests) => Err(DisplayStep::AssetUrls { pass, requests }),
        AssetStep::Done(text) => Ok(text),
    })
}

fn copy_highlights(found: &[Highlight], budget: &Budget) -> Result<Vec<Highlight>> {
    found.iter().map(|highlight| Ok(Highlight { start: highlight.start, end: highlight.end, lang: highlight.lang.copy(budget)?,
        code: highlight.code.copy(budget)?, kind: highlight.kind })).collect()
}

/// Runs the stages `progress` has not finished with `answers`, up to the first request they leave open.
fn advance(input: &DisplayInput, answers: &DisplayAnswers, progress: &mut Progress, budget: &Budget) -> Result<DisplayStep> {
    if progress.data.is_none() && progress.styled.is_none() {
        let data = match input.character {
            None => input.text.copy(budget)?,
            Some(character) => {
                let mode = if input.mode == DisplayMode::Back { AssetMode::Back } else { AssetMode::Normal };
                let inputs = AssetInputs { character, hide_all_images: input.hide_all_images, mode };
                if progress.first.is_none() {
                    match asset_pass(input.text, &inputs, &answers.assets[0], 0, budget)? {
                        Ok(text) => progress.first = Some(text),
                        Err(step) => return Ok(step),
                    }
                }
                let first = progress.first.as_ref().expect("first asset pass");
                let Some(scripted) = answers.scripted else { return Ok(DisplayStep::Scripts(first.copy(budget)?)) };
                let data = if scripted.equals(first) {
                    scripted.copy(budget)?
                } else {
                    match asset_pass(scripted, &inputs, &answers.assets[1], 1, budget)? {
                        Ok(text) => text,
                        Err(step) => return Ok(step),
                    }
                };
                progress.first = None;
                data
            }
        };
        progress.data = Some(data);
    }
    if progress.styled.is_none() {
        let data = progress.data.as_ref().expect("scripted text");
        let ids = find_inlays(data, budget)?;
        let sources = match answers.inlays {
            _ if ids.is_empty() => &[][..],
            None => return Ok(DisplayStep::Inlays(ids)),
            Some(sources) => sources,
        };
        let inlay_inputs = InlayInputs { sources, hide_all_images: input.hide_all_images, first_slot: input.first_slot, image_reservation: input.image_reservation };
        let inlaid = parse_inlay_assets(data, &inlay_inputs, budget)?;
        progress.data = None;
        let labels = ThoughtLabels { cot: input.labels.cot, tool_called: input.labels.tool_called };
        let thoughts = parse_thoughts_and_tools(&inlaid.text, input.thought_mode, &labels, input.js, budget)?;
        let text = encode_style(&thoughts, budget)?;
        drop(thoughts);
        let tex = match input.mode {
            DisplayMode::Back | DisplayMode::Pretranslate => Vec::new(),
            DisplayMode::Normal | DisplayMode::Notrim => find_math(&text, budget)?,
        };
        progress.styled = Some(Styled { text, registrations: inlaid.registrations, tex });
    }
    let styled = progress.styled.as_ref().expect("styled text");
    let html = match input.mode {
        DisplayMode::Back | DisplayMode::Pretranslate => None,
        DisplayMode::Normal | DisplayMode::Notrim => {
            if progress.rendered.is_none() {
                let results = match answers.math {
                    _ if styled.tex.is_empty() => &[][..],
                    None => return Ok(DisplayStep::Math(styled.tex.iter().map(|tex| tex.copy(budget)).collect::<std::result::Result<_, _>>()?)),
                    Some(results) => results,
                };
                let rendered = render_display_markdown(&styled.text, Engine::Highlight, input.quotes, results, input.js, budget)?;
                let found = locate_highlights(&rendered, budget)?;
                progress.rendered = Some((rendered, found));
            }
            let (rendered, found) = progress.rendered.as_ref().expect("rendered Markdown");
            let languages = found.iter().filter(|highlight| matches!(highlight.kind, HighlightKind::Language { .. })).count();
            let values = match answers.highlights {
                _ if languages == 0 => &[][..],
                None => return Ok(DisplayStep::Highlights(copy_highlights(found, budget)?)),
                Some(values) if values.len() != languages => return Err(RenderError::Unsupported("display-highlights")),
                Some(values) => values,
            };
            Some(splice_highlights(rendered, found, values, input.labels.error, budget)?)
        }
    };
    let styled = progress.styled.take().expect("styled text");
    let (html, highlights) = match html {
        Some(html) => (html, progress.rendered.take().expect("rendered Markdown").1),
        None => (styled.text, Vec::new()),
    };
    let style_history = has_risu_style(&html);
    Ok(DisplayStep::Done(DisplayOutput { html, registrations: styled.registrations, math: styled.tex, highlights, trim: input.mode != DisplayMode::Notrim,
        style_history }))
}

/// `parseMarkdownForDisplay(data, charArg, mode, chatID, cbsConditions, renderContext)` up to `trimMarkdown`, as one
/// stateless step: it runs the stages with `answers` and returns the first request they leave open, or the result.
/// The host calls it again with that answer added. Highlight answers whose count differs from the `Language`
/// placeholders decline as `display-highlights`.
pub fn render_display(input: &DisplayInput, answers: &DisplayAnswers, budget: &Budget) -> Result<DisplayStep> {
    advance(input, answers, &mut Progress::default(), budget)
}

/// The character fields of a [`DisplayRequest`], owned.
pub struct DisplayCharacter {
    pub asset_width: Option<f64>,
    pub chat_id: f64,
    pub cha_id: Text,
    pub character_image: Option<Text>,
    pub user_image: Option<Text>,
    /// As [`AssetCharacter::sources_throw`].
    pub sources_throw: bool,
}

/// A [`DisplayInput`] with owned strings, which a [`DisplaySession`] keeps for the whole render.
pub struct DisplayRequest {
    pub text: Text,
    pub mode: DisplayMode,
    pub character: Option<DisplayCharacter>,
    pub hide_all_images: bool,
    pub first_slot: Option<u64>,
    pub image_reservation: bool,
    pub thought_mode: ThoughtMode,
    /// `language.cot`, `language.toolCalled` and `language.error`.
    pub cot: Text,
    pub tool_called: Text,
    pub error: Text,
    pub quotes: QuoteSettings,
    pub js: JsEngine,
}

/// The host's answer to the last [`DisplayStep`] of a [`DisplaySession`], as [`DisplayAnswers`] describes each.
#[derive(Debug)]
pub enum DisplayReply {
    AssetLookups(Vec<Option<AssetPath>>),
    AssetUrls(Vec<Text>),
    Scripts(Text),
    Inlays(Vec<InlayEntry>),
    Math(Vec<Option<Text>>),
    Highlights(Vec<Option<Text>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending { Lookups(usize), Urls(usize), Scripts, Inlays, Math, Highlights }

/// The replies a session holds while a stage may still read them.
#[derive(Default)]
struct Replies {
    lookups: [Option<Vec<Option<AssetPath>>>; 2],
    urls: [Option<Vec<Text>>; 2],
    scripted: Option<Text>,
    inlays: Option<Vec<InlayEntry>>,
    math: Option<Vec<Option<Text>>>,
    highlights: Option<Vec<Option<Text>>>,
}

/// One display render that keeps its stages' results between requests: [`DisplaySession::start`] runs up to the
/// first request, and each [`DisplaySession::resume`] takes the reply to the last one and runs on to the next request
/// or the result. It makes the same requests and returns the same output as [`render_display`] fed the same answers.
/// Every call takes the same budget, whose limits then bound the whole render. A reply of another kind than the
/// request declines as `display-reply` and leaves the request open; one after the result or an error declines too.
pub struct DisplaySession {
    request: DisplayRequest,
    replies: Replies,
    progress: Progress,
    pending: Option<Pending>,
}

impl DisplaySession {
    pub fn start(request: DisplayRequest, budget: &Budget) -> Result<(DisplaySession, DisplayStep)> {
        let mut session = DisplaySession { request, replies: Replies::default(), progress: Progress::default(), pending: None };
        let step = session.advance(budget)?;
        Ok((session, step))
    }

    pub fn resume(&mut self, reply: DisplayReply, budget: &Budget) -> Result<DisplayStep> {
        let replies = &mut self.replies;
        match (self.pending, reply) {
            (Some(Pending::Lookups(pass)), DisplayReply::AssetLookups(found)) => replies.lookups[pass] = Some(found),
            (Some(Pending::Urls(pass)), DisplayReply::AssetUrls(urls)) => replies.urls[pass] = Some(urls),
            (Some(Pending::Scripts), DisplayReply::Scripts(text)) => replies.scripted = Some(text),
            (Some(Pending::Inlays), DisplayReply::Inlays(inlays)) => replies.inlays = Some(inlays),
            (Some(Pending::Math), DisplayReply::Math(math)) => replies.math = Some(math),
            (Some(Pending::Highlights), DisplayReply::Highlights(values)) => replies.highlights = Some(values),
            _ => return Err(RenderError::Unsupported("display-reply")),
        }
        self.pending = None;
        self.advance(budget)
    }

    fn advance(&mut self, budget: &Budget) -> Result<DisplayStep> {
        let request = &self.request;
        let character = request.character.as_ref().map(|character| AssetCharacter {
            asset_width: character.asset_width, chat_id: character.chat_id, cha_id: &character.cha_id, character_image: character.character_image.as_ref(),
            user_image: character.user_image.as_ref(), sources_throw: character.sources_throw,
        });
        let input = DisplayInput {
            text: &request.text, mode: request.mode, character: character.as_ref(), hide_all_images: request.hide_all_images,
            first_slot: request.first_slot, image_reservation: request.image_reservation, thought_mode: request.thought_mode,
            labels: DisplayLabels { cot: &request.cot, tool_called: &request.tool_called, error: &request.error }, quotes: &request.quotes, js: &request.js,
        };
        let replies = &self.replies;
        let answers = DisplayAnswers {
            assets: [0, 1].map(|pass| AssetAnswers { lookups: replies.lookups[pass].as_deref(), urls: replies.urls[pass].as_deref() }),
            scripted: replies.scripted.as_ref(),
            inlays: replies.inlays.as_deref(),
            math: replies.math.as_deref(),
            highlights: replies.highlights.as_deref(),
        };
        let step = advance(&input, &answers, &mut self.progress, budget)?;
        self.pending = match &step {
            DisplayStep::AssetLookups { pass, .. } => Some(Pending::Lookups(*pass)),
            DisplayStep::AssetUrls { pass, .. } => Some(Pending::Urls(*pass)),
            DisplayStep::Scripts(_) => Some(Pending::Scripts),
            DisplayStep::Inlays(_) => Some(Pending::Inlays),
            DisplayStep::Math(_) => Some(Pending::Math),
            DisplayStep::Highlights(_) => Some(Pending::Highlights),
            DisplayStep::Done(_) => None,
        };
        // The stages before this request keep their results in `progress`, so the replies they read are dropped.
        let stage = match self.pending {
            Some(Pending::Lookups(_) | Pending::Urls(_) | Pending::Scripts) => 0,
            Some(Pending::Inlays) => 1,
            Some(Pending::Math) => 2,
            Some(Pending::Highlights) => 3,
            None => 4,
        };
        let replies = &mut self.replies;
        if stage >= 1 {
            replies.lookups = [None, None];
            replies.urls = [None, None];
            replies.scripted = None;
        }
        if stage >= 2 { replies.inlays = None; }
        if stage >= 3 { replies.math = None; }
        if stage >= 4 { replies.highlights = None; }
        Ok(step)
    }
}
