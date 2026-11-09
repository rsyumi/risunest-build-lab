//! Replays `tests/fixtures/rust-core/render/pipeline.*.json`, recorded by
//! `benchmarks/rust-core-oracles/render-pipeline.oracle.test.ts` from the app's `ParseMarkdown`, `postTranslationParse`,
//! `parseMarkdownSafe` and `trimMarkdown` against a synthetic database.
//!
//! A `display` case runs one request at a time, through a `DisplaySession` and through `render_display`, which must
//! make the same requests and return the same result. Each request must be one the recording's host saw:
//! the asset index lookups of each pass with the names JS lowercased, the paths whose URLs it looked up, the text
//! `processScriptFull` ran on, the inlay ids, the TeX strings and the highlighted blocks. Each answer is what the host
//! returned. An image path answers from `getFileImageSource` on Tauri and falls back to `getFileSrc`, and the lookups
//! this implies must be the ones JS made. The HTML must be what `notrim` returned or what `trimMarkdown` sanitized. The
//! other kinds replay the string stages: the post-translation line trim before the Markdown, the `parseMarkdownSafe`
//! Markdown, the `<risu-style>` decoding and the `</style` escape, the `Buffer` polyfill's hex decoding, `hasCbsSyntax`,
//! and the lowercase table that the `{{source::char}}` and `{{source::user}}` match relies on.

use risunest_ir::budget::{Budget, Error, Limits};
use risunest_ir::text::Text;
use risunest_render::{
    decode_style_hex, escape_style_close, find_math, has_cbs_syntax, locate_highlights, parse_additional_assets, post_translation_lines,
    render_display, render_display_markdown, splice_highlights, AssetAnswers, AssetCharacter, AssetInputs, AssetMode, AssetPath, AssetStep, AssetTag,
    DisplayAnswers, DisplayCharacter, DisplayInput, DisplayLabels, DisplayMode, DisplayReply, DisplayRequest, DisplaySession, DisplayStep, Engine,
    Highlight, HighlightKind, InlayEntry, InlayKind, InlaySource, JsEngine, QuoteSettings, RenderError, ThoughtMode,
};
use serde::de::IgnoredAny;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::{atomic::AtomicBool, Arc};

const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/pipeline.cases.json");
const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/pipeline.expected.json");

#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum Units {
    Plain(String),
    Units { u: Vec<u16> },
}
impl Units {
    fn units(&self) -> Vec<u16> {
        match self {
            Units::Plain(text) => text.encode_utf16().collect(),
            Units::Units { u } => u.clone(),
        }
    }
}

#[derive(Deserialize)]
struct Labels {
    cot: String,
    #[serde(rename = "toolCalled")]
    tool_called: String,
    error: String,
}

#[derive(Deserialize)]
struct CaseFile { schema: u32, labels: Labels, cases: Vec<Case> }

/// The assets, emotions and scripts are the host's: their lookups and runs replay from the recording.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Character {
    cha_id: String,
    #[allow(dead_code)]
    assets: IgnoredAny,
    #[allow(dead_code)]
    emotions: IgnoredAny,
    #[allow(dead_code)]
    scripts: Option<IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inlay { id: String, #[serde(rename = "type")] kind: String, mime: String, width: Option<f64>, height: Option<f64> }

/// What the host reads from `context`, `database`, `settings` and `images` replays from the recording.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Case {
    id: String,
    kind: String,
    source: Option<Units>,
    mode: Option<String>,
    character: Option<Character>,
    #[serde(rename = "chatID")]
    chat_id: Option<f64>,
    #[allow(dead_code)]
    context: Option<IgnoredAny>,
    #[allow(dead_code)]
    database: Option<IgnoredAny>,
    #[allow(dead_code)]
    settings: Option<IgnoredAny>,
    #[allow(dead_code)]
    images: Option<IgnoredAny>,
    platform: Option<String>,
    inlays: Option<Vec<Inlay>>,
    reservation: Option<bool>,
    thought_mode: Option<String>,
    /// Records `styleHistory`, which the replay compares.
    #[allow(dead_code)]
    style_history: Option<bool>,
    hex: Option<Units>,
    texts: Option<Vec<Units>>,
    decline: Option<String>,
    /// A render made first on the same inlay registry; the recording's `firstSlot` is what it left.
    #[allow(dead_code)]
    prior: Option<IgnoredAny>,
    /// Renders with no character selected, which the recording's host carries as `sourcesThrow`.
    #[allow(dead_code)]
    selected: Option<bool>,
}

#[derive(Deserialize)]
struct ExpectedFile {
    schema: u32,
    #[serde(rename = "fixtureSha256")]
    fixture_sha256: String,
    cases: Vec<ExpectedCase>,
}

#[derive(Deserialize)]
struct ExpectedCase { id: String, result: serde_json::Value }

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Quotes {
    custom_quotes: Option<bool>,
    custom_quotes_data: Option<[String; 4]>,
    unformat_quotes: Option<bool>,
    blockquote_styling: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Host {
    asset_width: Option<f64>,
    hide_all_images: bool,
    character_image: Option<Units>,
    user_image: Option<Units>,
    /// Recorded only when it holds: no character is selected and the context gives no image source.
    #[serde(default)]
    sources_throw: bool,
    quotes: Quotes,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Found { src_paths: Vec<Units>, ext: Option<Units> }

type Math = Vec<(Units, Option<Units>)>;
type Highlights = Vec<(Units, String, Option<Units>)>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DisplayRecord {
    output: Option<Units>,
    host: Host,
    lookups: Vec<(usize, String, Units, Option<Found>)>,
    files: Vec<(usize, Units, Units)>,
    images: Vec<(usize, Units, Option<Units>)>,
    scripts: Vec<(Units, Units)>,
    inlay_calls: Vec<Units>,
    math: Math,
    highlights: Highlights,
    /// The slot the registry handed out next when the render began.
    first_slot: u64,
    /// The registrations `parseInlayAssets` made, without those of `applyAssetImageHints` in `trimMarkdown`.
    registrations: Vec<(u64, Units)>,
    style_history: Option<bool>,
    /// Recorded only when it holds: `ParseMarkdown` threw.
    #[serde(default)]
    threw: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PostTranslationRecord { output: Units, quotes: Quotes, math: Math, highlights: Highlights }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkdownSafeRecord { input: Units, quotes: Quotes, math: Math }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StyleElement { hex: Units, parsed: Units, stringified: Option<Units> }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StyleRecord { output: Units, elements: Vec<StyleElement>, styles: Vec<Units> }

fn budget() -> Budget {
    let limits = Limits {
        max_owned_bytes: 1 << 30,
        max_json_bytes: 1 << 24,
        max_text_units: 1 << 24,
        max_values: 1 << 22,
        max_depth: 128,
        max_work: 1 << 30,
    };
    Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits")
}

fn work_budget(max_work: usize) -> Budget {
    let limits = Limits { max_work, ..*budget().limits() };
    Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits")
}

fn text(units: &[u16], budget: &Budget) -> Text { Text::from_units(units, budget).expect("text") }
fn plain(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).expect("text") }
fn units(value: &Text) -> Vec<u16> { value.units().collect() }
fn show(units: &[u16]) -> String { String::from_utf16_lossy(units) }
fn unsupported(error: RenderError) -> String { format!("{error:?}") }
fn differ(what: &str, rust: &[u16], js: &[u16]) -> String { format!("{what}\n  rust {:?}\n  js   {:?}", show(rust), show(js)) }

/// `name.toLocaleLowerCase()` for the synthetic names, which are well formed.
fn lowercase(name: &Text) -> Vec<u16> {
    let name = units(name);
    String::from_utf16(&name).map(|name| name.to_lowercase().encode_utf16().collect()).unwrap_or(name)
}

fn quote_settings(quotes: &Quotes, budget: &Budget) -> Result<QuoteSettings, String> {
    let data = quotes.custom_quotes_data.as_ref().map(|data| data.each_ref().map(|quote| plain(quote, budget)));
    QuoteSettings::from_settings(quotes.custom_quotes.unwrap_or(false), data, quotes.unformat_quotes.unwrap_or(false),
        quotes.blockquote_styling.unwrap_or(false), budget).map_err(unsupported)
}

fn tex_list(tex: &[Text]) -> Vec<Vec<u16>> { tex.iter().map(units).collect() }
fn js_tex(math: &Math) -> Vec<Vec<u16>> { math.iter().map(|(tex, _)| tex.units()).collect() }
fn math_values(math: &Math, budget: &Budget) -> Vec<Option<Text>> { math.iter().map(|(_, value)| value.as_ref().map(|value| text(&value.units(), budget))).collect() }
fn highlight_calls(found: &[Highlight]) -> Vec<(Vec<u16>, &'static str)> {
    found.iter().filter_map(|highlight| match highlight.kind {
        HighlightKind::Language { language, .. } => Some((units(&highlight.code), language)),
        _ => None,
    }).collect()
}
fn js_highlight_calls(highlights: &Highlights) -> Vec<(Vec<u16>, &str)> { highlights.iter().map(|(code, language, _)| (code.units(), language.as_str())).collect() }
fn highlight_values(highlights: &Highlights, budget: &Budget) -> Vec<Option<Text>> {
    highlights.iter().map(|(_, _, value)| value.as_ref().map(|value| text(&value.units(), budget))).collect()
}

/// `renderHighlightableMarkdown` with the recorded KaTeX and highlight.js results.
fn highlightable_markdown(source: &Text, quotes: &QuoteSettings, math: &Math, highlights: &Highlights, error: &Text, budget: &Budget)
    -> Result<Vec<u16>, String> {
    let js = JsEngine::UNICODE_16;
    let tex = tex_list(&find_math(source, budget).map_err(unsupported)?);
    if tex != js_tex(math) { return Err(format!("math {:?}, js {:?}", tex.iter().map(|t| show(t)).collect::<Vec<_>>(), js_tex(math).iter().map(|t| show(t)).collect::<Vec<_>>())); }
    let rendered = render_display_markdown(source, Engine::Highlight, quotes, &math_values(math, budget), &js, budget).map_err(unsupported)?;
    let found = locate_highlights(&rendered, budget).map_err(unsupported)?;
    if highlight_calls(&found) != js_highlight_calls(highlights) {
        return Err(format!("highlight calls {:?}, js {:?}", highlight_calls(&found), js_highlight_calls(highlights)));
    }
    Ok(units(&splice_highlights(&rendered, &found, &highlight_values(highlights, budget), error, budget).map_err(unsupported)?))
}

/// The host answers given so far.
#[derive(Default)]
struct Answers {
    lookups: [Option<Vec<Option<AssetPath>>>; 2],
    urls: [Option<Vec<Text>>; 2],
    scripted: Option<Text>,
    inlays: Option<Vec<InlayEntry>>,
    math: Option<Vec<Option<Text>>>,
    highlights: Option<Vec<Option<Text>>>,
}
impl Answers {
    fn view(&self) -> DisplayAnswers<'_> {
        DisplayAnswers {
            assets: [0, 1].map(|pass| AssetAnswers { lookups: self.lookups[pass].as_deref(), urls: self.urls[pass].as_deref() }),
            scripted: self.scripted.as_ref(),
            inlays: self.inlays.as_deref(),
            math: self.math.as_deref(),
            highlights: self.highlights.as_deref(),
        }
    }

    /// Adds the reply to a request of asset pass `pass`, or to another request.
    fn store(&mut self, pass: usize, reply: DisplayReply) {
        match reply {
            DisplayReply::AssetLookups(found) => self.lookups[pass] = Some(found),
            DisplayReply::AssetUrls(urls) => self.urls[pass] = Some(urls),
            DisplayReply::Scripts(text) => self.scripted = Some(text),
            DisplayReply::Inlays(inlays) => self.inlays = Some(inlays),
            DisplayReply::Math(math) => self.math = Some(math),
            DisplayReply::Highlights(values) => self.highlights = Some(values),
        }
    }
}

/// The two forms of the display render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `render_display` with every answer so far.
    Stateless,
    /// A `DisplaySession` resumed with one reply per request.
    Session,
}

fn copy(value: &Text, budget: &Budget) -> Text { value.copy(budget).expect("copy") }

fn inlay_entry(inlay: &Inlay, budget: &Budget) -> InlayEntry {
    let kind = match inlay.kind.as_str() {
        "image" => InlayKind::Image,
        "video" => InlayKind::Video,
        "audio" => InlayKind::Audio,
        _ => InlayKind::Signature,
    };
    InlayEntry { id: plain(&inlay.id, budget), source: InlaySource { kind, mime: plain(&inlay.mime, budget), width: inlay.width, height: inlay.height } }
}

/// A step as the host reads it, for comparing the two forms.
fn describe(step: &DisplayStep) -> String {
    let texts = |list: &[Text]| list.iter().map(|value| show(&units(value))).collect::<Vec<_>>();
    match step {
        DisplayStep::AssetLookups { pass, lookups } => {
            format!("lookups {pass} {:?}", lookups.iter().map(|lookup| (lookup.tag, show(&units(&lookup.name)))).collect::<Vec<_>>())
        }
        DisplayStep::AssetUrls { pass, requests } => {
            format!("urls {pass} {:?}", requests.iter().map(|request| (show(&units(&request.path)), request.image)).collect::<Vec<_>>())
        }
        DisplayStep::Scripts(text) => format!("scripts {:?}", show(&units(text))),
        DisplayStep::Inlays(ids) => format!("inlays {:?}", texts(ids)),
        DisplayStep::Math(tex) => format!("math {:?}", texts(tex)),
        DisplayStep::Highlights(found) => format!("highlights {:?}", placeholders(found)),
        DisplayStep::Done(output) => format!("done {:?} {:?} {:?} {:?} {} {}", show(&units(&output.html)),
            output.registrations.iter().map(|entry| (entry.slot, show(&units(&entry.id)))).collect::<Vec<_>>(), texts(&output.math),
            placeholders(&output.highlights), output.trim, output.style_history),
    }
}

fn placeholders(found: &[Highlight]) -> Vec<(usize, usize, String, String, HighlightKind)> {
    found.iter().map(|highlight| (highlight.start, highlight.end, show(&units(&highlight.lang)), show(&units(&highlight.code)), highlight.kind)).collect()
}

type FileCall = (usize, Vec<u16>, Vec<u16>);
type ImageCall = (usize, Vec<u16>, Option<Vec<u16>>);

/// Replays one display case in `form`, adding each request and the result to `steps`.
fn replay_display(case: &Case, recorded: &DisplayRecord, labels: &Labels, form: Form, steps: &mut Vec<String>) -> Result<(), String> {
    let budget = budget();
    let host = &recorded.host;
    let source = text(&case.source.as_ref().map(Units::units).unwrap_or_default(), &budget);
    let mode = match case.mode.as_deref() {
        None | Some("normal") => DisplayMode::Normal,
        Some("back") => DisplayMode::Back,
        Some("pretranslate") => DisplayMode::Pretranslate,
        Some("notrim") => DisplayMode::Notrim,
        Some(other) => panic!("mode {other}"),
    };
    let thought_mode = match case.thought_mode.as_deref() {
        Some("recent") => ThoughtMode::Recent,
        Some("collapsed") => ThoughtMode::Collapsed,
        _ => ThoughtMode::Off,
    };
    let cha_id = case.character.as_ref().map(|character| plain(&character.cha_id, &budget));
    let character_image = host.character_image.as_ref().map(|image| text(&image.units(), &budget));
    let user_image = host.user_image.as_ref().map(|image| text(&image.units(), &budget));
    let character = cha_id.as_ref().map(|cha_id| AssetCharacter {
        asset_width: host.asset_width, chat_id: case.chat_id.unwrap_or(-1.0), cha_id, character_image: character_image.as_ref(),
        user_image: user_image.as_ref(), sources_throw: host.sources_throw,
    });
    let quotes = quote_settings(&host.quotes, &budget)?;
    let (cot, tool_called, error) = (plain(&labels.cot, &budget), plain(&labels.tool_called, &budget), plain(&labels.error, &budget));
    let js = JsEngine::UNICODE_16;
    let input = DisplayInput {
        text: &source, mode, character: character.as_ref(), hide_all_images: host.hide_all_images, first_slot: Some(recorded.first_slot),
        image_reservation: case.reservation.unwrap_or(false), thought_mode,
        labels: DisplayLabels { cot: &cot, tool_called: &tool_called, error: &error }, quotes: &quotes, js: &js,
    };
    let tauri = case.platform.as_deref() == Some("tauri");
    let mut answers = Answers::default();
    let mut session = None;
    let mut step = match form {
        Form::Stateless => render_display(&input, &answers.view(), &budget),
        Form::Session => {
            let request = DisplayRequest {
                text: copy(&source, &budget), mode,
                character: character.as_ref().map(|character| DisplayCharacter {
                    asset_width: character.asset_width, chat_id: character.chat_id, cha_id: copy(character.cha_id, &budget),
                    character_image: character.character_image.map(|image| copy(image, &budget)), user_image: character.user_image.map(|image| copy(image, &budget)),
                    sources_throw: character.sources_throw,
                }),
                hide_all_images: input.hide_all_images, first_slot: input.first_slot, image_reservation: input.image_reservation, thought_mode,
                cot: copy(&cot, &budget), tool_called: copy(&tool_called, &budget), error: copy(&error, &budget), quotes: quote_settings(&host.quotes, &budget)?, js,
            };
            DisplaySession::start(request, &budget).map(|(started, step)| {
                session = Some(started);
                step
            })
        }
    }.map_err(unsupported)?;
    let mut asked: Vec<String> = Vec::new();
    let mut lookups_made = 0;
    let (mut files, mut images): (Vec<FileCall>, Vec<ImageCall>) = (Vec::new(), Vec::new());
    for _ in 0..16 {
        steps.push(describe(&step));
        let request = match &step {
            DisplayStep::AssetLookups { pass, .. } => format!("lookups of pass {pass}"),
            DisplayStep::AssetUrls { pass, .. } => format!("URLs of pass {pass}"),
            DisplayStep::Scripts(_) => "scripts".into(),
            DisplayStep::Inlays(_) => "inlays".into(),
            DisplayStep::Math(_) => "math".into(),
            DisplayStep::Highlights(_) => "highlights".into(),
            DisplayStep::Done(_) => "result".into(),
        };
        if asked.contains(&request) { return Err(format!("{request} asked twice")); }
        asked.push(request);
        let (pass, reply) = match step {
            DisplayStep::AssetLookups { pass, lookups } => {
                let js: Vec<_> = recorded.lookups.iter().filter(|lookup| lookup.0 == pass).collect();
                let rust: Vec<(&str, Vec<u16>)> =
                    lookups.iter().map(|lookup| (if lookup.tag == AssetTag::Emotion { "emotion" } else { "additional" }, lowercase(&lookup.name))).collect();
                let js_lookups: Vec<(&str, Vec<u16>)> = js.iter().map(|lookup| (lookup.1.as_str(), lookup.2.units())).collect();
                if rust != js_lookups {
                    let names = |list: &[(&str, Vec<u16>)]| list.iter().map(|(kind, name)| format!("{kind} {}", show(name))).collect::<Vec<_>>();
                    return Err(format!("lookups of pass {pass} {:?}, js {:?}", names(&rust), names(&js_lookups)));
                }
                lookups_made += js.len();
                (pass, DisplayReply::AssetLookups(js.iter().map(|lookup| lookup.3.as_ref().map(|found| AssetPath {
                    src_paths: found.src_paths.iter().map(|path| text(&path.units(), &budget)).collect(),
                    ext: found.ext.as_ref().map(|ext| text(&ext.units(), &budget)),
                })).collect()))
            }
            DisplayStep::AssetUrls { pass, requests } => {
                let mut urls = Vec::with_capacity(requests.len());
                for request in &requests {
                    let path = units(&request.path);
                    let mut url = None;
                    if tauri && request.image {
                        let Some(found) = recorded.images.iter().find(|call| call.0 == pass && call.1.units() == path) else {
                            return Err(format!("no image source recorded for {} in pass {pass}", show(&path)));
                        };
                        url = found.2.as_ref().map(Units::units);
                        images.push((pass, path.clone(), url.clone()));
                    }
                    let url = match url {
                        Some(url) => url,
                        None => {
                            let Some(found) = recorded.files.iter().find(|call| call.0 == pass && call.1.units() == path) else {
                                return Err(format!("no file URL recorded for {} in pass {pass}", show(&path)));
                            };
                            files.push((pass, path, found.2.units()));
                            found.2.units()
                        }
                    };
                    urls.push(text(&url, &budget));
                }
                (pass, DisplayReply::AssetUrls(urls))
            }
            DisplayStep::Scripts(scripted) => {
                let [(js_input, js_output)] = recorded.scripts.as_slice() else { return Err(format!("{} script runs recorded", recorded.scripts.len())) };
                if units(&scripted) != js_input.units() { return Err(differ("script input", &units(&scripted), &js_input.units())); }
                (0, DisplayReply::Scripts(text(&js_output.units(), &budget)))
            }
            DisplayStep::Inlays(ids) => {
                let ids: Vec<Vec<u16>> = ids.iter().map(units).collect();
                let js_ids: Vec<Vec<u16>> = recorded.inlay_calls.iter().map(Units::units).collect();
                if ids != js_ids { return Err(format!("inlays {:?}, js {:?}", ids.iter().map(|id| show(id)).collect::<Vec<_>>(), js_ids.iter().map(|id| show(id)).collect::<Vec<_>>())); }
                (0, DisplayReply::Inlays(case.inlays.iter().flatten().filter(|inlay| ids.contains(&inlay.id.encode_utf16().collect())).map(|inlay| inlay_entry(inlay, &budget)).collect()))
            }
            DisplayStep::Math(tex) => {
                if tex_list(&tex) != js_tex(&recorded.math) { return Err(format!("math {:?}", tex_list(&tex).iter().map(|t| show(t)).collect::<Vec<_>>())); }
                (0, DisplayReply::Math(math_values(&recorded.math, &budget)))
            }
            DisplayStep::Highlights(found) => {
                if highlight_calls(&found) != js_highlight_calls(&recorded.highlights) {
                    return Err(format!("highlight calls {:?}, js {:?}", highlight_calls(&found), js_highlight_calls(&recorded.highlights)));
                }
                (0, DisplayReply::Highlights(highlight_values(&recorded.highlights, &budget)))
            }
            DisplayStep::Done(output) => {
                if recorded.threw { return Err("the app threw, so the render must decline".into()); }
                if lookups_made != recorded.lookups.len() { return Err(format!("{lookups_made} lookups made, js {}", recorded.lookups.len())); }
                let mut js_files: Vec<FileCall> = recorded.files.iter().map(|(pass, path, url)| (*pass, path.units(), url.units())).collect();
                let mut js_images: Vec<ImageCall> =
                    recorded.images.iter().map(|(pass, path, url)| (*pass, path.units(), url.as_ref().map(Units::units))).collect();
                for list in [&mut files, &mut js_files] { list.sort(); }
                for list in [&mut images, &mut js_images] { list.sort(); }
                if files != js_files { return Err(format!("{} getFileSrc calls, js {}", files.len(), js_files.len())); }
                if images != js_images { return Err(format!("{} getFileImageSource calls, js {}", images.len(), js_images.len())); }
                if !asked.iter().any(|request| request == "scripts") && !recorded.scripts.is_empty() { return Err("the scripts never ran".into()); }
                if !asked.iter().any(|request| request == "inlays") && !recorded.inlay_calls.is_empty() { return Err("the inlays were never read".into()); }
                if tex_list(&output.math) != js_tex(&recorded.math) { return Err(format!("{} TeX strings, js {}", output.math.len(), recorded.math.len())); }
                if highlight_calls(&output.highlights) != js_highlight_calls(&recorded.highlights) {
                    return Err(format!("{} highlighted blocks, js {}", highlight_calls(&output.highlights).len(), recorded.highlights.len()));
                }
                let registrations: Vec<(u64, Vec<u16>)> = output.registrations.iter().map(|entry| (entry.slot, units(&entry.id))).collect();
                let js_registrations: Vec<(u64, Vec<u16>)> = recorded.registrations.iter().map(|(slot, id)| (*slot, id.units())).collect();
                if registrations != js_registrations { return Err(format!("registrations {registrations:?}, js {js_registrations:?}")); }
                if output.trim != (mode != DisplayMode::Notrim) { return Err(format!("trim {}", output.trim)); }
                let html = units(&output.html);
                match &recorded.output {
                    Some(js) if html != js.units() => return Err(differ("rendered", &html, &js.units())),
                    // `trimMarkdown` returns '' for empty data before it sanitizes.
                    None if !html.is_empty() || !output.trim => return Err(differ("rendered", &html, &[])),
                    _ => {}
                }
                if let Some(js) = recorded.style_history {
                    if output.style_history != js { return Err(format!("style history {}, js {js}", output.style_history)); }
                }
                return Ok(());
            }
        };
        step = match session.as_mut() {
            None => {
                answers.store(pass, reply);
                render_display(&input, &answers.view(), &budget)
            }
            Some(session) => session.resume(reply, &budget),
        }.map_err(unsupported)?;
    }
    Err("no result after 16 requests".into())
}

fn replay_case(case: &Case, recorded: &serde_json::Value, labels: &Labels, form: Form, steps: &mut Vec<String>) -> Result<(), String> {
    let budget = budget();
    let parse = |error: serde_json::Error| format!("recording: {error}");
    let source = || text(&case.source.as_ref().map(Units::units).unwrap_or_default(), &budget);
    match case.kind.as_str() {
        "display" => replay_display(case, &DisplayRecord::deserialize(recorded).map_err(parse)?, labels, form, steps),
        "post-translation" => {
            let recorded = PostTranslationRecord::deserialize(recorded).map_err(parse)?;
            let lines = post_translation_lines(&source(), &budget).map_err(unsupported)?;
            let quotes = quote_settings(&recorded.quotes, &budget)?;
            let html = highlightable_markdown(&lines, &quotes, &recorded.math, &recorded.highlights, &plain(&labels.error, &budget), &budget)?;
            if html != recorded.output.units() { return Err(differ("rendered", &html, &recorded.output.units())); }
            Ok(())
        }
        "markdown-safe" => {
            let recorded = MarkdownSafeRecord::deserialize(recorded).map_err(parse)?;
            let source = source();
            let tex = tex_list(&find_math(&source, &budget).map_err(unsupported)?);
            if tex != js_tex(&recorded.math) { return Err(format!("{} TeX strings, js {}", tex.len(), recorded.math.len())); }
            let quotes = quote_settings(&recorded.quotes, &budget)?;
            let html = units(&render_display_markdown(&source, Engine::Markdown, &quotes, &math_values(&recorded.math, &budget), &JsEngine::UNICODE_16, &budget)
                .map_err(unsupported)?);
            if html != recorded.input.units() { return Err(differ("sanitize input", &html, &recorded.input.units())); }
            Ok(())
        }
        "style" => {
            let recorded = StyleRecord::deserialize(recorded).map_err(parse)?;
            if recorded.elements.is_empty() { return Err(format!("no <risu-style> element decoded from {:?}", show(&recorded.output.units()))); }
            let mut styles = Vec::new();
            for element in &recorded.elements {
                let decoded = decode_style_hex(&text(&element.hex.units(), &budget), &budget).map_err(unsupported)?;
                // With CBS syntax, `risuChatParser` in the WebView rewrites the CSS before it is parsed.
                if has_cbs_syntax(&decoded) { return Err(format!("{:?} holds CBS syntax", show(&units(&decoded)))); }
                if units(&decoded) != element.parsed.units() { return Err(differ("decoded", &units(&decoded), &element.parsed.units())); }
                if let Some(stringified) = &element.stringified {
                    styles.push(units(&escape_style_close(&text(&stringified.units(), &budget), &budget).map_err(unsupported)?));
                }
            }
            let js_styles: Vec<Vec<u16>> = recorded.styles.iter().map(Units::units).collect();
            if styles != js_styles { return Err(format!("styles {:?}, js {:?}", styles.iter().map(|s| show(s)).collect::<Vec<_>>(), js_styles.iter().map(|s| show(s)).collect::<Vec<_>>())); }
            Ok(())
        }
        "hex" => {
            let recorded = Units::deserialize(recorded).map_err(parse)?;
            let hex = text(&case.hex.as_ref().expect("hex").units(), &budget);
            let decoded = units(&decode_style_hex(&hex, &budget).map_err(unsupported)?);
            if decoded != recorded.units() { return Err(differ("decoded", &decoded, &recorded.units())); }
            Ok(())
        }
        "lowercase-ascii" => {
            let recorded = Vec::<(u32, String, String)>::deserialize(recorded).map_err(parse)?;
            // `{{source::}}` compares its lowercased name with `char` and `user`: no other code point may lowercase to
            // one of their letters, and an ASCII one must lowercase as ASCII does unless it is outside those words.
            for (code, locale, lower) in &recorded {
                let ascii = *code < 0x80;
                if !ascii && lower.chars().any(|letter| "charuse".contains(letter)) { return Err(format!("U+{code:04X} lowercases to {lower:?} in {locale}")); }
                if ascii && "CHARUSEcharuse".contains(char::from_u32(*code).unwrap()) { return Err(format!("{code:#x} lowercases to {lower:?} in {locale}")); }
            }
            if !recorded.iter().any(|(code, _, lower)| *code == 0x212a && lower == "k") { return Err("the table misses U+212A".into()); }
            Ok(())
        }
        "cbs-syntax" => {
            let recorded = Vec::<bool>::deserialize(recorded).map_err(parse)?;
            let texts = case.texts.as_ref().expect("texts");
            let found: Vec<bool> = texts.iter().map(|value| has_cbs_syntax(&text(&value.units(), &budget))).collect();
            if found != recorded { return Err(format!("{found:?}, js {recorded:?}")); }
            Ok(())
        }
        other => panic!("kind {other}"),
    }
}

/// The replay runs on a thread with the 8 MiB stack of a stage thread, like the Markdown replay.
#[test]
fn replays_the_pipeline_oracle() {
    std::thread::Builder::new().stack_size(8 << 20).spawn(replay).unwrap().join().unwrap();
}

fn replay() {
    let cases: CaseFile = serde_json::from_slice(CASES).expect("cases");
    let expected: ExpectedFile = serde_json::from_slice(EXPECTED).expect("expected");
    assert_eq!(expected.schema, cases.schema);
    let digest: String = Sha256::digest(CASES).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(expected.fixture_sha256, digest, "regenerate the expected file");
    assert_eq!(expected.cases.len(), cases.cases.len());
    let mut failures = Vec::new();
    let (mut compared, mut declined, mut both) = (0, 0, 0);
    for (case, recorded) in cases.cases.iter().zip(&expected.cases) {
        assert_eq!(case.id, recorded.id);
        let forms: &[Form] = if case.kind == "display" { &[Form::Session, Form::Stateless] } else { &[Form::Stateless] };
        let mut traces = Vec::new();
        for &form in forms {
            let mut steps = Vec::new();
            let outcome = replay_case(case, &recorded.result, &cases.labels, form, &mut steps);
            let failure = match (&case.decline, outcome) {
                (Some(name), Err(found)) if found == format!("Unsupported({name:?})") => None,
                (Some(name), other) => Some(format!("expected decline {name}, got {other:?}")),
                (None, Err(error)) => Some(error),
                (None, Ok(())) => None,
            };
            let label = if forms.len() > 1 { format!("{} ({form:?})", case.id) } else { case.id.clone() };
            if let Some(failure) = failure { failures.push(format!("{label}: {failure}")); }
            traces.push(steps);
        }
        if forms.len() > 1 {
            both += 1;
            if traces[0] != traces[1] { failures.push(format!("{}: the session and the stateless form differ step by step", case.id)); }
        }
        if case.decline.is_some() { declined += 1 } else { compared += 1 }
    }
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
    println!("pipeline oracle: {compared} compared, {declined} declined, {both} display cases in both forms");
}

/// Host answers whose count differs from the requests, and a found asset without a path, decline by name.
#[test]
fn answers_that_do_not_fit_decline() {
    let budget = budget();
    let source = plain("{{img::a}}", &budget);
    let cha_id = plain("c", &budget);
    let character = AssetCharacter { asset_width: None, chat_id: 0.0, cha_id: &cha_id, character_image: None, user_image: None, sources_throw: false };
    let inputs = AssetInputs { character: &character, hide_all_images: false, mode: AssetMode::Normal };
    let found = [Some(AssetPath { src_paths: vec![plain("p", &budget)], ext: None })];
    let pathless = [Some(AssetPath { src_paths: Vec::new(), ext: None })];
    let attempts: [(AssetAnswers, &str); 3] = [
        (AssetAnswers { lookups: Some(&[]), urls: None }, "asset-lookups"),
        (AssetAnswers { lookups: Some(&found), urls: Some(&[]) }, "asset-urls"),
        (AssetAnswers { lookups: Some(&pathless), urls: None }, "asset-paths"),
    ];
    for (answers, name) in attempts {
        assert!(matches!(parse_additional_assets(&source, &inputs, &answers, &budget), Err(RenderError::Unsupported(found)) if found == name), "{name}");
    }
    // Where TS throws on a `source` tag, only a pass that has one declines.
    let throwing = AssetCharacter { sources_throw: true, ..character };
    let inputs = AssetInputs { character: &throwing, ..inputs };
    assert!(matches!(parse_additional_assets(&source, &inputs, &AssetAnswers::default(), &budget), Ok(AssetStep::Lookups(_))));
    let sourced = plain("{{img::a}} {{source::user}}", &budget);
    assert!(matches!(parse_additional_assets(&sourced, &inputs, &AssetAnswers::default(), &budget), Err(RenderError::Unsupported("asset-source"))));
    let fence = plain("```js\nx()\n```", &budget);
    let (label, quotes, js) = (plain("", &budget), QuoteSettings::from_settings(false, None, false, false, &budget).unwrap(), JsEngine::UNICODE_16);
    let input = DisplayInput {
        text: &fence, mode: DisplayMode::Normal, character: None, hide_all_images: false, first_slot: Some(0), image_reservation: false,
        thought_mode: ThoughtMode::Off, labels: DisplayLabels { cot: &label, tool_called: &label, error: &label }, quotes: &quotes, js: &js,
    };
    let answers = DisplayAnswers { highlights: Some(&[]), ..DisplayAnswers::default() };
    assert!(matches!(render_display(&input, &answers, &budget), Err(RenderError::Unsupported("display-highlights"))));
}

/// A request a test session renders, with the character `c` when `character` holds.
fn test_request(source: &str, character: bool, hide_all_images: bool, budget: &Budget) -> DisplayRequest {
    DisplayRequest {
        text: plain(source, budget), mode: DisplayMode::Normal,
        character: character.then(|| DisplayCharacter {
            asset_width: None, chat_id: 0.0, cha_id: plain("c", budget), character_image: None, user_image: None, sources_throw: false,
        }),
        hide_all_images, first_slot: Some(0), image_reservation: false, thought_mode: ThoughtMode::Off,
        cot: plain("", budget), tool_called: plain("", budget), error: plain("", budget),
        quotes: QuoteSettings::from_settings(false, None, false, false, budget).unwrap(), js: JsEngine::UNICODE_16,
    }
}

/// A session takes one reply per request, of the kind it asked for: another kind declines as `display-reply` and the
/// request stays open for the right one, while a reply after the result or after an error declines. Highlight values
/// that do not fit decline as in the stateless form.
#[test]
fn session_replies_that_do_not_fit_decline() {
    let budget = budget();
    let declines = |session: &mut DisplaySession, reply: DisplayReply| matches!(session.resume(reply, &budget), Err(RenderError::Unsupported("display-reply")));
    let (mut session, step) = DisplaySession::start(test_request("{{img::a}}", true, false, &budget), &budget).unwrap();
    assert!(matches!(step, DisplayStep::AssetLookups { pass: 0, .. }), "{step:?}");
    assert!(declines(&mut session, DisplayReply::Scripts(plain("x", &budget))));
    let step = session.resume(DisplayReply::AssetLookups(vec![None]), &budget).unwrap();
    assert!(!matches!(step, DisplayStep::AssetLookups { pass: 0, .. }), "{step:?}");
    let (mut session, step) = DisplaySession::start(test_request("plain", false, false, &budget), &budget).unwrap();
    assert!(matches!(step, DisplayStep::Done(_)), "{step:?}");
    assert!(declines(&mut session, DisplayReply::Math(Vec::new())));
    let (mut session, step) = DisplaySession::start(test_request("```js\nx()\n```", false, false, &budget), &budget).unwrap();
    assert!(matches!(step, DisplayStep::Highlights(_)), "{step:?}");
    assert!(matches!(session.resume(DisplayReply::Highlights(Vec::new()), &budget), Err(RenderError::Unsupported("display-highlights"))));
    assert!(declines(&mut session, DisplayReply::Highlights(vec![None])));
}

/// A session runs each stage once: it completes within a work limit that the stateless form, which reruns the stages
/// before each request, exceeds on the same render.
#[test]
fn a_session_runs_each_stage_once() {
    let host = budget();
    let source = format!("{{{{inlay::i}}}} $$x$$\n\n```js\nf()\n```\n\n{}", "Some words of prose. ".repeat(200));
    let request = test_request(&source, false, false, &host);
    let inlays = || vec![InlayEntry { id: plain("i", &host), source: InlaySource { kind: InlayKind::Image, mime: plain("image/png", &host), width: None, height: None } }];
    let math = || vec![Some(plain("<math></math>", &host))];
    let highlights = || vec![Some(plain("f()", &host))];
    let session_completes = |work: usize| {
        let budget = work_budget(work);
        let copied = test_request(&source, false, false, &host);
        let (mut session, mut step) = match DisplaySession::start(copied, &budget) {
            Ok(started) => started,
            Err(RenderError::Budget(Error::Limit("work"))) => return false,
            Err(other) => panic!("{other:?}"),
        };
        loop {
            let reply = match step {
                DisplayStep::Inlays(_) => DisplayReply::Inlays(inlays()),
                DisplayStep::Math(_) => DisplayReply::Math(math()),
                DisplayStep::Highlights(_) => DisplayReply::Highlights(highlights()),
                DisplayStep::Done(_) => return true,
                other => panic!("{other:?}"),
            };
            step = match session.resume(reply, &budget) {
                Ok(next) => next,
                Err(RenderError::Budget(Error::Limit("work"))) => return false,
                Err(other) => panic!("{other:?}"),
            };
        }
    };
    let stateless_completes = |work: usize| {
        let budget = work_budget(work);
        let (inlays, math, highlights) = (inlays(), math(), highlights());
        let input = DisplayInput {
            text: &request.text, mode: request.mode, character: None, hide_all_images: false, first_slot: Some(0), image_reservation: false,
            thought_mode: ThoughtMode::Off, labels: DisplayLabels { cot: &request.cot, tool_called: &request.tool_called, error: &request.error },
            quotes: &request.quotes, js: &request.js,
        };
        let mut answers = DisplayAnswers::default();
        loop {
            match render_display(&input, &answers, &budget) {
                Ok(DisplayStep::Inlays(_)) => answers.inlays = Some(&inlays),
                Ok(DisplayStep::Math(_)) => answers.math = Some(&math),
                Ok(DisplayStep::Highlights(_)) => answers.highlights = Some(&highlights),
                Ok(DisplayStep::Done(_)) => return true,
                Ok(other) => panic!("{other:?}"),
                Err(RenderError::Budget(Error::Limit("work"))) => return false,
                Err(other) => panic!("{other:?}"),
            }
        }
    };
    // The least work each form completes in.
    let least = |completes: &dyn Fn(usize) -> bool| {
        let (mut low, mut high) = (1, 1 << 26);
        assert!(completes(high));
        while low < high {
            let middle = (low + high) / 2;
            if completes(middle) { high = middle } else { low = middle + 1 }
        }
        low
    };
    let (session, stateless) = (least(&session_completes), least(&stateless_completes));
    println!("work: session {session}, stateless {stateless}");
    assert!(stateless * 2 >= session * 3, "work: session {session}, stateless {stateless}");
}

/// Between requests a session holds its request and what the remaining stages read, not the replies and texts the
/// stages before have finished with.
#[test]
fn a_session_drops_what_its_stages_have_read() {
    let budget = budget();
    let id = "a".repeat(100_000);
    let base = budget.owned_bytes();
    let request = test_request(&format!("{{{{inlay::{id}}}}} $$x$$"), true, true, &budget);
    let request_bytes = budget.owned_bytes() - base;
    let (mut session, step) = DisplaySession::start(request, &budget).unwrap();
    let DisplayStep::Scripts(scripted) = step else { panic!("{step:?}") };
    // A script that changes the text, so the second asset pass runs.
    let changed = format!("{} ", show(&units(&scripted)));
    drop(scripted);
    let step = session.resume(DisplayReply::Scripts(plain(&changed, &budget)), &budget).unwrap();
    assert!(matches!(&step, DisplayStep::Inlays(ids) if ids.len() == 1), "{step:?}");
    drop(step);
    // With `hideAllImages` the stored image removes its marker, so what the later stages read is small.
    let entry = InlayEntry { id: plain(&id, &budget), source: InlaySource { kind: InlayKind::Image, mime: plain("image/png", &budget), width: None, height: None } };
    let step = session.resume(DisplayReply::Inlays(vec![entry]), &budget).unwrap();
    assert!(matches!(&step, DisplayStep::Math(tex) if tex.len() == 1), "{step:?}");
    drop(step);
    let held = budget.owned_bytes() - base;
    println!("held {held} bytes, the request {request_bytes}");
    assert!(held < request_bytes * 3 / 2, "held {held} bytes, the request {request_bytes}");
}
