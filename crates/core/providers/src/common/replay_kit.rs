//! Navigation of the committed oracle fixtures for the replay tests of every family.

use risunest_ir::{json, Budget, OwnedVec, Text, Value};
use risunest_ir::chat::{Multimodal, MultimodalData, MultimodalKind, PromptMessage, Role};
use risunest_ir::provider::{LlmFlag, LlmFormat, ModelDescriptor};
use sha2::{Digest, Sha256};
use super::settings::{ParameterValues, SamplingSettings};

#[derive(Clone, Debug)]
pub(crate) enum Jv { Null, Bool(bool), Num(f64), Str(Vec<u16>), Arr(Vec<Jv>), Obj(Vec<(String, Jv)>) }

pub(crate) fn lossy(units: &[u16]) -> String { String::from_utf16_lossy(units) }

pub(crate) fn from_value(value: &Value) -> Jv {
    match value {
        Value::Undefined | Value::Null => Jv::Null,
        Value::Bool(flag) => Jv::Bool(*flag),
        Value::Number(number) => Jv::Num(*number),
        Value::Text(text) => Jv::Str(text.units().collect()),
        Value::Array(items) => Jv::Arr(items.as_slice().iter().map(from_value).collect()),
        Value::Object(entries) => Jv::Obj(entries.as_slice().iter().map(|(key, item)| (lossy(&key.units().collect::<Vec<_>>()), from_value(item))).collect()),
    }
}

pub(crate) fn parse(bytes: &[u8], b: &Budget) -> Jv {
    assert!(!bytes.contains(&13), "fixtures use LF");
    from_value(&json::parse_json(std::str::from_utf8(bytes).expect("utf-8"), b).expect("fixture json"))
}

pub(crate) fn to_value(value: &Jv, b: &Budget) -> Value {
    match value {
        Jv::Null => Value::Null,
        Jv::Bool(flag) => Value::Bool(*flag),
        Jv::Num(number) => Value::Number(*number),
        Jv::Str(units) => Value::Text(Text::from_units(units, b).unwrap()),
        Jv::Arr(items) => {
            let mut array = OwnedVec::new(b).unwrap();
            for item in items { array.push(to_value(item, b)).unwrap(); }
            Value::Array(array)
        }
        Jv::Obj(entries) => {
            let mut object = OwnedVec::new(b).unwrap();
            for (key, item) in entries { object.push((Text::from_str(key, b).unwrap(), to_value(item, b))).unwrap(); }
            Value::Object(object)
        }
    }
}

impl Jv {
    pub(crate) fn get(&self, key: &str) -> Option<&Jv> {
        match self { Jv::Obj(entries) => entries.iter().find(|(name, _)| name == key).map(|(_, value)| value), _ => None }
    }
    pub(crate) fn has(&self, key: &str) -> bool { self.get(key).is_some_and(|value| !matches!(value, Jv::Null)) }
    pub(crate) fn need(&self, key: &str) -> &Jv { self.get(key).unwrap_or_else(|| panic!("missing `{key}`")) }
    pub(crate) fn string(&self) -> String { match self { Jv::Str(units) => lossy(units), other => panic!("not a string: {other:?}") } }
    pub(crate) fn num(&self) -> f64 { match self { Jv::Num(number) => *number, other => panic!("not a number: {other:?}") } }
    pub(crate) fn flag(&self) -> bool { match self { Jv::Bool(flag) => *flag, other => panic!("not a bool: {other:?}") } }
    pub(crate) fn list(&self) -> &[Jv] { match self { Jv::Arr(items) => items, other => panic!("not an array: {other:?}") } }
    pub(crate) fn entries(&self) -> &[(String, Jv)] { match self { Jv::Obj(entries) => entries, other => panic!("not an object: {other:?}") } }
    pub(crate) fn text(&self, b: &Budget) -> Text { match self { Jv::Str(units) => Text::from_units(units, b).unwrap(), other => panic!("not a string: {other:?}") } }
}

pub(crate) fn t(value: &str, b: &Budget) -> Text { Text::from_str(value, b).unwrap() }

pub(crate) fn same_hash(cases: &[u8], expected: &Jv) {
    let digest = Sha256::digest(cases);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(hex, expected.need("fixtureSha256").string(), "the expectations were generated from other fixtures");
}

pub(crate) struct Db<'a> { pub(crate) base: &'a Jv, pub(crate) over: Option<&'a Jv> }

impl<'a> Db<'a> {
    pub(crate) fn new(fixture: &'a Jv, entry: &'a Jv) -> Self { Self { base: fixture.need("baseDb"), over: entry.get("db") } }
    pub(crate) fn get(&self, name: &str) -> Option<&'a Jv> {
        if let Some(over) = self.over { if let Some(value) = over.get(name) { return Some(value); } }
        self.base.get(name)
    }
    pub(crate) fn need(&self, name: &str) -> &'a Jv { self.get(name).unwrap_or_else(|| panic!("db.{name} missing from the fixture")) }
    pub(crate) fn text(&self, name: &str, b: &Budget) -> Text { self.need(name).text(b) }
    pub(crate) fn flag(&self, name: &str) -> bool { self.need(name).flag() }
    pub(crate) fn num(&self, name: &str) -> f64 { self.need(name).num() }
}

pub(crate) fn model_descriptor(model: &Jv, b: &Budget) -> ModelDescriptor {
    let flag = |name: &str| match name {
        "hasImageInput" => LlmFlag::HasImageInput, "hasImageOutput" => LlmFlag::HasImageOutput, "hasAudioInput" => LlmFlag::HasAudioInput,
        "hasAudioOutput" => LlmFlag::HasAudioOutput, "hasPrefill" => LlmFlag::HasPrefill, "hasCache" => LlmFlag::HasCache,
        "hasFullSystemPrompt" => LlmFlag::HasFullSystemPrompt, "hasFirstSystemPrompt" => LlmFlag::HasFirstSystemPrompt,
        "hasStreaming" => LlmFlag::HasStreaming, "requiresAlternateRole" => LlmFlag::RequiresAlternateRole,
        "mustStartWithUserInput" => LlmFlag::MustStartWithUserInput, "poolSupported" => LlmFlag::PoolSupported,
        "hasVideoInput" => LlmFlag::HasVideoInput, "OAICompletionTokens" => LlmFlag::OAICompletionTokens,
        "DeveloperRole" => LlmFlag::DeveloperRole, "geminiThinking" => LlmFlag::GeminiThinking, "geminiBlockOff" => LlmFlag::GeminiBlockOff,
        "deepSeekPrefix" => LlmFlag::DeepSeekPrefix, "deepSeekThinkingInput" => LlmFlag::DeepSeekThinkingInput,
        "deepSeekThinkingOutput" => LlmFlag::DeepSeekThinkingOutput, "noCivilIntegrity" => LlmFlag::NoCivilIntegrity,
        "claudeThinking" => LlmFlag::ClaudeThinking, "claudeAdaptiveThinking" => LlmFlag::ClaudeAdaptiveThinking,
        "claudeXHighEffort" => LlmFlag::ClaudeXHighEffort, "deepSeekThinkingToggle" => LlmFlag::DeepSeekThinkingToggle,
        "noStructuredOutput" => LlmFlag::NoStructuredOutput, "geminiThinkingNoMinimal" => LlmFlag::GeminiThinkingNoMinimal,
        "claudeThinkingOnByDefault" => LlmFlag::ClaudeThinkingOnByDefault,
        other => panic!("flag `{other}` is not mapped"),
    };
    const FORMATS: [LlmFormat; 24] = [
        LlmFormat::OpenAICompatible, LlmFormat::OpenAILegacyInstruct, LlmFormat::Anthropic, LlmFormat::AnthropicLegacy, LlmFormat::Mistral,
        LlmFormat::GoogleCloud, LlmFormat::VertexAIGemini, LlmFormat::NovelList, LlmFormat::Cohere, LlmFormat::NovelAI, LlmFormat::WebLLM,
        LlmFormat::OobaLegacy, LlmFormat::Plugin, LlmFormat::Ooba, LlmFormat::Kobold, LlmFormat::Ollama, LlmFormat::Horde,
        LlmFormat::AWSBedrockClaude, LlmFormat::OpenAIResponseAPI, LlmFormat::Echo, LlmFormat::NanoGPT, LlmFormat::NanoGPTResponses,
        LlmFormat::NanoGPTMessages, LlmFormat::NanoGPTLegacy,
    ];
    let format = FORMATS[model.need("format").num() as usize];
    ModelDescriptor {
        id: model.need("id").text(b),
        internal_id: model.get("internalID").filter(|_| model.has("internalID")).map(|value| value.text(b)),
        format,
        flags: model.need("flags").list().iter().map(|name| flag(&name.string())).collect(),
        parameters: model.need("parameters").list().iter().map(|name| name.text(b)).collect(),
        endpoint: model.get("endpoint").filter(|_| model.has("endpoint")).map(|value| value.text(b)),
    }
}

pub(crate) fn message(entry: &Jv, b: &Budget) -> PromptMessage {
    let role = match entry.need("role").string().as_str() {
        "system" => Role::System, "user" => Role::User, "assistant" => Role::Assistant, "function" => Role::Function,
        other => panic!("role {other}"),
    };
    let mut result = PromptMessage::new(role, entry.need("content").text(b));
    for (key, value) in entry.entries() {
        match key.as_str() {
            "role" | "content" => {}
            "name" => result.name = Some(value.text(b)),
            "memo" => result.memo = Some(value.text(b)),
            "removable" => result.removable = Some(value.flag()),
            "cachePoint" => result.cache_point = Some(value.flag()),
            "attr" => result.attr = Some(value.list().iter().map(|item| item.text(b)).collect()),
            "thoughts" => result.thoughts = Some(value.list().iter().map(|item| item.text(b)).collect()),
            "multimodals" => result.multimodals = Some(value.list().iter().map(|item| Multimodal {
                kind: match item.need("type").string().as_str() {
                    "image" => MultimodalKind::Image, "video" => MultimodalKind::Video, "audio" => MultimodalKind::Audio, _ => MultimodalKind::Signature,
                },
                data: MultimodalData::Inline(item.need("base64").text(b)),
                width: item.get("width").map(Jv::num),
                height: item.get("height").map(Jv::num),
                extra: Vec::new(),
            }).collect()),
            other => panic!("message field {other} is not mapped"),
        }
    }
    result
}

pub(crate) fn parameter_values(entry: &Jv, names: [&str; 11]) -> ParameterValues {
    let n = |index: usize| entry.need(names[index]).num();
    ParameterValues {
        temperature: n(0), top_k: n(1), repetition_penalty: n(2), min_p: n(3), top_a: n(4), top_p: n(5), frequency_penalty: n(6),
        presence_penalty: n(7), thinking_tokens: n(8), reasoning_effort: n(9), verbosity: n(10),
    }
}

pub(crate) fn sampling_settings(db: &Db, b: &Budget) -> SamplingSettings {
    let base = parameter_values(&Jv::Obj(vec![
        ("temperature".into(), db.need("temperature").clone()), ("top_k".into(), db.need("top_k").clone()),
        ("repetition_penalty".into(), db.need("repetition_penalty").clone()), ("min_p".into(), db.need("min_p").clone()),
        ("top_a".into(), db.need("top_a").clone()), ("top_p".into(), db.need("top_p").clone()),
        ("frequency_penalty".into(), db.need("frequencyPenalty").clone()), ("presence_penalty".into(), db.need("PresensePenalty").clone()),
        ("thinking_tokens".into(), db.need("thinkingTokens").clone()), ("reasoning_effort".into(), db.need("reasoningEffort").clone()),
        ("verbosity".into(), db.need("verbosity").clone()),
    ]), ["temperature", "top_k", "repetition_penalty", "min_p", "top_a", "top_p", "frequency_penalty", "presence_penalty", "thinking_tokens", "reasoning_effort", "verbosity"]);
    let names = ["temperature", "top_k", "repetition_penalty", "min_p", "top_a", "top_p", "frequency_penalty", "presence_penalty", "thinking_tokens", "reasoning_effort", "verbosity"];
    let enabled = db.flag("seperateParametersEnabled");
    let mut by_mode = Vec::new();
    let mut overrides = Vec::new();
    if enabled {
        let table = db.need("seperateParameters");
        for mode in ["model", "submodel", "memory", "emotion", "translate", "otherAx"] {
            by_mode.push((t(mode, b), parameter_values(table.need(mode), names)));
        }
        for (id, values) in table.need("overrides").entries() { overrides.push((t(id, b), parameter_values(values, names))); }
    }
    SamplingSettings { base, separate_enabled: enabled, separate_by_model: db.flag("seperateParametersByModel"), by_mode, overrides }
}

pub(crate) fn pair_list(value: &Jv, b: &Budget) -> Vec<(Text, Text)> {
    value.list().iter().map(|pair| (pair.list()[0].text(b), pair.list()[1].text(b))).collect()
}

/// `Date.parse` of an ISO instant such as `2026-01-02T03:04:05.987Z`, in milliseconds.
pub(crate) fn clock_ms(text: &str) -> i64 {
    let number = |range: std::ops::Range<usize>| text[range].parse::<i64>().unwrap();
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    let (hour, minute, second) = (number(11..13), number(14..16), number(17..19));
    let millis = text.get(20..).and_then(|rest| rest.strip_suffix('Z')).filter(|rest| !rest.is_empty()).map_or(0, |rest| rest.parse::<i64>().unwrap());
    // Days from civil (proleptic Gregorian).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    ((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis
}
