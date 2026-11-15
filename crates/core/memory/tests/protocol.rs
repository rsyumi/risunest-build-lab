use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::stage::{EmbedRequest, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, TokenCounter, TokenInput};
use risunest_ir::{Budget, Limits, Text};
use risunest_memory::hanurai::{Hanurai, HanuraiConfig, Retrieval, RetrievalOutput, ScoredDocument};
use std::borrow::Cow;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};

fn budget(cancel: Arc<AtomicBool>) -> Budget {
    Budget::new(Limits { max_owned_bytes: 4 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
        max_values: 100_000, max_depth: 128, max_work: 1_000_000 }, cancel).unwrap()
}
fn text(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).unwrap() }
fn config(reserve: f64, split: bool, budget: &Budget) -> HanuraiConfig {
    HanuraiConfig { reserve_tokens: reserve, split, context_limit_error: text("context limit", budget) }
}
fn input(chats: Vec<PromptMessage>, tokens: f64, max: f64) -> MemoryInput {
    MemoryInput { chats, current_tokens: tokens, max_context_tokens: max }
}
fn done(step: Result<Step<MemoryOutput>, StageError>) -> MemoryOutput {
    match step.unwrap() { Step::Done(output) => output, Step::NeedsHost(_) => panic!("unexpected suspension") }
}

type Calls = Arc<Mutex<Vec<Vec<u16>>>>;
struct Tokens { calls: Calls, suspend: bool }
impl TokenCounter for Tokens {
    fn count(&mut self, input: TokenInput<'_>, budget: &Budget) -> Result<Step<u32>, StageError> {
        let TokenInput::Message(message) = input else { panic!("complete messages required") };
        self.calls.lock().unwrap().push(message.content.units().collect());
        if self.suspend { Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text: message.content.copy(budget)? })) }
        else { Ok(Step::Done(1)) }
    }
    fn resume(&mut self, reply: HostReply, _: &Budget) -> Result<Step<u32>, StageError> {
        match reply {
            HostReply::Tokens(ids) => Ok(Step::Done(ids.len() as u32)),
            HostReply::Failed { message } => Err(StageError::Host(message.units().collect())),
            HostReply::Cancelled => Err(StageError::Cancelled),
            HostReply::Unsupported(reason) => Err(StageError::Unsupported(reason)),
            _ => Err(StageError::Invalid(Cow::Borrowed("test-token-reply"))),
        }
    }
}
struct Search { calls: Calls, documents: Vec<Text>, suspend_query: bool, prepared: Arc<Mutex<usize>> }
impl Retrieval for Search {
    fn prepare(&mut self, documents: Vec<Text>, _: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        *self.prepared.lock().unwrap() += 1;
        for document in &documents { self.calls.lock().unwrap().push(document.units().collect()); }
        self.documents = documents;
        Ok(Step::Done(RetrievalOutput::Prepared))
    }
    fn query(&mut self, query: Text, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        self.calls.lock().unwrap().push(query.units().collect());
        if self.suspend_query {
            Ok(Step::NeedsHost(HostRequest::Embed(EmbedRequest { model: Text::from_units(&[0xd800], budget)?, texts: vec![query] })))
        } else {
            Ok(Step::Done(RetrievalOutput::Scored(self.documents.iter().map(|document|
                Ok(ScoredDocument { document: Some(document.copy(budget)?), score: 1.0 })).collect::<Result<_, StageError>>()?)))
        }
    }
    fn resume(&mut self, reply: HostReply, _: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        self.calls.lock().unwrap().push(vec![0xffff]);
        match reply {
            HostReply::Embeddings(_) => Ok(Step::Done(RetrievalOutput::Scored(Vec::new()))),
            HostReply::Failed { message } => Err(StageError::Host(message.units().collect())),
            HostReply::Cancelled => Err(StageError::Cancelled),
            HostReply::Unsupported(reason) => Err(StageError::Unsupported(reason)),
            _ => Err(StageError::Invalid(Cow::Borrowed("test-retrieval-reply"))),
        }
    }
}
fn dependencies(token_suspend: bool, query_suspend: bool) -> (Tokens, Search, Calls, Calls, Arc<Mutex<usize>>) {
    let token_calls = Arc::new(Mutex::new(Vec::new()));
    let retrieval_calls = Arc::new(Mutex::new(Vec::new()));
    let prepared = Arc::new(Mutex::new(0));
    (Tokens { calls: Arc::clone(&token_calls), suspend: token_suspend },
        Search { calls: Arc::clone(&retrieval_calls), documents: Vec::new(), suspend_query: query_suspend, prepared: Arc::clone(&prepared) },
        token_calls, retrieval_calls, prepared)
}

#[test]
fn isolated_utf16_split_query_and_injected_content_stay_exact() {
    let budget = budget(Arc::new(AtomicBool::new(false)));
    let (tokens, search, token_calls, retrieval_calls, _) = dependencies(false, false);
    let old = Text::from_units(&[32, 0xd800, 32, 10, 10, 0xfeff, 10, 10, 0xdc00, 32], &budget).unwrap();
    let chats = vec![PromptMessage::new(Role::User, old), PromptMessage::new(Role::User, text(" ", &budget)),
        PromptMessage::new(Role::User, text("", &budget)), PromptMessage::new(Role::Assistant, text("😀", &budget))];
    let mut stage = Hanurai::new(config(0.0, true, &budget), tokens, search);
    let output = done(stage.run(input(chats, 0.0, 100.0), &budget));
    let prefix = |value: &str, tail: &[u16]| value.encode_utf16().chain(tail.iter().copied()).collect::<Vec<_>>();
    assert_eq!(*retrieval_calls.lock().unwrap(), vec![prefix("search_document: ", &[0xd800]),
        prefix("search_document: ", &[0xdc00]), prefix("search_query: ", &[0xd83d, 0xde00]), prefix("search_query: ", &[32])]);
    assert_eq!(*token_calls.lock().unwrap(), vec![vec![32, 0xd800], vec![32, 0xdc00]]);
    assert_eq!(output.chats[0].content.units().collect::<Vec<_>>(), vec![32, 0xd800, 10, 10, 32, 0xdc00]);
    assert_eq!(output.current_tokens, 6.0);
    assert_eq!(output.chats.len(), 5);
}

#[test]
fn dependency_failures_after_preparation_remain_terminal_and_keep_original_utf16() {
    let budget = budget(Arc::new(AtomicBool::new(false)));
    for failure in 0..3 {
        let (tokens, search, _, _, prepared) = dependencies(false, true);
        let mut stage = Hanurai::new(config(0.0, false, &budget), tokens, search);
        let request = stage.run(input(vec![PromptMessage::new(Role::User, text("recent", &budget))], 0.0, 100.0), &budget).unwrap();
        let Step::NeedsHost(HostRequest::Embed(request)) = request else { panic!("expected unchanged retrieval leaf") };
        assert_eq!(request.model.units().collect::<Vec<_>>(), [0xd800]);
        assert_eq!(request.texts[0].as_unicode(), Some("search_query: recent"));
        let (reply, expected) = match failure {
            0 => (HostReply::Failed { message: Text::from_units(&[0xdc00, 0x61], &budget).unwrap() }, StageError::Host(vec![0xdc00, 0x61])),
            1 => (HostReply::Cancelled, StageError::Cancelled),
            _ => (HostReply::Unsupported(Cow::Borrowed("native-retrieval")), StageError::Unsupported(Cow::Borrowed("native-retrieval"))),
        };
        assert!(matches!(stage.resume(reply, &budget), Err(error) if error == expected));
        assert_eq!(*prepared.lock().unwrap(), 1, "completed dependency work remains completed");
        assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &budget), Err(StageError::Invalid(_))));
        assert!(matches!(stage.run(input(Vec::new(), 0.0, 1.0), &budget), Err(StageError::Invalid(_))));
    }
}

#[test]
fn cancelled_budget_drops_suspended_policy_without_more_dependency_work() {
    let cancel = Arc::new(AtomicBool::new(false));
    let budget = budget(Arc::clone(&cancel));
    let (tokens, search, _, retrieval_calls, prepared) = dependencies(false, true);
    let mut stage = Hanurai::new(config(0.0, false, &budget), tokens, search);
    let request = stage.run(input(vec![PromptMessage::new(Role::User, text("recent", &budget))], 0.0, 100.0), &budget).unwrap();
    assert!(matches!(request, Step::NeedsHost(_)));
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &budget), Err(StageError::Cancelled)));
    assert_eq!(*prepared.lock().unwrap(), 1);
    assert_eq!(retrieval_calls.lock().unwrap().len(), 1);
    cancel.store(false, Ordering::Relaxed);
    assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &budget), Err(StageError::Invalid(_))));
}

#[test]
fn tokenizer_host_failure_is_not_logical_exhaustion_and_success_resumes_trim() {
    let budget = budget(Arc::new(AtomicBool::new(false)));
    for fail in [false, true] {
        let (tokens, search, _, _, prepared) = dependencies(true, false);
        let mut stage = Hanurai::new(config(3.0, false, &budget), tokens, search);
        let request = stage.run(input(vec![PromptMessage::new(Role::User, text("old", &budget))], 5.0, 6.0), &budget).unwrap();
        let Step::NeedsHost(HostRequest::PluginTokenizer { text: requested }) = request else { panic!("tokenizer leaf") };
        assert_eq!(requested.as_unicode(), Some("old"));
        if fail {
            assert!(matches!(stage.resume(HostReply::Failed { message: Text::from_units(&[0xd800], &budget).unwrap() }, &budget),
                Err(StageError::Host(message)) if message == [0xd800]));
        } else {
            let output = done(stage.resume(HostReply::Tokens(vec![0.0, 1.0]), &budget));
            assert!(output.error.is_none());
            assert_eq!(output.current_tokens, 3.0);
            assert_eq!(output.chats.len(), 1);
            assert_eq!(output.chats[0].content.len_units(), 0);
        }
        assert_eq!(*prepared.lock().unwrap(), 1);
    }
}

#[test]
fn floating_reserve_arithmetic_and_logical_exhaustion_match_javascript_numbers() {
    let budget = budget(Arc::new(AtomicBool::new(false)));
    for (reserve, current, limit, expected) in [(f64::NAN, 1.0, 10.0, f64::NAN),
        (f64::INFINITY, 0.0, f64::INFINITY, f64::NAN), (-0.25, -1.5, 10.0, -1.5), (0.0, f64::NAN, 0.0, f64::NAN)] {
        let (tokens, search, _, _, prepared) = dependencies(false, false);
        let mut stage = Hanurai::new(config(reserve, false, &budget), tokens, search);
        let output = done(stage.run(input(Vec::new(), current, limit), &budget));
        assert!(output.error.is_none());
        if expected.is_nan() { assert!(output.current_tokens.is_nan()); }
        else { assert_eq!(output.current_tokens, expected); }
        assert_eq!(output.chats.len(), 1);
        assert_eq!(*prepared.lock().unwrap(), 1, "preparation is not skipped for empty histories");
    }
    let (tokens, search, _, _, _) = dependencies(false, false);
    let mut stage = Hanurai::new(config(f64::INFINITY, false, &budget), tokens, search);
    let output = done(stage.run(input(Vec::new(), 0.0, 10.0), &budget));
    assert_eq!(output.error.unwrap().as_unicode(), Some("context limit\n\nRequired Tokens: Infinity"));
    assert!(output.chats.is_empty());
    assert!(output.effects.is_empty());
}

struct ResourceSearch { documents: Vec<Text>, queries: usize, suspend_at: usize, prepared: Arc<Mutex<usize>> }
impl Retrieval for ResourceSearch {
    fn prepare(&mut self, documents: Vec<Text>, _: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        *self.prepared.lock().unwrap() += 1;
        self.documents = documents;
        Ok(Step::Done(RetrievalOutput::Prepared))
    }
    fn query(&mut self, query: Text, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        self.queries += 1;
        if self.queries == self.suspend_at {
            return Ok(Step::NeedsHost(HostRequest::Embed(EmbedRequest { model: text("resource-probe", budget), texts: vec![query] })));
        }
        Ok(Step::Done(RetrievalOutput::Scored(std::mem::take(&mut self.documents).into_iter()
            .map(|document| ScoredDocument { document: Some(document), score: 1.0 }).collect())))
    }
    fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        Ok(Step::Done(RetrievalOutput::Scored(Vec::new())))
    }
}
fn limited_budget(owned: usize, work: usize) -> Budget {
    Budget::new(Limits { max_owned_bytes: owned, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
        max_values: 100_000, max_depth: 128, max_work: work }, Arc::new(AtomicBool::new(false))).unwrap()
}

#[test]
fn resource_work_budget_bounds_long_surviving_content_comparisons() {
    let initial = budget(Arc::new(AtomicBool::new(false)));
    let mut chats = Vec::new();
    for suffix in b'A'..=b'X' {
        chats.push(PromptMessage::new(Role::User, text(&format!(" {}{}", "x".repeat(128), suffix as char), &initial)));
    }
    for _ in 0..3 { chats.push(PromptMessage::new(Role::User, text("recent", &initial))); }
    let prepared = Arc::new(Mutex::new(0));
    let (tokens, _, calls, _, _) = dependencies(false, false);
    let search = ResourceSearch { documents: Vec::new(), queries: 0, suspend_at: 3, prepared };
    let mut stage = Hanurai::new(config(0.0, false, &initial), tokens, search);
    assert!(matches!(stage.run(input(chats, 0.0, 1000.0), &initial), Ok(Step::NeedsHost(_))));
    // Ranking copies fit this budget, but the triangular set of long matching-prefix comparisons does not.
    let constrained = limited_budget(1 << 20, 16_000);
    assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &constrained),
        Err(StageError::Limit(name)) if name == "work"));
    assert!(calls.lock().unwrap().is_empty(), "surviving candidates do not need tokenization");
}

#[test]
fn resource_owned_budget_covers_split_scratch_and_suspended_score_keys() {
    let mut limited = Vec::new();
    for split in [false, true] {
        for owned in [18_000, 100_000] {
            let initial = budget(Arc::new(AtomicBool::new(false)));
            let chats = vec![PromptMessage::new(Role::User, text(&"x".repeat(4096), &initial)),
                PromptMessage::new(Role::User, text("old query", &initial)),
                PromptMessage::new(Role::User, text("middle query", &initial)),
                PromptMessage::new(Role::User, text("new query", &initial))];
            let prepared = Arc::new(Mutex::new(0));
            let search = ResourceSearch { documents: Vec::new(), queries: 0, suspend_at: 2, prepared: Arc::clone(&prepared) };
            let (tokens, _, _, _, _) = dependencies(false, false);
            let mut stage = Hanurai::new(config(0.0, split, &initial), tokens, search);
            // The low limit fits the prepared text and its transient trim copy, but not another UTF-16 copy.
            let constrained = limited_budget(owned, 1_000_000);
            let result = stage.run(input(chats, 0.0, 1000.0), &constrained);
            if owned == 18_000 {
                limited.push((split, matches!(result, Err(StageError::Limit(name)) if name == "owned-bytes"), *prepared.lock().unwrap()));
            } else {
                assert!(matches!(result, Ok(Step::NeedsHost(_))), "sufficient memory admits split={split}");
                // The retained UTF-8 document plus UTF-16 key need at least three bytes per ASCII unit.
                assert!(constrained.owned_bytes() >= 3 * 4096, "retained key is visible to the owner, split={split}");
            }
        }
    }
    assert_eq!(limited, vec![(false, true, 1), (true, true, 0)], "split scratch is bounded before retrieval; keys before the next query");
}

struct MissingAndLiteral;
impl Retrieval for MissingAndLiteral {
    fn prepare(&mut self, _: Vec<Text>, _: &Budget) -> Result<Step<RetrievalOutput>, StageError> { Ok(Step::Done(RetrievalOutput::Prepared)) }
    fn query(&mut self, _: Text, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        Ok(Step::Done(RetrievalOutput::Scored(vec![
            ScoredDocument { document: None, score: 1.0 },
            ScoredDocument { document: Some(text("undefined", budget)), score: 2.0 },
            ScoredDocument { document: Some(text("search_document: next", budget)), score: 2.5 },
        ])))
    }
    fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<RetrievalOutput>, StageError> { unreachable!() }
}
#[test]
fn missing_document_collides_with_literal_undefined_at_policy_aggregation() {
    let budget = budget(Arc::new(AtomicBool::new(false)));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tokenizer = Tokens { calls: Arc::clone(&calls), suspend: false };
    let mut stage = Hanurai::new(config(0.0, false, &budget), tokenizer, MissingAndLiteral);
    let output = done(stage.run(input(vec![PromptMessage::new(Role::User, text("recent", &budget))], 0.0, 4.0), &budget));
    assert_eq!(output.chats[0].content.as_unicode(), Some(""));
    assert_eq!(output.current_tokens, 3.0);
    assert_eq!(*calls.lock().unwrap(), vec![Vec::<u16>::new(), " next".encode_utf16().collect::<Vec<_>>()]);
}
