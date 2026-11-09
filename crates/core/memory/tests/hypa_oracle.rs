//! Actual retrieval executes here over fixed cache/inference inputs. Only tokenizer observations are replayed.
use risunest_ir::{json, Budget, Limits, OwnedVec, Text};
use risunest_ir::chat::PromptMessage;
use risunest_ir::stage::{EmbedRequest, HostReply, HostRequest, MemoryInput, MemoryStage, StageError, Step, TokenCounter, TokenInput};
use risunest_memory::hanurai::{Hanurai, HanuraiConfig, Retrieval, RetrievalOutput};
use risunest_memory::hypa_v1::*;
use serde_json::{json as j, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, atomic::AtomicBool};
fn budget() -> Budget { Budget::new(Limits { max_owned_bytes: 64 << 20, max_json_bytes: 16 << 20, max_text_units: 4 << 20,
    max_values: 1_000_000, max_depth: 128, max_work: 100_000_000 }, Arc::new(AtomicBool::new(false))).unwrap() }
fn text(v: &Value, b: &Budget) -> Text {
    if let Some(s) = v.as_str() { Text::from_str(s, b).unwrap() }
    else { Text::from_units(&v["u"].as_array().unwrap().iter().map(|n| n.as_u64().unwrap() as u16).collect::<Vec<_>>(), b).unwrap() }
}
fn packed_text(v: &Text) -> Value { let units: Vec<_> = v.units().collect(); match String::from_utf16(&units) { Ok(s) => j!(s), Err(_) => j!({"u":units}) } }
fn number(v: &Value) -> f64 { match v.as_str() { Some("NaN") => f64::NAN, Some("Infinity") => f64::INFINITY,
    Some("-Infinity") => f64::NEG_INFINITY, Some("-0") => -0.0, _ => v.as_f64().unwrap() } }
fn packed_number(n: f64) -> Value { if n.is_nan() { j!("NaN") } else if n == f64::INFINITY { j!("Infinity") }
    else if n == f64::NEG_INFINITY { j!("-Infinity") } else if n == 0.0 && n.is_sign_negative() { j!("-0") } else { j!(n) } }
// JSON has one Number type; serde_json separately stores integral and floating representations.
fn same_js_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64().unwrap().to_bits() == b.as_f64().unwrap().to_bits(),
        (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a,b)| same_js_value(a,b)),
        (Value::Object(a), Value::Object(b)) => a.len() == b.len() && a.iter().all(|(key,value)| b.get(key).is_some_and(|other| same_js_value(value,other))),
        _ => left == right,
    }
}
fn key_string(key: CacheKey) -> String { String::from_utf8(key.hex().to_vec()).unwrap() }
fn parse_key(s: &str) -> CacheKey { let mut bytes = [0; 32]; for (i, slot) in bytes.iter_mut().enumerate() { *slot = u8::from_str_radix(&s[i*2..i*2+2], 16).unwrap(); } CacheKey(bytes) }
fn vector(v: &Value, b: &Budget) -> Option<EmbeddingVector> {
    if v.get("absent").is_some() || v.get("null").is_some() { return None; }
    if v["precision"] == "f32" {
        let input = v["bits"].as_array().unwrap();
        let offset = v["view"]["offset"].as_u64().unwrap_or(0) as usize;
        let length = v["view"]["length"].as_u64().map(|v| v as usize).unwrap_or(input.len());
        let mut bits = OwnedVec::new(b).unwrap();
        for bit in &input[offset..offset + length] { bits.push(bit.as_u64().unwrap() as u32).unwrap(); }
        Some(EmbeddingVector::F32(bits))
    } else {
        let mut values = OwnedVec::new(b).unwrap(); for n in v["values"].as_array().unwrap() { values.push(number(n)).unwrap(); }
        Some(EmbeddingVector::F64(values))
    }
}
fn bytes(bits: &[u32]) -> String { bits.iter().flat_map(|v| v.to_le_bytes()).map(|v| format!("{v:02x}")).collect() }
fn target(input: &Value, b: &Budget) -> EmbeddingTarget {
    EmbeddingTarget::capture(&text(&input["target"]["model"], b), None,
        &text(input["target"].get("endpoint").unwrap_or(&j!("")), b),
        &text(input["target"].get("customModel").unwrap_or(&j!("")), b), b).unwrap()
}
type Events = Arc<Mutex<Vec<Value>>>;
type Stored = Arc<Mutex<Vec<(CacheKey, Vec<u32>, Value)>>>;
struct Cache { stored: Stored, events: Events, faults: Value, reads: usize, writes: usize }
impl ImmediateCache for Cache {
    fn read_batch(&mut self, keys: &[CacheKey], b: &Budget) -> Result<OwnedVec<Option<CacheRow>>, CacheError> {
        self.reads += 1;
        let mut event = j!({"kind":"cache-read","keys": keys.iter().map(|key| key_string(*key)).collect::<Vec<_>>()});
        if self.faults["readFaultAt"].as_u64() == Some(self.reads as u64) {
            event["failure"] = j!({"name":"Error","message":"synthetic cache read failure"}); self.events.lock().unwrap().push(event); return Err(CacheError::Unavailable);
        }
        if self.faults["malformedReadAt"].as_u64() == Some(self.reads as u64) {
            event["malformed"] = j!([0]); self.events.lock().unwrap().push(event); return Err(CacheError::Unavailable);
        }
        let stored = self.stored.lock().unwrap();
        let mut rows = OwnedVec::new(b).map_err(|e| CacheError::Terminal(e.into()))?;
        let mut observed = Vec::new();
        for key in keys {
            if let Some((_, bits, row)) = stored.iter().find(|(candidate, _, _)| candidate == key) {
                observed.push(row.clone());
                if !bits.is_empty() {
                    let mut values = OwnedVec::new(b).map_err(|e| CacheError::Terminal(e.into()))?;
                    values.extend(bits).map_err(|e| CacheError::Terminal(e.into()))?;
                    rows.push(Some(CacheRow { key: *key, dimensions: bits.len(), bits: values })).map_err(|e| CacheError::Terminal(e.into()))?;
                }
            }
        }
        event["rows"] = j!(observed); self.events.lock().unwrap().push(event); Ok(rows)
    }
    fn write_batch(&mut self, target: &EmbeddingTarget, entries: &[CacheWrite], _: &Budget) -> Result<(), CacheError> {
        // The real bridge rejects empty vectors during frame construction, before native IO.
        if entries.iter().any(|entry| entry.bits.as_slice().is_empty()) { return Err(CacheError::Unavailable); }
        self.writes += 1;
        let (model, endpoint) = target.identity();
        let rows: Vec<_> = entries.iter().map(|entry| j!({"key":entry.key.map(key_string).map(Value::String).unwrap_or(j!({"undefined":true})),
            "producer":"hypa-v1-text","model":packed_text(model),"endpoint":if endpoint.len_units()==0 { Value::Null } else { packed_text(endpoint) },
            "preprocessVersion":1,"dimensions":entry.bits.as_slice().len(),"byteLength":entry.bits.as_slice().len()*4,
            "metadata":null,"bytes":bytes(entry.bits.as_slice())})).collect();
        let mut event = j!({"kind":"cache-write","entries":rows});
        let invalid = entries.iter().any(|entry| entry.key.is_none());
        if invalid || self.faults["writeFaultAt"].as_u64() == Some(self.writes as u64) {
            event["failure"] = j!({"name":"Error","message":if invalid {"synthetic invalid cache key"} else {"synthetic cache write failure"}});
            self.events.lock().unwrap().push(event); return Err(CacheError::Unavailable);
        }
        let mut stored = self.stored.lock().unwrap();
        for (entry, row) in entries.iter().zip(rows) {
            let key = entry.key.unwrap(); let record = (key, entry.bits.as_slice().to_vec(), row);
            if let Some(old) = stored.iter_mut().find(|(candidate, _, _)| *candidate == key) { *old = record; } else { stored.push(record); }
        }
        event["committed"] = j!(true); self.events.lock().unwrap().push(event); Ok(())
    }
}
struct Service { input: Arc<Mutex<VecDeque<Value>>>, pending: Option<Value>, yields: u8, remaining: u8 }
impl Service {
    fn output(input: Value, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        if let Some(failure) = input.get("failure") { return Err(StageError::Host(text(&failure["message"], b).units().collect())); }
        let mut out = OwnedVec::new(b)?;
        for item in input["result"].as_array().unwrap() { out.push(vector(item, b))?; }
        Ok(Step::Done(out))
    }
    fn leaf(b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        Ok(Step::NeedsHost(HostRequest::Embed(EmbedRequest { model: Text::from_str("synthetic-vector-service", b)?, texts: Vec::new() })))
    }
}
impl EmbeddingService for Service {
    fn embed(&mut self, request: EmbeddingRequest, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        let input = self.input.lock().unwrap().pop_front().expect("unexpected inference");
        let purpose = match request.purpose { EmbeddingPurpose::Document => "document", EmbeddingPurpose::Query => "query" };
        let (shape, texts) = match request.input { EmbeddingInput::Scalar(value) => ("scalar", vec![packed_text(&value)]),
            EmbeddingInput::Batch(values) => ("batch", values.as_slice().iter().map(packed_text).collect()) };
        assert_eq!(j!({"purpose":purpose,"shape":shape,"texts":texts}), j!({"purpose":input["purpose"],"shape":input["shape"],"texts":input["texts"]}));
        if self.yields == 0 { Self::output(input, b) } else { self.pending = Some(input); self.remaining = self.yields; Self::leaf(b) }
    }
    fn resume(&mut self, reply: HostReply, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        assert!(matches!(reply, HostReply::Embeddings(_)));
        self.remaining -= 1;
        if self.remaining > 0 { Self::leaf(b) } else { Self::output(self.pending.take().unwrap(), b) }
    }
}
fn completed(stage: &mut HypaV1Retrieval<Cache,Service>, mut step: Result<Step<RetrievalOutput>,StageError>, b: &Budget) -> Result<RetrievalOutput,StageError> {
    loop { match step? { Step::Done(out) => return Ok(out), Step::NeedsHost(_) => step = stage.resume(HostReply::Embeddings(Vec::new()), b) } }
}
fn message(v: &Value, b: &Budget) -> PromptMessage { PromptMessage::from_value(json::parse_json(&v.to_string(), b).unwrap(), b).unwrap() }
fn packed_message(v: &PromptMessage, b: &Budget) -> Value {
    serde_json::from_str(json::stringify(&v.to_value(b).unwrap(), b).unwrap().unwrap().as_unicode().unwrap()).unwrap()
}
struct Tokens { tape: VecDeque<Value> }
impl TokenCounter for Tokens {
    fn count(&mut self, input: TokenInput<'_>, b: &Budget) -> Result<Step<u32>,StageError> {
        let TokenInput::Message(input) = input else { panic!() };
        let next = self.tape.pop_front().expect("unobserved tokenizer boundary");
        assert_eq!(packed_message(input,b), next["message"]);
        Ok(Step::Done(next["count"].as_u64().unwrap() as u32))
    }
    fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<u32>,StageError> { unreachable!() }
}
#[test]
fn real_typescript_retrieval_matches_native_math_cache_and_hanurai_composition() {
    let bytes = include_bytes!("../../../../tests/fixtures/rust-core/memory/hypa-v1/observations.cases.json");
    let cases: Value = serde_json::from_slice(bytes).unwrap();
    let expected: Value = serde_json::from_str(include_str!("../../../../tests/fixtures/rust-core/memory/hypa-v1/observations.expected.json")).unwrap();
    assert_eq!(cases["schema"], expected["schema"]);
    assert_eq!(expected["fixtureSha256"], Sha256::digest(bytes).iter().map(|v| format!("{v:02x}")).collect::<String>());
    assert_eq!(cases["cases"].as_array().unwrap().len(), expected["cases"].as_array().unwrap().len());
    for (case, expected) in cases["cases"].as_array().unwrap().iter().zip(expected["cases"].as_array().unwrap()) {
        assert_eq!(case["id"], expected["id"]);
        for yields in [0, 2] { run(case, &expected["result"], yields); }
    }
}
fn run(case: &Value, expected: &Value, yields: u8) {
    let b = budget(); let input = &case["input"]; let target = target(input,&b);
    assert_eq!(expected["boundary"], "hypa-v1-real-retrieval");
    let documents: Vec<_> = input["documents"].as_array().unwrap().iter().map(|v| text(v,&b)).collect();
    let keys: Vec<_> = documents.iter().map(|doc| key_string(target.cache_key(doc,&b).unwrap())).collect();
    if input["mode"] == "keys" { assert_eq!(j!(keys), expected["outcome"]["keys"], "{}", case["id"]); return; }
    if input["mode"] != "hanurai" { assert_eq!(keys, expected["hashes"].as_array().unwrap().iter().map(|h| h["hex"].as_str().unwrap().to_owned()).collect::<Vec<_>>(), "keys {}", case["id"]); }
    let stored: Stored = Arc::new(Mutex::new(Vec::new()));
    if let Some(seeds) = input["cache"]["seeds"].as_array() {
        for seed in seeds {
            let key = if let Some(s) = seed["key"].as_str() { parse_key(s) } else { target.cache_key(&text(&seed["text"],&b),&b).unwrap() };
            let vector = vector(&seed["vector"],&b).unwrap();
            let bits: Vec<u32> = match vector { EmbeddingVector::F32(bits) => bits.as_slice().to_vec(), EmbeddingVector::F64(values) => values.as_slice().iter().map(|n| (*n as f32).to_bits()).collect() };
            let row = j!({"key":key_string(key),"dimensions":bits.len(),"byteLength":bits.len()*4,"bytes":bytes(&bits)});
            let record = (key,bits,row); let mut rows = stored.lock().unwrap();
            if let Some(old) = rows.iter_mut().find(|(candidate,_,_)| *candidate==key) { *old=record; } else { rows.push(record); }
        }
    }
    assert_eq!(j!(stored.lock().unwrap().iter().map(|(_,_,v)| v.clone()).collect::<Vec<_>>()),expected["initial"]);
    let events: Events = Arc::new(Mutex::new(Vec::new()));
    let tape = Arc::new(Mutex::new(input["embeddings"].as_array().unwrap().iter().cloned().collect()));
    let cache = Cache { stored: stored.clone(), events: events.clone(), faults: input["cache"].clone(), reads:0,writes:0 };
    let service = Service { input:tape.clone(),pending:None,yields,remaining:0 };
    let mut stage = HypaV1Retrieval::new(target,cache,service,input["target"]["cacheBatchLimit"].as_u64().unwrap() as usize).unwrap();
    if input["mode"] == "hanurai" {
        let args=&input["hanurai"];
        let tokens=Tokens { tape:expected["events"].as_array().unwrap().iter().filter(|e| e["kind"]=="token").cloned().collect() };
        let config=HanuraiConfig { split:args["split"].as_bool().unwrap(),reserve_tokens:number(&args["reserve"]),context_limit_error:text(&expected["contextLimitError"],&b) };
        let mut policy=Hanurai::new(config,tokens,stage);
        let mut step=policy.run(MemoryInput { chats:args["chats"].as_array().unwrap().iter().map(|v| message(v,&b)).collect(),current_tokens:number(&args["currentTokens"]),max_context_tokens:number(&args["maxContextTokens"]) },&b);
        loop { match step.unwrap() { Step::NeedsHost(_) => step=policy.resume(HostReply::Embeddings(Vec::new()),&b), Step::Done(out) => {
            assert!(out.error.is_none()); assert!(out.effects.is_empty());
            assert_eq!(j!(out.chats.iter().map(|v| packed_message(v,&b)).collect::<Vec<_>>()),expected["outcome"]["chats"],"{}",case["id"]);
            assert_eq!(out.current_tokens,number(&expected["outcome"]["tokens"])); break;
        } } }
    } else {
        let step=stage.prepare(documents,&b); let prepared=completed(&mut stage,step,&b);
        let mut failed=prepared.is_err(); let mut results=Vec::new();
        if !failed {
            assert!(matches!(prepared,Ok(RetrievalOutput::Prepared)));
            for query in input["queries"].as_array().unwrap() {
                let step=stage.query(text(query,&b),&b);
                match completed(&mut stage,step,&b) { Ok(RetrievalOutput::Scored(scored)) => results.push(j!(scored.iter().map(|v| j!([v.document.as_ref().map(packed_text).unwrap_or(j!({"undefined":true})),packed_number(v.score)])).collect::<Vec<_>>())),
                    Err(_) => { failed=true;break; }, _=>panic!() }
            }
        }
        assert_eq!(failed,expected["outcome"].get("failure").is_some(),"failure {}",case["id"]);
        if !failed { assert!(same_js_value(&j!(results),&expected["outcome"]["queries"]),"scored {} yields={yields}: actual={}",case["id"],j!(results)); }
    }
    assert!(tape.lock().unwrap().is_empty(),"inference not consumed {}",case["id"]);
    let cache_events:Vec<_>=expected["events"].as_array().unwrap().iter().filter(|e| e["kind"]=="cache-read"||e["kind"]=="cache-write").cloned().collect();
    assert_eq!(*events.lock().unwrap(),cache_events,"cache events {} yields={yields}",case["id"]);
    assert_eq!(j!(stored.lock().unwrap().iter().map(|(_,_,v)|v.clone()).collect::<Vec<_>>()),expected["durable"],"durable {}",case["id"]);
}

#[test]
fn typed_service_vectors_match_real_typescript_cache_precision_helper() {
    let input_bytes = include_bytes!("../../../../tests/fixtures/rust-core/memory/hypa-v1/precision.cases.json");
    let cases: Value = serde_json::from_slice(input_bytes).unwrap();
    let expected: Value = serde_json::from_str(include_str!("../../../../tests/fixtures/rust-core/memory/hypa-v1/precision.expected.json")).unwrap();
    assert_eq!(cases["schema"], expected["schema"]);
    assert_eq!(expected["fixtureSha256"], Sha256::digest(input_bytes).iter().map(|v| format!("{v:02x}")).collect::<String>());
    assert_eq!(cases["cases"].as_array().unwrap().len(), expected["cases"].as_array().unwrap().len());
    for (case, expected) in cases["cases"].as_array().unwrap().iter().zip(expected["cases"].as_array().unwrap()) {
        assert_eq!(case["id"], expected["id"]);
        assert_eq!(expected["result"]["boundary"], "hypa-v1-typed-vector-conversion");
        let b = budget();
        let vectors = case["input"]["vectors"].as_array().unwrap();
        let documents: Vec<_> = (0..vectors.len()).map(|index| j!(format!("typed vector {index}"))).collect();
        // This is a native typed service boundary, deliberately not an HTTP/JSON response.
        let request = j!({"purpose":"document","shape":"batch","texts":documents,"result":vectors});
        let tape = Arc::new(Mutex::new(VecDeque::from([request])));
        let stored: Stored = Arc::new(Mutex::new(Vec::new()));
        let cache = Cache { stored: stored.clone(), events: Arc::new(Mutex::new(Vec::new())), faults: Value::Null, reads:0,writes:0 };
        let service = Service { input:tape.clone(),pending:None,yields:0,remaining:0 };
        let target = target(&j!({"target":{"model":"custom","endpoint":"","customModel":"typed"}}), &b);
        let mut stage = HypaV1Retrieval::new(target,cache,service,1024).unwrap();
        assert!(matches!(stage.prepare(documents.iter().map(|v| text(v,&b)).collect(),&b),Ok(Step::Done(RetrievalOutput::Prepared))));
        let conversions: Vec<_> = stored.lock().unwrap().iter().map(|(_,bits,_)| j!({"dimensions":bits.len(),"bytesHex":bytes(bits)})).collect();
        assert_eq!(j!(conversions),expected["result"]["conversions"],"{}",case["id"]);
        assert!(tape.lock().unwrap().is_empty());
    }
}
