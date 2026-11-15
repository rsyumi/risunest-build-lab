//! The last TS step of the native content import, run by `risunest-formats` when the prepared receipt is read: the card
//! mapping `mapPreparedNativeCharacterCard`, the module `activatePreparedNativeModuleContent` appends for a RISUM module,
//! and for a card imported as a module, the character mapping and the module converted from it. The receipt carries the
//! character or module TS would store, as the exact text `JSON.stringify` gives for it, or the reason the port declined;
//! TS maps the content itself only where the port does not answer. A `-0` therefore arrives as `0` where the TS mapping
//! keeps `-0` in memory; the stored record is the same.
//!
//! A PNG card is still decoded in TS, which also stages its inline payloads and asks for an RCC password. The port
//! decodes plain base64 metadata the same way to map it; an RCC envelope declines, and a card with an inline payload is
//! one the mapping finds invalid, so TS maps both.

use super::{PreparedContent, PreparedContentFormat};
use base64::alphabet::STANDARD as STANDARD_ALPHABET;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine as _;
use risunest_formats::card_import::{map_prepared_card, PreparedAsset, PreparedCard};
use risunest_formats::ids::{Ids, RandomIds};
use risunest_formats::module_import::{map_prepared_risum, module_from_mapped_card};
use risunest_ir::json::{parse_json, stringify};
use risunest_ir::rng::OsEntropy;
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Limits, Text, Value};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::sync::{atomic::AtomicBool, Arc};

// One card's metadata is at most 128 MiB; a card whose mapping needs more falls back to TS.
pub(super) const MAPPING_LIMITS: Limits = Limits { max_owned_bytes: 1 << 28, max_json_bytes: 1 << 27, max_text_units: 1 << 27,
    max_values: 1 << 24, max_depth: 128, max_work: usize::MAX / 4 };

// The core crate's deep copies assume a stage thread's stack at `max_depth`; blocking pool threads have less.
pub(super) const MAPPING_STACK_BYTES: usize = 8 << 20;

// A larger card or RISUM module is mapped in TS alone, and a larger exported module keeps the RISUM job's own metadata
// layout. The release build's mapping time and peak memory grow linearly with the card, to about 200 ms and 185 MiB at
// 24 MiB; by 32 MiB `MAPPING_LIMITS` declines a text-heavy exported module after doing most of the work.
pub(super) const MAX_MAPPED_CARD_BYTES: usize = 24 << 20;

/// What the content was picked for, which the content import request names: a card imported as a module is also
/// converted. A RISUM module is a module either way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ContentDestination {
    #[default]
    Character,
    Module,
}

/// What `native_file_job_prepared_content` returns: the prepared content and the port's mapping of it, and for a card
/// imported as a module, the module converted from that mapping.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreparedContentReceipt {
    #[serde(flatten)]
    pub(crate) content: PreparedContent,
    pub(crate) native_mapping: NativeMapping,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) native_module_mapping: Option<NativeMapping>,
}

/// `{"outcome": "mapped", "character": …}` for a card, `{"outcome": "mapped", "module": …}` for a RISUM module or a
/// card's converted module.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub(crate) enum NativeMapping {
    Mapped(Mapped),
    Declined { reason: DeclineReason, name: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Mapped {
    Character(Box<RawValue>),
    Module(Box<RawValue>),
}

/// The `StageError` kind, or `Failed` when the input or the output could not cross to or from the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DeclineReason {
    Unsupported,
    Invalid,
    Limit,
    Cancelled,
    Host,
    Failed,
}

pub(crate) fn receipt(content: PreparedContent, destination: ContentDestination) -> PreparedContentReceipt {
    let (native_mapping, native_module_mapping) = map_content(&content, destination, &mut RandomIds(OsEntropy));
    PreparedContentReceipt { content, native_mapping, native_module_mapping }
}

/// The port's mapping of a prepared JSON, PNG or CharX card, or of a RISUM module, and for a card imported as a module,
/// the module converted from the mapped card.
pub(crate) fn map_content(
    content: &PreparedContent,
    destination: ContentDestination,
    ids: &mut (dyn Ids + Send),
) -> (NativeMapping, Option<NativeMapping>) {
    map_content_within(content, destination, ids, MAX_MAPPED_CARD_BYTES)
}

/// [`map_content`] with a content size above which the port declines before it reads the content.
fn map_content_within(
    content: &PreparedContent,
    destination: ContentDestination,
    ids: &mut (dyn Ids + Send),
    max_card_bytes: usize,
) -> (NativeMapping, Option<NativeMapping>) {
    let risum = content.format == PreparedContentFormat::RisuModule;
    let converted = !risum && destination == ContentDestination::Module;
    let both = |mapping: NativeMapping| (mapping.clone(), converted.then_some(mapping));
    if card_bytes(content, max_card_bytes).is_none() {
        return both(declined(DeclineReason::Limit, if risum { "moduleBytes" } else { "cardBytes" }));
    }
    let outcome = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("card-mapping".into())
            .stack_size(MAPPING_STACK_BYTES)
            .spawn_scoped(scope, || match (risum, converted) {
                (true, _) => (map_module(content, ids), None),
                (false, true) => map_card_module(content, ids),
                (false, false) => (map_card(content, ids), None),
            })
            .map(|thread| thread.join())
    });
    match outcome {
        Ok(Ok(mappings)) => mappings,
        Ok(Err(_)) => both(declined(DeclineReason::Failed, "panic")),
        Err(_) => both(declined(DeclineReason::Failed, "thread")),
    }
}

fn map_card(content: &PreparedContent, ids: &mut dyn Ids) -> NativeMapping {
    mapping(mapped_text(content, ids), Mapped::Character, "characterJson")
}

fn map_module(content: &PreparedContent, ids: &mut dyn Ids) -> NativeMapping {
    mapping(mapped_module_text(content, ids), Mapped::Module, "moduleJson")
}

// The character, as for a card imported as one, and the module `activatePreparedNativeModuleContent` converts from it.
// A card the mapping declines declines both; a conversion that declines leaves the character mapped.
fn map_card_module(content: &PreparedContent, ids: &mut dyn Ids) -> (NativeMapping, Option<NativeMapping>) {
    let declined_both = |refusal: Refusal| {
        let declined = refused(refusal);
        (declined.clone(), Some(declined))
    };
    let budget = match Budget::new(MAPPING_LIMITS, Arc::new(AtomicBool::new(false))) {
        Ok(budget) => budget,
        Err(error) => return declined_both(error.into()),
    };
    let character = match mapped_character(content, ids, &budget) {
        Ok(character) => character,
        Err(refusal) => return declined_both(refusal),
    };
    let module = module_from_mapped_card(&character, ids, &budget)
        .map_err(Refusal::from)
        .and_then(|module| json_text(&module, &budget, "moduleText"));
    (
        mapping(json_text(&character, &budget, "characterText"), Mapped::Character, "characterJson"),
        Some(mapping(module, Mapped::Module, "moduleJson")),
    )
}

fn mapping(text: Result<String, Refusal>, mapped: fn(Box<RawValue>) -> Mapped, failed: &str) -> NativeMapping {
    match text.map(RawValue::from_string) {
        Ok(Ok(value)) => NativeMapping::Mapped(mapped(value)),
        Ok(Err(_)) => declined(DeclineReason::Failed, failed),
        Err(refusal) => refused(refusal),
    }
}

fn refused(refusal: Refusal) -> NativeMapping {
    match refusal {
        Refusal::Stage(error) => declined_by(error),
        Refusal::Failed(name) => declined(DeclineReason::Failed, name),
    }
}

enum Refusal {
    Stage(StageError),
    Failed(&'static str),
}

impl From<StageError> for Refusal {
    fn from(error: StageError) -> Self { Self::Stage(error) }
}

impl From<risunest_ir::Error> for Refusal {
    fn from(error: risunest_ir::Error) -> Self { Self::Stage(error.into()) }
}

fn mapped_text(content: &PreparedContent, ids: &mut dyn Ids) -> Result<String, Refusal> {
    let budget = Budget::new(MAPPING_LIMITS, Arc::new(AtomicBool::new(false)))?;
    let character = mapped_character(content, ids, &budget)?;
    json_text(&character, &budget, "characterText")
}

// The input TS's `mapPreparedNativeContent` builds from the validated receipt: the card, each asset's token and
// logical ID, the portrait and the CharX module. The receipt crosses IPC as JSON text, so the card and the module are
// what `JSON.parse` gives TS for the text they are written as; a PNG card is what TS decodes from its metadata.
fn mapped_character(content: &PreparedContent, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, Refusal> {
    let card = match content.format {
        PreparedContentFormat::PngCard => png_card(&content.metadata, budget)?,
        _ => parse_json(&serde_json::to_string(&content.metadata).map_err(|_| Refusal::Failed("cardJson"))?, budget)?,
    };
    let module = match &content.module {
        Some(module) => Some(parse_json(&serde_json::to_string(module).map_err(|_| Refusal::Failed("moduleJson"))?, budget)?),
        None => None,
    };
    let pairs = content
        .assets
        .iter()
        .map(|asset| Ok((Text::from_str(&asset.token, budget)?, Text::from_str(&asset.logical_id, budget)?)))
        .collect::<Result<Vec<_>, risunest_ir::Error>>()?;
    let assets: Vec<PreparedAsset<'_>> =
        pairs.iter().map(|(token, logical_id)| PreparedAsset { token, logical_id }).collect();
    let portrait = content.portrait_logical_id.as_deref().map(|id| Text::from_str(id, budget)).transpose()?;
    let input = PreparedCard { card: &card, assets: &assets, portrait_logical_id: portrait.as_ref(), module: module.as_ref() };
    Ok(map_prepared_card(&input, ids, budget)?)
}

// The input TS's RISUM activation reads from the validated receipt: the module metadata as `JSON.parse` gives it for
// the text the receipt writes, and each asset's logical ID by its position, which the receipt validator holds equal to
// its index.
fn mapped_module_text(content: &PreparedContent, ids: &mut dyn Ids) -> Result<String, Refusal> {
    let budget = Budget::new(MAPPING_LIMITS, Arc::new(AtomicBool::new(false)))?;
    let metadata = parse_json(&serde_json::to_string(&content.metadata).map_err(|_| Refusal::Failed("moduleJson"))?, &budget)?;
    let logical_ids =
        content.assets.iter().map(|asset| Text::from_str(&asset.logical_id, &budget)).collect::<Result<Vec<_>, _>>()?;
    let logical_ids: Vec<&Text> = logical_ids.iter().collect();
    let module = map_prepared_risum(&metadata, &logical_ids, ids, &budget)?;
    json_text(&module, &budget, "moduleText")
}

fn json_text(value: &Value, budget: &Budget, failed: &'static str) -> Result<String, Refusal> {
    let text = stringify(value, budget)?.ok_or(Refusal::Failed(failed))?;
    match text.as_unicode() {
        Some(text) => Ok(text.to_owned()),
        None => String::from_utf16(&text.units().collect::<Vec<_>>()).map_err(|_| Refusal::Failed(failed)),
    }
}

// `Buffer.from(value, 'base64')` over text `isStandardBase64` accepts, which ignores the bits after the last byte.
const PNG_METADATA_BASE64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD_ALPHABET,
    GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
);

const UTF8_BOM: &[u8] = &[0xef, 0xbb, 0xbf];

/// `decodePreparedNativePngCardMetadata` (`src/ts/storage/nativePngCardAdapter.ts`) up to the inline payload staging,
/// for metadata that is no RCC envelope: `ccv3 ?? chara`, standard base64 of UTF-8 JSON (a leading BOM dropped as
/// `TextDecoder` drops it), and a v2 card's numeric `character_version` as its JS string.
fn png_card(metadata: &serde_json::Value, budget: &Budget) -> Result<Value, Refusal> {
    let decline = |name: &'static str| Refusal::Stage(StageError::Invalid(Cow::Borrowed(name)));
    let selected = selected_png_metadata(metadata).ok_or_else(|| decline("pngMetadata"))?;
    if selected.starts_with("rcc||") {
        return Err(Refusal::Stage(StageError::Unsupported(Cow::Borrowed("rcc"))));
    }
    if !is_standard_base64(selected) {
        return Err(decline("pngBase64"));
    }
    let bytes = PNG_METADATA_BASE64.decode(selected).map_err(|_| decline("pngBase64"))?;
    let text = std::str::from_utf8(bytes.strip_prefix(UTF8_BOM).unwrap_or(&bytes)).map_err(|_| decline("pngUtf8"))?;
    let mut card = parse_json(text, budget)?;
    drop(bytes);
    normalize_v2_character_version(&mut card, budget)?;
    Ok(card)
}

/// `metadata.ccv3 ?? metadata.chara`, the text chunk TS decodes, when it is a string.
fn selected_png_metadata(metadata: &serde_json::Value) -> Option<&str> {
    let present = |key: &str| metadata.get(key).filter(|value| !value.is_null());
    present("ccv3").or_else(|| present("chara"))?.as_str()
}

/// `isStandardBase64`: nonempty, a multiple of four, the standard alphabet, and `=` only to pad a partial last group.
fn is_standard_base64(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return false;
    }
    let padding = if bytes.ends_with(b"==") { 2 } else { usize::from(bytes.ends_with(b"=")) };
    let content = bytes.len() - padding;
    bytes[..content].iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
        && match padding { 0 => true, 1 => content % 4 == 3, _ => content % 4 == 2 }
}

/// `normalizeV2CharacterVersion`: a v2 card's numeric `data.character_version` becomes `characterVersion.toString()`.
fn normalize_v2_character_version(card: &mut Value, budget: &Budget) -> Result<(), risunest_ir::Error> {
    if !matches!(card.get(b"spec", budget)?, Some(Value::Text(spec)) if spec.equals_ascii(b"chara_card_v2")) {
        return Ok(());
    }
    let Value::Object(entries) = card else { return Ok(()) };
    let Some((_, Value::Object(data))) = entries.as_mut_slice().iter_mut().find(|(key, _)| key.equals_ascii(b"data")) else {
        return Ok(());
    };
    if let Some((_, version)) = data.as_mut_slice().iter_mut().find(|(key, _)| key.equals_ascii(b"character_version")) {
        if matches!(version, Value::Number(_)) {
            *version = Value::Text(version.to_text(budget)?);
        }
    }
    Ok(())
}

/// The content's size as JSON text, or `None` above `max_bytes`, where counting stops: the receipt's card or module
/// metadata and CharX module as the IPC response writes them, or the decoded length of a PNG card's selected metadata.
fn card_bytes(content: &PreparedContent, max_bytes: usize) -> Option<usize> {
    if content.format == PreparedContentFormat::PngCard {
        let size = selected_png_metadata(&content.metadata).map_or(0, |text| text.len() / 4 * 3);
        return (size <= max_bytes).then_some(size);
    }
    struct Count {
        written: usize,
        max_bytes: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.written = self.written.saturating_add(bytes.len());
            if self.written > self.max_bytes {
                return Err(std::io::Error::other("content exceeds the mapped size"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut count = Count { written: 0, max_bytes };
    serde_json::to_writer(&mut count, &content.metadata).ok()?;
    if let Some(module) = &content.module {
        serde_json::to_writer(&mut count, module).ok()?;
    }
    Some(count.written)
}

fn declined(reason: DeclineReason, name: &str) -> NativeMapping {
    NativeMapping::Declined { reason, name: name.to_owned() }
}

fn declined_by(error: StageError) -> NativeMapping {
    match error {
        StageError::Unsupported(name) => declined(DeclineReason::Unsupported, &name),
        StageError::Invalid(name) => declined(DeclineReason::Invalid, &name),
        StageError::Limit(name) => declined(DeclineReason::Limit, &name),
        StageError::Cancelled => declined(DeclineReason::Cancelled, ""),
        StageError::Host(message) => declined(DeclineReason::Host, &String::from_utf16_lossy(&message)),
        _ => declined(DeclineReason::Failed, "stage"),
    }
}

#[cfg(test)]
#[path = "card_mapping_tests.rs"]
mod tests;
