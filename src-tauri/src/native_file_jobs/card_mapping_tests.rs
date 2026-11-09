//! Replays `tests/fixtures/rust-core/formats/wiring.*.json` (receipts activated through the TS mapping by
//! `benchmarks/rust-core-oracles/formats-wiring.oracle.test.ts`) and the import cases of `cards.*.json` through the
//! receipt's mapping.

use super::super::{PreparedContentAsset, PreparedContentModule, PreparedContentOwnerHead};
use super::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const WIRING_CASES: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/rust-core/formats/wiring.cases.json"));
const WIRING_EXPECTED: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/rust-core/formats/wiring.expected.json"));
const CARD_CASES: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/rust-core/formats/cards.cases.json"));
const CARD_EXPECTED: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/rust-core/formats/cards.expected.json"));

/// The oracles' `uuid` mock: `00000000-0000-4000-8000-000000000001` and up, restarted for every case.
struct Counter(u64);

impl Ids for Counter {
    fn next_id(&mut self, budget: &Budget) -> Result<Text, StageError> {
        self.0 += 1;
        Ok(Text::from_str(&format!("00000000-0000-4000-8000-{:012}", self.0), budget)?)
    }
}

/// The messages `mapPreparedNativeCharacterCard` throws on purpose, by the `Invalid` field the port names.
const INVALID: &[(&str, &str)] = &[
    ("Prepared native card data URI payloads are not allowed", "dataUri"),
    ("Prepared native asset token is missing:", "missingAsset"),
    ("Prepared native ccdefault asset requires a portrait logical ID", "ccdefaultPortrait"),
    ("Duplicate prepared native asset token:", "duplicateAssetToken"),
    ("Prepared native asset token and logical ID must not be empty", "assetToken"),
    ("Prepared native portrait logical ID must not be empty", "portraitLogicalId"),
    ("Prepared native v2 payload fields require a logical asset reference", "assetReference"),
];

/// The JSON text with each escape of an isolated surrogate written as `�`. A receipt is a `serde_json` value,
/// which cannot hold an isolated surrogate, so the replays skip a case this changes.
fn without_isolated_surrogates(json: &[u8]) -> String {
    let unit = |escape: &str| escape.strip_prefix("\\u").and_then(|hex| hex.get(..4)).and_then(|hex| u16::from_str_radix(hex, 16).ok());
    let (mut text, mut rest) = (String::with_capacity(json.len()), std::str::from_utf8(json).unwrap());
    while let Some(at) = rest.find('\\') {
        text.push_str(&rest[..at]);
        let escape = &rest[at..];
        let length = match unit(escape) {
            Some(0xd800..=0xdbff) if matches!(unit(&escape[6..]), Some(0xdc00..=0xdfff)) => 12,
            Some(0xd800..=0xdfff) => {
                text.push_str("\\ufffd");
                rest = &escape[6..];
                continue;
            }
            Some(_) => 6,
            None => 2,
        };
        text.push_str(&escape[..length]);
        rest = &escape[length..];
    }
    text.push_str(rest);
    text
}

/// Each case with its recorded result, after checking that the expected file was recorded from these cases.
fn oracle(cases: &[u8], expected: &[u8]) -> Vec<(Value, Value)> {
    let digest: String = Sha256::digest(cases).iter().map(|byte| format!("{byte:02x}")).collect();
    let parse = |json: &[u8]| serde_json::from_str::<Value>(&without_isolated_surrogates(json)).unwrap();
    let (cases, expected) = (parse(cases), parse(expected));
    assert_eq!(expected["fixtureSha256"], json!(digest), "regenerate the expected file");
    let (cases, recorded) = (cases["cases"].as_array().unwrap(), expected["cases"].as_array().unwrap());
    assert_eq!(cases.len(), recorded.len());
    cases
        .iter()
        .zip(recorded)
        .map(|(case, record)| {
            assert_eq!(case["id"], record["id"]);
            (case.clone(), record["result"].clone())
        })
        .collect()
}

fn text(value: &Value) -> Option<String> { value.as_str().map(str::to_owned) }

/// A card asset, or a RISUM asset frame, which has a position and no token.
fn asset(value: &Value) -> Option<PreparedContentAsset> {
    let position = value["position"].as_u64().map(|position| position as usize);
    Some(PreparedContentAsset {
        reference_key: text(&value["referenceKey"]).unwrap_or_default(),
        token: match position {
            Some(_) => String::new(),
            None => text(&value["token"])?,
        },
        position,
        logical_id: text(&value["logicalId"])?,
        object_hash: text(&value["objectHash"]).unwrap_or_default(),
        byte_size: value["byteSize"].as_u64().unwrap_or(0),
        mime: text(&value["mime"]).unwrap_or_default(),
        name: text(&value["name"]).unwrap_or_default(),
        ext: text(&value["ext"]).unwrap_or_default(),
    })
}

/// The module the job sends, or `None` for a fixture module its arrays cannot carry.
fn module(value: &Value) -> Option<PreparedContentModule> {
    let object = value.as_object()?;
    if !object.keys().all(|key| matches!(key.as_str(), "trigger" | "regex" | "lorebook")) {
        return None;
    }
    let array = |key: &str| match object.get(key) {
        None => Some(None),
        Some(Value::Array(items)) => Some(Some(items.clone())),
        Some(_) => None,
    };
    Some(PreparedContentModule { trigger: array("trigger")?.unwrap_or_default(), regex: array("regex")?.unwrap_or_default(), lorebook: array("lorebook")? })
}

/// The receipt the job hands TS for these fields (`metadata`, or a card case's `card`), or `None` where a field has a
/// shape the job never sends.
fn content(format: PreparedContentFormat, fields: &Value) -> Option<PreparedContent> {
    let assets = match &fields["assets"] {
        Value::Null => Vec::new(),
        assets => assets.as_array()?.iter().map(asset).collect::<Option<Vec<_>>>()?,
    };
    let portrait_logical_id = match &fields["portraitLogicalId"] {
        Value::Null => None,
        value => Some(text(value)?),
    };
    let module = match fields.get("module") {
        None => None,
        Some(value) => Some(module(value)?),
    };
    let owner_head = match &fields["ownerHead"] {
        Value::Null => None,
        head => Some(PreparedContentOwnerHead {
            present: head["present"].as_bool()?,
            manifest_hash: text(&head["manifestHash"]),
            entry_count: head["entryCount"].as_u64()? as usize,
        }),
    };
    Some(PreparedContent {
        format,
        metadata: fields.get("metadata").or_else(|| fields.get("card")).cloned()?,
        assets,
        cas_session_id: "card-mapping-test".into(),
        portrait_logical_id,
        module,
        owner_head,
    })
}

fn format(name: &str) -> PreparedContentFormat {
    match name {
        "json-card" => PreparedContentFormat::JsonCard,
        "png-card" => PreparedContentFormat::PngCard,
        "charx-card" => PreparedContentFormat::CharxCard,
        "appended-charx-jpeg" => PreparedContentFormat::AppendedCharxJpeg,
        "risu-module" => PreparedContentFormat::RisuModule,
        other => panic!("unknown format {other}"),
    }
}

fn describe(mapping: &NativeMapping) -> Value {
    match mapping {
        NativeMapping::Mapped(Mapped::Character(character)) => json!({"character": character.get()}),
        NativeMapping::Mapped(Mapped::Module(module)) => json!({"module": module.get()}),
        NativeMapping::Declined { reason, name } => json!({"reason": reason, "name": name}),
    }
}

// The same receipt through the TS activation and through the Rust receipt: the character or module text must be the
// text TS's mapping stringifies to, which is what the activation stores (`canonicalClone` of it). A card imported as a
// module also carries the module TS converts from that character, under the IDs TS draws after the character's.
#[test]
fn receipt_mapping_matches_the_ts_activation() {
    let (mut matched, mut declined, mut modules) = (0, 0, 0);
    let (mut converted, mut conversions_declined) = (0, 0);
    for (case, result) in oracle(WIRING_CASES, WIRING_EXPECTED) {
        let id = case["id"].as_str().unwrap();
        let receipt = &case["receipt"];
        let format = format(receipt["format"].as_str().unwrap());
        let content = content(format, receipt).expect(id);
        let destination = if case["destination"] == "module" { ContentDestination::Module } else { ContentDestination::Character };
        let (mapping, module) = map_content(&content, destination, &mut Counter(0));
        let outcome = describe(&mapping);
        let mapped = if format == PreparedContentFormat::RisuModule { "module" } else { "character" };
        if case["native"] == "mapped" {
            assert_eq!(outcome, json!({mapped: result["mapped"]}), "{id}");
            assert!(result["stored"].is_string(), "{id}: the TS activation stored the {mapped}");
            matched += 1;
        } else {
            assert_eq!(outcome, case["native"], "{id}");
            declined += 1;
        }
        modules += usize::from(format == PreparedContentFormat::RisuModule);
        let expected = match &case["nativeModule"] {
            Value::Null => None,
            native if native == "mapped" => {
                assert!(result["stored"].is_string(), "{id}: the TS activation stored the converted module");
                converted += 1;
                Some(json!({"module": result["module"]}))
            }
            native => {
                conversions_declined += 1;
                Some(native.clone())
            }
        };
        assert_eq!(module.as_ref().map(describe), expected, "{id}");
    }
    assert!(matched >= 28 && declined >= 31 && modules >= 15, "matched {matched}, declined {declined}, modules {modules}");
    assert!(converted >= 8 && conversions_declined >= 5, "converted {converted}, declined {conversions_declined}");
}

// Every import case of the core card oracle, converted from the job's JSON.
#[test]
fn receipt_mapping_replays_the_card_oracle() {
    let (mut mapped, mut rejected, mut declined, mut skipped) = (0, 0, 0, 0);
    for (case, result) in oracle(CARD_CASES, CARD_EXPECTED) {
        if case["kind"] != "import" {
            continue;
        }
        let id = case["id"].as_str().unwrap();
        let format = if case.get("module").is_some() { PreparedContentFormat::CharxCard } else { PreparedContentFormat::JsonCard };
        let receivable = !case.to_string().contains('\u{fffd}');
        let Some(content) = content(format, &case).filter(|_| receivable) else {
            skipped += 1;
            continue;
        };
        let outcome = describe(&map_content(&content, ContentDestination::Character, &mut Counter(0)).0);
        if let Some(name) = case["decline"].as_str() {
            assert_eq!(outcome, json!({"reason": "unsupported", "name": name}), "{id}");
            declined += 1;
        } else if let Some(error) = result["error"].as_str() {
            let name = INVALID.iter().find(|(message, _)| error.starts_with(message)).map(|(_, name)| *name);
            assert_eq!(outcome, json!({"reason": "invalid", "name": name.expect(error)}), "{id}");
            rejected += 1;
        } else {
            assert_eq!(outcome, json!({"character": result["ok"]}), "{id}");
            mapped += 1;
        }
    }
    assert!(mapped > 20 && rejected > 5 && declined > 20, "mapped {mapped}, rejected {rejected}, declined {declined}");
    assert!(skipped < 10, "skipped {skipped}");
}

fn is_fresh_id(id: &str) -> bool { id.len() == 36 && &id[14..15] == "4" && !id.starts_with("00000000-0000") }

#[test]
fn receipt_maps_card_formats_and_modules_with_fresh_ids_only() {
    use base64::Engine as _;
    let metadata = json!({"spec": "chara_card_v3", "spec_version": "3.0", "data": {"name": "Fresh", "extensions": {}}});
    let card = json!({"metadata": metadata});
    let png = json!({"metadata": {"ccv3": base64::engine::general_purpose::STANDARD.encode(metadata.to_string())},
        "portraitLogicalId": "assets/p.png", "assets": [{"token": "native-png-portrait", "logicalId": "assets/p.png"}]});
    for (format, fields) in [(PreparedContentFormat::JsonCard, &card), (PreparedContentFormat::PngCard, &png)] {
        let mapped = receipt(content(format, fields).unwrap(), ContentDestination::Module);
        let NativeMapping::Mapped(Mapped::Character(character)) = &mapped.native_mapping else {
            panic!("{:?}", mapped.native_mapping)
        };
        let character: Value = serde_json::from_str(character.get()).unwrap();
        let (cha_id, chat_id) = (character["chaId"].as_str().unwrap(), character["chats"][0]["id"].as_str().unwrap());
        assert!(is_fresh_id(cha_id) && is_fresh_id(chat_id), "{cha_id} {chat_id}");
        assert_ne!(cha_id, chat_id);
        let Some(NativeMapping::Mapped(Mapped::Module(module))) = &mapped.native_module_mapping else {
            panic!("{:?}", mapped.native_module_mapping)
        };
        let module: Value = serde_json::from_str(module.get()).unwrap();
        let module_id = module["id"].as_str().unwrap();
        assert!(is_fresh_id(module_id) && module_id != cha_id && module_id != chat_id, "{module}");
        assert!(receipt(content(format, fields).unwrap(), ContentDestination::Character).native_module_mapping.is_none());
    }
    let risum = json!({"metadata": {"name": "Fresh", "id": "stored-id"}, "ownerHead": {"present": false, "manifestHash": null, "entryCount": 0}});
    let mapped = receipt(content(PreparedContentFormat::RisuModule, &risum).unwrap(), ContentDestination::Module);
    let NativeMapping::Mapped(Mapped::Module(module)) = &mapped.native_mapping else { panic!("{:?}", mapped.native_mapping) };
    let module: Value = serde_json::from_str(module.get()).unwrap();
    assert!(is_fresh_id(module["id"].as_str().unwrap()), "{module}");
    assert!(mapped.native_module_mapping.is_none(), "a RISUM module carries one module");
}

// TS's validator reads exactly these fields beside the content's own, and the character or module text crosses
// verbatim.
#[test]
fn receipt_serializes_the_mapping_beside_the_content() {
    let card = json!({"metadata": {"spec": "chara_card_v3", "data": {"name": "Shape"}}, "portraitLogicalId": "assets/p.png",
        "assets": [{"token": "t", "logicalId": "assets/p.png"}]});
    let text = || RawValue::from_string(r#"{"b":1,"10":2,"a":[3]}"#.into()).unwrap();
    for (format, mapping, module, expected) in [
        (PreparedContentFormat::CharxCard, NativeMapping::Mapped(Mapped::Character(text())), None,
            r#""nativeMapping":{"outcome":"mapped","character":{"b":1,"10":2,"a":[3]}}"#),
        (PreparedContentFormat::RisuModule, NativeMapping::Mapped(Mapped::Module(text())), None,
            r#""nativeMapping":{"outcome":"mapped","module":{"b":1,"10":2,"a":[3]}}"#),
        (PreparedContentFormat::JsonCard, declined(DeclineReason::Unsupported, "lowLevelAccess"), None,
            r#""nativeMapping":{"outcome":"declined","reason":"unsupported","name":"lowLevelAccess"}"#),
        (PreparedContentFormat::JsonCard, NativeMapping::Mapped(Mapped::Character(text())),
            Some(NativeMapping::Mapped(Mapped::Module(text()))),
            r#""nativeMapping":{"outcome":"mapped","character":{"b":1,"10":2,"a":[3]}},"nativeModuleMapping":{"outcome":"mapped","module":{"b":1,"10":2,"a":[3]}}"#),
    ] {
        let build = || content(format, &card).unwrap();
        let converted = module.is_some();
        let receipt = PreparedContentReceipt { content: build(), native_mapping: mapping, native_module_mapping: module };
        let written = serde_json::to_string(&receipt).unwrap();
        assert!(written.contains(expected), "{written}");
        let mut value: Value = serde_json::from_str(&written).unwrap();
        let fields = value.as_object_mut().unwrap();
        assert!(fields.remove("nativeMapping").is_some());
        assert_eq!(fields.remove("nativeModuleMapping").is_some(), converted, "{written}");
        assert_eq!(value, serde_json::to_value(build()).unwrap());
    }
}

// A card or module above the size cutoff is mapped in TS. Its size is its JSON text, or the decoded length of a PNG
// card's metadata; counting the text stops past the cutoff.
#[test]
fn receipt_leaves_content_above_the_size_cutoff_to_ts() {
    use base64::Engine as _;
    let metadata = json!({"spec": "chara_card_v3", "spec_version": "3.0", "data": {"name": "Sized", "extensions": {}}});
    let text = metadata.to_string();
    let json_card = content(PreparedContentFormat::JsonCard, &json!({"metadata": metadata})).unwrap();
    let png_card = content(PreparedContentFormat::PngCard, &json!({
        "metadata": {"chara": "e30=", "ccv3": base64::engine::general_purpose::STANDARD.encode(&text)},
        "portraitLogicalId": "assets/p.png", "assets": [{"token": "native-png-portrait", "logicalId": "assets/p.png"}],
    }))
    .unwrap();
    let module = json!({"name": "Sized", "lorebook": [{"key": "k", "content": "c"}]});
    let risum = content(PreparedContentFormat::RisuModule, &json!({"metadata": module})).unwrap();
    assert_eq!(card_bytes(&json_card, usize::MAX), Some(text.len()));
    assert_eq!(card_bytes(&png_card, usize::MAX), Some(text.len().div_ceil(3) * 3));
    assert_eq!(card_bytes(&risum, usize::MAX), Some(module.to_string().len()));
    for (content, name) in [(&json_card, "cardBytes"), (&png_card, "cardBytes"), (&risum, "moduleBytes")] {
        let size = card_bytes(content, usize::MAX).unwrap();
        assert_eq!((card_bytes(content, size), card_bytes(content, size - 1)), (Some(size), None));
        let converted = content.format != PreparedContentFormat::RisuModule;
        let (within, module) = map_content_within(content, ContentDestination::Module, &mut Counter(0), size);
        assert!(matches!(within, NativeMapping::Mapped(_)), "{within:?}");
        assert!(matches!(module, None | Some(NativeMapping::Mapped(_))) && module.is_some() == converted, "{module:?}");
        let (above, module) = map_content_within(content, ContentDestination::Module, &mut Counter(0), size - 1);
        assert_eq!(describe(&above), json!({"reason": "limit", "name": name}));
        assert_eq!(module.as_ref().map(describe), converted.then(|| json!({"reason": "limit", "name": name})));
    }
}

// A card the mapping declines declines its module too, and one whose conversion declines keeps its character mapped,
// which TS converts.
#[test]
fn receipt_declines_a_card_module_with_its_card_or_alone() {
    let declined_card = content(PreparedContentFormat::JsonCard, &json!({"metadata": json!({"spec": "chara_card_v3", "spec_version": "3.0",
        "data": {"name": "Low", "extensions": {"risuai": {"lowLevelAccess": true}}}})})).unwrap();
    let (mapping, module) = map_content(&declined_card, ContentDestination::Module, &mut Counter(0));
    let expected = json!({"reason": "unsupported", "name": "lowLevelAccess"});
    assert_eq!((describe(&mapping), module.as_ref().map(describe)), (expected.clone(), Some(expected)));
    let greetings = content(PreparedContentFormat::JsonCard, &json!({"metadata": json!({"spec": "chara_card_v3", "spec_version": "3.0",
        "data": {"name": "Greeting", "first_mes": "Hi.", "alternate_greetings": "Hey.", "extensions": {}}})})).unwrap();
    let (mapping, module) = map_content(&greetings, ContentDestination::Module, &mut Counter(0));
    assert!(matches!(mapping, NativeMapping::Mapped(Mapped::Character(_))), "{mapping:?}");
    assert_eq!(module.as_ref().map(describe), Some(json!({"reason": "unsupported", "name": "alternateGreetings"})));
}
