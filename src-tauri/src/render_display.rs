//! The display render of `parseMarkdownForDisplay` in Rust (`risunest-render`'s `DisplaySession`), behind three
//! commands. `render_display_start` runs a message up to the first request the WebView answers, `render_display_resume`
//! takes that answer and runs on to the next request or the HTML, and `render_display_cancel` drops a session the page
//! no longer needs. Open sessions live in a capped map that a reload of the main page empties.
//!
//! Strings travel as JSON strings, or as `{ "u": [...] }` UTF-16 units when they hold an isolated surrogate, which a
//! JSON string cannot carry into Rust.

use risunest_ir::budget::{Budget, Limits};
use risunest_ir::Text;
use risunest_render::{
    AssetPath, AssetTag, DisplayCharacter, DisplayMode, DisplayReply, DisplayRequest, DisplaySession, DisplayStep, HighlightKind, InlayEntry,
    InlayKind, InlaySource, JsEngine, QuoteSettings, RenderError, ThoughtMode, UnicodeVersion,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Sessions waiting for an answer; past this the oldest is dropped.
const MAX_SESSIONS: usize = 64;
/// The Markdown rules recurse with the nesting of the source.
const STACK_BYTES: usize = 8 << 20;
/// What one render may own. A Markdown-heavy message peaks near 100 bytes per unit, so this holds messages several
/// times the size the page routes here.
const SESSION_BYTES: usize = 64 << 20;
/// What the sessions together may own: the bytes the waiting ones hold, plus `SESSION_BYTES` for each render that runs.
const TOTAL_BYTES: usize = 256 << 20;

/// One render's limits. A render that reaches one fails as `budget` and the page renders the message in TS.
fn limits(session_bytes: usize) -> Limits {
    Limits {
        max_owned_bytes: session_bytes,
        max_json_bytes: 1 << 20,
        max_text_units: 16 << 20,
        max_values: 1 << 20,
        max_depth: 128,
        max_work: 1 << 31,
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum WireText {
    Text(String),
    Units { u: Vec<u16> },
}

impl WireText {
    fn text(&self, budget: &Budget) -> Result<Text, RenderError> {
        Ok(match self {
            WireText::Text(value) => Text::from_str(value, budget)?,
            WireText::Units { u } => Text::from_units(u, budget)?,
        })
    }

    fn of(text: &Text) -> WireText {
        if let Some(value) = text.as_unicode() {
            return WireText::Text(value.to_owned());
        }
        let units: Vec<u16> = text.units().collect();
        match String::from_utf16(&units) {
            Ok(value) => WireText::Text(value),
            Err(_) => WireText::Units { u: units },
        }
    }
}

fn texts(list: &[WireText], budget: &Budget) -> Result<Vec<Text>, RenderError> {
    list.iter().map(|value| value.text(budget)).collect()
}

fn optional(value: &Option<WireText>, budget: &Budget) -> Result<Option<Text>, RenderError> {
    value.as_ref().map(|value| value.text(budget)).transpose()
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireMode {
    Normal,
    Back,
    Pretranslate,
    Notrim,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireThoughtMode {
    Off,
    Recent,
    Collapsed,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WireUnicode {
    Before16,
    Unicode16,
    Unicode17,
    From18,
}

impl WireUnicode {
    fn version(self) -> UnicodeVersion {
        match self {
            WireUnicode::Before16 => UnicodeVersion::Before16,
            WireUnicode::Unicode16 => UnicodeVersion::Unicode16,
            WireUnicode::Unicode17 => UnicodeVersion::Unicode17,
            WireUnicode::From18 => UnicodeVersion::From18,
        }
    }
}

/// The WebView's probes, as `JsEngine` describes them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireJsEngine {
    case_unicode: Option<WireUnicode>,
    segmenter: Option<WireUnicode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireQuotes {
    custom_quotes: bool,
    custom_quotes_data: Option<[WireText; 4]>,
    unformat_quotes: bool,
    blockquote_styling: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireCharacter {
    asset_width: Option<f64>,
    chat_id: f64,
    cha_id: WireText,
    character_image: Option<WireText>,
    user_image: Option<WireText>,
    sources_throw: bool,
}

/// A `DisplayRequest`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderDisplayRequest {
    text: WireText,
    mode: WireMode,
    character: Option<WireCharacter>,
    hide_all_images: bool,
    first_slot: Option<u64>,
    image_reservation: bool,
    thought_mode: WireThoughtMode,
    cot: WireText,
    tool_called: WireText,
    error: WireText,
    quotes: WireQuotes,
    js: WireJsEngine,
}

impl RenderDisplayRequest {
    fn request(&self, budget: &Budget) -> Result<DisplayRequest, RenderError> {
        let character = match &self.character {
            Some(character) => Some(DisplayCharacter {
                asset_width: character.asset_width,
                chat_id: character.chat_id,
                cha_id: character.cha_id.text(budget)?,
                character_image: optional(&character.character_image, budget)?,
                user_image: optional(&character.user_image, budget)?,
                sources_throw: character.sources_throw,
            }),
            None => None,
        };
        let quotes = &self.quotes;
        let data = match &quotes.custom_quotes_data {
            Some([a, b, c, d]) => Some([a.text(budget)?, b.text(budget)?, c.text(budget)?, d.text(budget)?]),
            None => None,
        };
        Ok(DisplayRequest {
            text: self.text.text(budget)?,
            mode: match self.mode {
                WireMode::Normal => DisplayMode::Normal,
                WireMode::Back => DisplayMode::Back,
                WireMode::Pretranslate => DisplayMode::Pretranslate,
                WireMode::Notrim => DisplayMode::Notrim,
            },
            character,
            hide_all_images: self.hide_all_images,
            first_slot: self.first_slot,
            image_reservation: self.image_reservation,
            thought_mode: match self.thought_mode {
                WireThoughtMode::Off => ThoughtMode::Off,
                WireThoughtMode::Recent => ThoughtMode::Recent,
                WireThoughtMode::Collapsed => ThoughtMode::Collapsed,
            },
            cot: self.cot.text(budget)?,
            tool_called: self.tool_called.text(budget)?,
            error: self.error.text(budget)?,
            quotes: QuoteSettings::from_settings(quotes.custom_quotes, data, quotes.unformat_quotes, quotes.blockquote_styling, budget)?,
            js: JsEngine {
                case_unicode: self.js.case_unicode.map(WireUnicode::version),
                segmenter: self.js.segmenter.map(WireUnicode::version),
            },
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireAssetPath {
    src_paths: Vec<WireText>,
    ext: Option<WireText>,
}

/// A stored inlay as `getInlayRenderSources` describes it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireInlay {
    id: WireText,
    #[serde(rename = "type")]
    kind: String,
    mime: WireText,
    width: Option<f64>,
    height: Option<f64>,
}

/// A `DisplayReply`, tagged with the kind of the step it answers.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum RenderDisplayReply {
    AssetLookups { found: Vec<Option<WireAssetPath>> },
    AssetUrls { urls: Vec<WireText> },
    Scripts { text: WireText },
    Inlays { inlays: Vec<WireInlay> },
    Math { values: Vec<Option<WireText>> },
    Highlights { values: Vec<Option<WireText>> },
}

impl RenderDisplayReply {
    fn reply(&self, budget: &Budget) -> Result<DisplayReply, RenderError> {
        Ok(match self {
            RenderDisplayReply::AssetLookups { found } => DisplayReply::AssetLookups(
                found.iter().map(|found| found.as_ref().map(|found| -> Result<AssetPath, RenderError> {
                    Ok(AssetPath { src_paths: texts(&found.src_paths, budget)?, ext: optional(&found.ext, budget)? })
                }).transpose()).collect::<Result<_, _>>()?,
            ),
            RenderDisplayReply::AssetUrls { urls } => DisplayReply::AssetUrls(texts(urls, budget)?),
            RenderDisplayReply::Scripts { text } => DisplayReply::Scripts(text.text(budget)?),
            RenderDisplayReply::Inlays { inlays } => DisplayReply::Inlays(inlays.iter().map(|inlay| -> Result<InlayEntry, RenderError> {
                let kind = match inlay.kind.as_str() {
                    "image" => InlayKind::Image,
                    "video" => InlayKind::Video,
                    "audio" => InlayKind::Audio,
                    _ => InlayKind::Signature,
                };
                Ok(InlayEntry {
                    id: inlay.id.text(budget)?,
                    source: InlaySource { kind, mime: inlay.mime.text(budget)?, width: inlay.width, height: inlay.height },
                })
            }).collect::<Result<_, _>>()?),
            RenderDisplayReply::Math { values } => DisplayReply::Math(values.iter().map(|value| optional(value, budget)).collect::<Result<_, _>>()?),
            RenderDisplayReply::Highlights { values } => {
                DisplayReply::Highlights(values.iter().map(|value| optional(value, budget)).collect::<Result<_, _>>()?)
            }
        })
    }
}

#[derive(Debug, Serialize, PartialEq)]
pub struct WireLookup {
    tag: &'static str,
    name: WireText,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct WireUrlRequest {
    path: WireText,
    image: bool,
}

/// A highlight.js call: one per placeholder of a language the app loads.
#[derive(Debug, Serialize, PartialEq)]
pub struct WireHighlight {
    language: &'static str,
    code: WireText,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct WireRegistration {
    slot: u64,
    id: WireText,
}

/// A `DisplayStep`, with the values the host reads.
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum WireStep {
    AssetLookups { pass: usize, lookups: Vec<WireLookup> },
    AssetUrls { pass: usize, requests: Vec<WireUrlRequest> },
    Scripts { text: WireText },
    Inlays { ids: Vec<WireText> },
    Math { tex: Vec<WireText> },
    Highlights { blocks: Vec<WireHighlight> },
    Done { html: WireText, registrations: Vec<WireRegistration>, trim: bool, style_history: bool },
}

fn tag_name(tag: AssetTag) -> &'static str {
    match tag {
        AssetTag::Raw => "raw",
        AssetTag::Path => "path",
        AssetTag::Img => "img",
        AssetTag::Image => "image",
        AssetTag::Video => "video",
        AssetTag::Audio => "audio",
        AssetTag::Bgm => "bgm",
        AssetTag::Bg => "bg",
        AssetTag::Emotion => "emotion",
        AssetTag::Asset => "asset",
        AssetTag::VideoImg => "video-img",
        AssetTag::Source => "source",
    }
}

fn wire_step(step: &DisplayStep) -> WireStep {
    let list = |values: &[Text]| values.iter().map(WireText::of).collect();
    match step {
        DisplayStep::AssetLookups { pass, lookups } => WireStep::AssetLookups {
            pass: *pass,
            lookups: lookups.iter().map(|lookup| WireLookup { tag: tag_name(lookup.tag), name: WireText::of(&lookup.name) }).collect(),
        },
        DisplayStep::AssetUrls { pass, requests } => WireStep::AssetUrls {
            pass: *pass,
            requests: requests.iter().map(|request| WireUrlRequest { path: WireText::of(&request.path), image: request.image }).collect(),
        },
        DisplayStep::Scripts(text) => WireStep::Scripts { text: WireText::of(text) },
        DisplayStep::Inlays(ids) => WireStep::Inlays { ids: list(ids) },
        DisplayStep::Math(tex) => WireStep::Math { tex: list(tex) },
        DisplayStep::Highlights(found) => WireStep::Highlights {
            blocks: found.iter().filter_map(|highlight| match highlight.kind {
                HighlightKind::Language { language, .. } => Some(WireHighlight { language, code: WireText::of(&highlight.code) }),
                _ => None,
            }).collect(),
        },
        DisplayStep::Done(output) => WireStep::Done {
            html: WireText::of(&output.html),
            registrations: output.registrations.iter().map(|entry| WireRegistration { slot: entry.slot, id: WireText::of(&entry.id) }).collect(),
            trim: output.trim,
            style_history: output.style_history,
        },
    }
}

/// What a command answers: the next step with the session that waits for its answer (`None` once done), or why the
/// render stopped: a decline by name, which the page renders in TS, or `budget` for a limit or cancellation.
#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum RenderDisplayResponse {
    Step { session: Option<u32>, step: WireStep },
    Declined { declined: &'static str },
    Failed { failed: &'static str },
}

fn stopped(error: RenderError) -> RenderDisplayResponse {
    match error {
        RenderError::Unsupported(name) => RenderDisplayResponse::Declined { declined: name },
        RenderError::Budget(_) => RenderDisplayResponse::Failed { failed: "budget" },
    }
}

/// A render that does not fit beside the others: the page renders the message in TS.
const NO_ROOM: RenderDisplayResponse = RenderDisplayResponse::Declined { declined: "display-memory" };

struct Entry {
    session: DisplaySession,
    budget: Budget,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Sessions {
    next_id: u32,
    waiting: BTreeMap<u32, Entry>,
    /// The cancel flags of the renders that run now, by session.
    running: BTreeMap<u32, Arc<AtomicBool>>,
}

impl Sessions {
    /// Keeps a session that waits for an answer, dropping the oldest past the cap.
    fn keep(&mut self, id: u32, entry: Entry) {
        self.waiting.insert(id, entry);
        while self.waiting.len() > MAX_SESSIONS {
            let Some((_, oldest)) = self.waiting.pop_first() else { break };
            oldest.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Whether one more render fits in the total beside what the waiting sessions hold and the renders that run.
    fn admits(&self, caps: Caps) -> bool {
        let held = self.waiting.values().fold(0usize, |held, entry| held.saturating_add(entry.budget.owned_bytes()));
        held.saturating_add(caps.session.saturating_mul(self.running.len() + 1)) <= caps.total
    }
}

#[derive(Debug, Clone, Copy)]
struct Caps {
    /// Each render's `max_owned_bytes`.
    session: usize,
    total: usize,
}

struct Shared {
    caps: Caps,
    sessions: Mutex<Sessions>,
}

/// The open display render sessions of the main page.
#[derive(Clone)]
pub struct RenderDisplaySessions(Arc<Shared>);

impl Default for RenderDisplaySessions {
    fn default() -> Self {
        Self::with_caps(SESSION_BYTES, TOTAL_BYTES)
    }
}

/// A render among those that run. It leaves `running` however it ends, a panic included.
struct Run {
    sessions: RenderDisplaySessions,
    id: u32,
    cancel: Arc<AtomicBool>,
    left: bool,
}

impl Drop for Run {
    fn drop(&mut self) {
        if !self.left {
            self.sessions.lock().running.remove(&self.id);
        }
    }
}

impl RenderDisplaySessions {
    fn with_caps(session: usize, total: usize) -> Self {
        Self(Arc::new(Shared { caps: Caps { session, total }, sessions: Mutex::default() }))
    }

    fn lock(&self) -> MutexGuard<'_, Sessions> {
        self.0.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends every session, for a page that has started loading again. A render that runs stops at its next check
    /// and counts against the total until then.
    pub fn reset_renderer_session(&self) {
        let mut sessions = self.lock();
        for entry in std::mem::take(&mut sessions.waiting).into_values() {
            entry.cancel.store(true, Ordering::Relaxed);
        }
        for cancel in sessions.running.values() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Enters a new render in `running`, so a cancel or a reload stops it from here on.
    fn begin(&self) -> Result<(Run, Budget), RenderDisplayResponse> {
        let cancel = Arc::new(AtomicBool::new(false));
        let Ok(budget) = Budget::new(limits(self.0.caps.session), cancel.clone()) else {
            return Err(RenderDisplayResponse::Failed { failed: "budget" });
        };
        let mut sessions = self.lock();
        if !sessions.admits(self.0.caps) {
            return Err(NO_ROOM);
        }
        sessions.next_id = sessions.next_id.wrapping_add(1);
        let id = sessions.next_id;
        sessions.running.insert(id, cancel.clone());
        Ok((Run { sessions: self.clone(), id, cancel, left: false }, budget))
    }

    fn start(&self, request: RenderDisplayRequest) -> RenderDisplayResponse {
        match self.begin() {
            Ok((run, budget)) => self.render_start(run, budget, request),
            Err(response) => response,
        }
    }

    fn render_start(&self, run: Run, budget: Budget, request: RenderDisplayRequest) -> RenderDisplayResponse {
        let started = request.request(&budget).and_then(|request| DisplaySession::start(request, &budget));
        drop(request);
        match started {
            Ok((session, step)) => {
                let cancel = run.cancel.clone();
                self.finish(run, Some(Entry { session, budget, cancel }), Ok(step))
            }
            Err(error) => self.finish(run, None, Err(error)),
        }
    }

    fn resume(&self, id: u32, reply: RenderDisplayReply) -> RenderDisplayResponse {
        let (run, mut entry) = match self.take(id) {
            Ok(taken) => taken,
            Err(response) => return response,
        };
        let result = reply.reply(&entry.budget).and_then(|reply| entry.session.resume(reply, &entry.budget));
        drop(reply);
        self.finish(run, Some(entry), result)
    }

    /// Takes a waiting session out of the map while a resume runs it. A session that does not fit beside the
    /// renders that run ends.
    fn take(&self, id: u32) -> Result<(Run, Entry), RenderDisplayResponse> {
        let mut sessions = self.lock();
        let Some(entry) = sessions.waiting.remove(&id) else {
            return Err(RenderDisplayResponse::Declined { declined: "display-session" });
        };
        if !sessions.admits(self.0.caps) {
            entry.cancel.store(true, Ordering::Relaxed);
            return Err(NO_ROOM);
        }
        sessions.running.insert(id, entry.cancel.clone());
        let cancel = entry.cancel.clone();
        Ok((Run { sessions: self.clone(), id, cancel, left: false }, entry))
    }

    /// Ends a render, keeping its session while it waits for another answer, unless a cancel or a reload came first.
    fn finish(&self, mut run: Run, entry: Option<Entry>, result: Result<DisplayStep, RenderError>) -> RenderDisplayResponse {
        // A reply of another kind leaves the request open, as the session does.
        let open = match &result {
            Ok(DisplayStep::Done(_)) => false,
            Ok(_) | Err(RenderError::Unsupported("display-reply")) => true,
            Err(_) => false,
        };
        let response = match result {
            Ok(step) => RenderDisplayResponse::Step { session: open.then_some(run.id), step: wire_step(&step) },
            Err(error) => stopped(error),
        };
        let mut sessions = self.lock();
        sessions.running.remove(&run.id);
        run.left = true;
        if run.cancel.load(Ordering::Relaxed) {
            return match response {
                RenderDisplayResponse::Step { .. } => RenderDisplayResponse::Failed { failed: "budget" },
                other => other,
            };
        }
        if let (true, Some(entry)) = (open, entry) {
            sessions.keep(run.id, entry);
        }
        response
    }

    fn cancel(&self, id: u32) {
        let mut sessions = self.lock();
        if let Some(entry) = sessions.waiting.remove(&id) {
            entry.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = sessions.running.get(&id) {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Runs `work` on its own thread with room for the Markdown rules' recursion.
async fn on_render_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("render-display".into())
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let _ = sender.send(work());
        })
        .map_err(|_| "render_display_worker_failed".to_string())?;
    receiver.await.map_err(|_| "render_display_worker_failed".to_string())
}

#[tauri::command]
pub async fn render_display_start(state: tauri::State<'_, RenderDisplaySessions>, request: RenderDisplayRequest)
    -> Result<RenderDisplayResponse, String> {
    let sessions = state.inner().clone();
    crate::native_log::logged_without_detail("render_display_start", on_render_thread(move || sessions.start(request)).await)
}

#[tauri::command]
pub async fn render_display_resume(state: tauri::State<'_, RenderDisplaySessions>, session: u32, reply: RenderDisplayReply)
    -> Result<RenderDisplayResponse, String> {
    let sessions = state.inner().clone();
    crate::native_log::logged_without_detail("render_display_resume", on_render_thread(move || sessions.resume(session, reply)).await)
}

#[tauri::command]
pub async fn render_display_cancel(state: tauri::State<'_, RenderDisplaySessions>, session: u32) -> Result<(), String> {
    state.cancel(session);
    Ok(())
}

#[cfg(test)]
#[path = "render_display_tests.rs"]
mod tests;
