#[path = "support/embedding_adapter.rs"] mod embedding_adapter;
use embedding_adapter::{Adapter, Settings};
use risunest_ir::{Budget, Limits, OwnedVec, Text};
use risunest_ir::stage::{HostReply, HostRequest, StageError, Step};
use risunest_memory::hanurai::{Retrieval, RetrievalOutput};
use risunest_memory::hypa_v1::*;
use risunest_providers::embeddings::{Failure, Purpose};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, atomic::AtomicBool};

fn budget() -> Budget { Budget::new(Limits { max_owned_bytes: 8 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
    max_values: 100_000, max_depth: 128, max_work: 10_000_000 }, Arc::new(AtomicBool::new(false))).unwrap() }
fn text(value: &str, b: &Budget) -> Text { Text::from_str(value, b).unwrap() }
fn target(b: &Budget) -> EmbeddingTarget { EmbeddingTarget::capture(&text("custom", b), None, &text("https://synthetic.invalid/old", b), &text("old-cache-model", b), b).unwrap() }
type Stored = Arc<Mutex<Vec<(CacheKey, Vec<u32>)>>>;
struct Cache { stored: Stored }
impl ImmediateCache for Cache {
    fn read_batch(&mut self, keys: &[CacheKey], b: &Budget) -> Result<OwnedVec<Option<CacheRow>>, CacheError> {
        let mut out = OwnedVec::new(b).map_err(|e| CacheError::Terminal(e.into()))?;
        for key in keys {
            let stored = self.stored.lock().unwrap();
            if let Some((_, source)) = stored.iter().find(|(candidate, _)| candidate == key) {
                let mut bits = OwnedVec::new(b).map_err(|e| CacheError::Terminal(e.into()))?;
                bits.extend(source).map_err(|e| CacheError::Terminal(e.into()))?;
                out.push(Some(CacheRow { key: *key, dimensions: source.len(), bits })).map_err(|e| CacheError::Terminal(e.into()))?;
            }
        }
        Ok(out)
    }
    fn write_batch(&mut self, _: &EmbeddingTarget, entries: &[CacheWrite], _: &Budget) -> Result<(), CacheError> {
        self.stored.lock().unwrap().extend(entries.iter().map(|entry| (entry.key.unwrap(), entry.bits.as_slice().to_vec()))); Ok(())
    }
}
fn take_http(step: Step<RetrievalOutput>, model: &str, key: &str) -> Value {
    let Step::NeedsHost(HostRequest::Http(http)) = step else { panic!("expected real provider HTTP") };
    assert_eq!(http.url, "https://synthetic.invalid/old/embeddings");
    assert_eq!(http.headers[1].1, format!("Bearer {key}"));
    let body: Value = serde_json::from_slice(&http.body).unwrap();
    assert_eq!(body["model"], model); body
}
fn response(values: &[f64]) -> HostReply {
    HostReply::Http { status: 200, body: serde_json::to_vec(&json!({"data":values.iter().map(|v| json!({"embedding":[v]})).collect::<Vec<_>>()})).unwrap() }
}

#[test]
fn actual_service_composes_repeated_batches_queries_and_cache_hits() {
    let b = budget();
    let settings = Arc::new(Mutex::new(Settings { custom_model: Some("first-model".into()), custom_key: Some("first-key".into()) }));
    let adapter = Adapter::new(settings.clone());
    let purposes = adapter.purposes.clone(); let moves = adapter.moves.clone();
    let stored: Stored = Arc::new(Mutex::new(Vec::new()));
    let mut stage = HypaV1Retrieval::new(target(&b), Cache { stored: stored.clone() }, adapter, 1024).unwrap();
    let documents = (0..51).map(|index| text(&format!("doc{index}"), &b)).collect();
    let body = take_http(stage.prepare(documents, &b).unwrap(), "first-model", "first-key");
    assert_eq!(body["input"].as_array().unwrap().len(), 50);
    *settings.lock().unwrap() = Settings { custom_model: Some("second-model".into()), custom_key: Some("second-key".into()) };
    let values: Vec<_> = (0..50).map(|n| 1.00000001 + n as f64 * 0.000000001).collect();
    let body = take_http(stage.resume(response(&values), &b).unwrap(), "second-model", "second-key");
    assert_eq!(body["input"], json!(["doc50"]));
    assert!(matches!(stage.resume(response(&[1.00000009]), &b), Ok(Step::Done(RetrievalOutput::Prepared))));
    assert_eq!(stored.lock().unwrap().len(), 51);
    assert!(stored.lock().unwrap().iter().all(|(_, bits)| bits == &[1f32.to_bits()] || bits == &[f32::from_bits(1f32.to_bits() + 1).to_bits()]));
    for query in ["q1", "q2"] {
        let body = take_http(stage.query(text(query, &b), &b).unwrap(), "second-model", "second-key");
        assert_eq!(body["input"], query);
        let Step::Done(RetrievalOutput::Scored(scored)) = stage.resume(response(&[1.0]), &b).unwrap() else { panic!() };
        assert_eq!(scored[0].document.as_ref().unwrap().as_unicode(), Some("doc50"));
        assert_eq!(scored[0].score, 1.00000009);
    }
    assert_eq!(*purposes.lock().unwrap(), [Purpose::Document, Purpose::Document, Purpose::Query, Purpose::Query]);
    assert!(moves.lock().unwrap().iter().all(|(from, to)| from == to));
    drop(stage);
    let cached_adapter = Adapter::new(settings);
    let cached_purposes = cached_adapter.purposes.clone();
    let mut cached = HypaV1Retrieval::new(target(&b), Cache { stored }, cached_adapter, 1024).unwrap();
    assert!(matches!(cached.prepare((0..51).map(|index| text(&format!("doc{index}"), &b)).collect(), &b), Ok(Step::Done(RetrievalOutput::Prepared))));
    assert!(cached_purposes.lock().unwrap().is_empty());
    drop(cached);
    assert_eq!(b.owned_bytes(), Budget::context_bytes());
}

#[test]
fn query_failure_keeps_document_write_and_owned_diagnostic_without_retry() {
    for kind in ["http", "decode", "transport"] {
        let b = budget();
        let settings = Arc::new(Mutex::new(Settings { custom_model: Some("model".into()), custom_key: Some("key".into()) }));
        let adapter = Adapter::new(settings);
        let diagnostic = adapter.diagnostic.clone(); let purposes = adapter.purposes.clone();
        let stored: Stored = Arc::new(Mutex::new(Vec::new()));
        let mut stage = HypaV1Retrieval::new(target(&b), Cache { stored: stored.clone() }, adapter, 1024).unwrap();
        take_http(stage.prepare(vec![text("doc", &b)], &b).unwrap(), "model", "key");
        assert!(matches!(stage.resume(response(&[1.00000001]), &b), Ok(Step::Done(RetrievalOutput::Prepared))));
        take_http(stage.query(text("q", &b), &b).unwrap(), "model", "key");
        let response = match kind { "http" => HostReply::Http { status: 400, body: br#"{"error":"remote-secret","extra":1}"#.to_vec() },
            "decode" => HostReply::Http { status: 200, body: b"{".to_vec() }, _ => HostReply::Failed { message: text("transport-secret", &b) } };
        assert!(matches!(stage.resume(response, &b), Err(StageError::Invalid(category)) if category == "embedding-provider-failure"));
        assert_eq!(*stored.lock().unwrap(), [(target(&b).cache_key(&text("doc", &b), &b).unwrap(), vec![1f32.to_bits()])]);
        assert!(matches!(stage.query(text("again", &b), &b), Err(StageError::Invalid(_))));
        assert_eq!(*purposes.lock().unwrap(), [Purpose::Document, Purpose::Query]);
        drop(stage);
        assert!(b.owned_bytes() > Budget::context_bytes());
        let error = diagnostic.lock().unwrap().take().unwrap();
        match &error {
            Failure::Remote { diagnostic, .. } => assert_eq!(diagnostic.as_unicode(), Some(r#"{"error":"remote-secret","extra":1}"#)),
            Failure::Decode { diagnostic } => assert_eq!(diagnostic.as_unicode(), Some("embedding-response-json")),
            Failure::Transport { diagnostic } => assert_eq!(diagnostic.as_unicode(), Some("transport-secret")), _ => panic!(),
        }
        drop(error);
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
    }
}
