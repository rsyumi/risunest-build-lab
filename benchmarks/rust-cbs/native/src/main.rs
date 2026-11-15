use cbs::{parser, Budget, Context, Units};
use risunest_cbs_experimental::{cbs, relaxed};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::time::Instant;
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireContext {
    #[serde(default)]
    variables: Vec<(Units, Units)>,
    #[serde(default)]
    globals: Vec<(Units, Units)>,
    #[serde(default)]
    index: i64,
    #[serde(default)]
    user: Units,
    #[serde(default)]
    seed: u32,
}
impl From<WireContext> for Context {
    fn from(w: WireContext) -> Self {
        Self {
            variables: w.variables.into_iter().collect(),
            globals: w.globals.into_iter().collect(),
            index: w.index,
            user: w.user,
            state: w.seed,
            ..Default::default()
        }
    }
}
fn context(v: &Value) -> Result<Context, String> {
    let w: WireContext = serde_json::from_value(v.clone()).map_err(|_| "context")?;
    if w.variables.len() + w.globals.len() > 4096
        || w.variables
            .iter()
            .chain(&w.globals)
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
            + w.user.len()
            > 1_048_576
    {
        return Err("context-limit".into());
    }
    Ok(w.into())
}
// The invocation's `functions` map, which the ordinary stream engine reads for `call::`.
fn invocation(v: &Value) -> Result<(Context, HashMap<Units, Units>), String> {
    let functions: Vec<(Units, Units)> = match v.get("functions") {
        None | Some(Value::Null) => Vec::new(),
        Some(f) => serde_json::from_value(f.clone()).map_err(|_| "context")?,
    };
    if functions.len() > 256 || functions.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() > 1_048_576 {
        return Err("context-limit".into());
    }
    Ok((context(v)?, functions.into_iter().collect()))
}
fn options(req: &Value) -> parser::Options {
    parser::Options {
        displaying: req["visualize"] == true,
        tailored_case: req["tailoredCase"] == true,
        // The oracle page's Chromium shares Rust's case tables.
        case_tables: parser::CaseTables::Native,
        tokenize_accurate: req["tokenizeAccurate"] == true,
    }
}
fn text(v: &Value) -> Result<Units, String> {
    if let Some(s) = v.as_str() {
        Ok(cbs::units(s))
    } else {
        serde_json::from_value(v.clone()).map_err(|_| "text".into())
    }
}
fn hash_number(value: Option<&Value>) -> Result<f64, String> {
    match value {
        None => Ok(f64::NAN),
        Some(Value::String(value)) => match value.as_str() {
            "NaN" => Ok(f64::NAN),
            "Infinity" => Ok(f64::INFINITY),
            "-Infinity" => Ok(f64::NEG_INFINITY),
            _ => Err("hash-number".into()),
        },
        Some(value) => value.as_f64().ok_or("hash-number".into()),
    }
}

fn response_json(value: &Value) -> String {
    // The line reader also treats Unicode line and paragraph separators as boundaries.
    value.to_string().replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029")
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum WireCell {
    Undefined,
    Text { units: Units },
    Error { message: Units },
    HistoryError { access: Units },
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireEffectLimits {
    effects: Option<usize>,
    reads: Option<usize>,
    retained_bytes: Option<usize>,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireEffectContext {
    #[serde(default)]
    variables: Vec<(Units, Units)>,
    #[serde(default)]
    globals: Vec<(Units, Units)>,
    #[serde(default)]
    index: i64,
    #[serde(default)]
    user: Units,
    #[serde(default)]
    seed: u32,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireHistoryProvenance {
    capture_id: Units,
    character_id: Units,
    conversation_id: Units,
    revision: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireHistoryRow {
    index: i64,
    data: Units,
    role: Units,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireHistoryLimits {
    rows: usize,
    input_units: usize,
    retained_bytes: usize,
    dependencies: usize,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireHistory {
    mode: String,
    length: i64,
    rows: Vec<WireHistoryRow>,
    selected_character: bool,
    condition_chat_role: Option<Units>,
    #[serde(default)]
    firstmsg: bool,
    role: Option<Units>,
    provenance: WireHistoryProvenance,
    limits: WireHistoryLimits,
}
impl WireHistory {
    fn capture(self) -> Result<cbs::history::PreparedAbsoluteHistory, String> {
        use cbs::history::{Limits, PreparedAbsoluteHistory, Provenance, Row};
        if self.mode != "preparedAbsolute" { return Err("history-domain".into()); }
        let revision = self.provenance.revision.map(|value| {
            let n = value.parse::<u64>().map_err(|_| "history-domain")?;
            if n.to_string() != value { return Err("history-domain"); }
            Ok(n)
        }).transpose()?;
        Ok(PreparedAbsoluteHistory {
            length: self.length, rows: self.rows.into_iter().map(|row| Row { index: row.index, data: row.data, role: row.role }).collect(),
            selected_character: self.selected_character, condition_chat_role: self.condition_chat_role,
            firstmsg: self.firstmsg, role: self.role,
            provenance: Provenance { capture_id: self.provenance.capture_id, character_id: self.provenance.character_id,
                conversation_id: self.provenance.conversation_id, revision },
            limits: Limits { rows: self.limits.rows, input_units: self.limits.input_units,
                retained_bytes: self.limits.retained_bytes, dependencies: self.limits.dependencies },
        })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireProgramLimits {
    functions: usize,
    frames: usize,
    source_units: usize,
    expanded_units: usize,
    retained_bytes: usize,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireProgram {
    mode: String,
    limits: WireProgramLimits,
}
impl WireProgram {
    fn limits(self) -> Result<cbs::program::Limits, String> {
        if self.mode != "localFunctions" { return Err("program-domain".into()); }
        Ok(cbs::program::Limits { functions: self.limits.functions, frames: self.limits.frames,
            source_units: self.limits.source_units, expanded_units: self.limits.expanded_units,
            retained_bytes: self.limits.retained_bytes })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireEffectInput {
    mode: String,
    #[serde(default)]
    run_var: bool,
    #[serde(default)]
    rm_var: bool,
    #[serde(default)]
    variables: Vec<(Units, WireCell)>,
    #[serde(default)]
    context: WireEffectContext,
    #[serde(default)]
    limits: WireEffectLimits,
    history: Option<WireHistory>,
    program: Option<WireProgram>,
}
fn effect_value(value: &cbs::compat::Value) -> Value {
    match value {
        cbs::compat::Value::Undefined => json!({"kind":"undefined"}),
        cbs::compat::Value::Text(value) => json!({"kind":"text","units":value}),
    }
}
fn effect_error(error: &cbs::compat::ReadError) -> Value {
    let mut value = json!({"name":error.name(),"message":error.message()});
    if let cbs::compat::ReadError::HistoryAccess(access) = error { value["access"] = json!(access); }
    value
}
fn evaluate_effects(req: &Value) -> Result<Value, String> {
    use cbs::compat::{self, Binding, Cell, Input, Outcome, ReadError};
    let wire: WireEffectInput = serde_json::from_value(req["input"].clone()).map_err(|_| "binding-domain")?;
    let binding = match wire.mode.as_str() {
        "mutableMap" => Binding::MutableMap,
        "snapshotOnly" => Binding::SnapshotOnly,
        _ => return Err("binding-domain".into()),
    };
    if !wire.context.variables.is_empty() { return Err("binding-domain".into()); }
    let mut used = wire.context.user.len() + wire.context.globals.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>();
    if used > 1_048_576 || wire.variables.len() + wire.context.globals.len() > 4096 { return Err("context-limit".into()); }
    let context = Context { globals: wire.context.globals.into_iter().collect(), index: wire.context.index,
        user: wire.context.user, state: wire.context.seed, ..Context::default() };
    let mut variables = HashMap::new();
    for (key, cell) in wire.variables {
        used = used.saturating_add(key.len());
        let cell = match cell {
            WireCell::Undefined => Cell::Value(compat::Value::Undefined),
            WireCell::Text { units } => {
                used = used.saturating_add(units.len());
                Cell::Value(compat::Value::Text(units))
            }
            WireCell::Error { message } => {
                used = used.saturating_add(message.len());
                Cell::Error(ReadError::Ordinary(message))
            }
            WireCell::HistoryError { access } => {
                used = used.saturating_add(access.len());
                Cell::Error(ReadError::HistoryAccess(access))
            }
        };
        if used > 1_048_576 { return Err("context-limit".into()); }
        if variables.insert(key, cell).is_some() { return Err("duplicate-variable-key".into()); }
    }
    let mut limits = compat::Limits::default();
    if let Some(n) = wire.limits.effects { limits.effects = n; }
    if let Some(n) = wire.limits.reads { limits.reads = n; }
    if let Some(n) = wire.limits.retained_bytes { limits.retained_bytes = n; }
    let history = wire.history.map(WireHistory::capture).transpose()?;
    let program = wire.program.map(WireProgram::limits).transpose()?;
    let input = Input { binding, run_var: wire.run_var, rm_var: wire.rm_var, variables, context, limits, history, program };
    let source = text(&req["text"])?;
    let mut program = if input.program.is_some() { compat::compile_program(&source, input.history.is_some())? }
        else if input.history.is_some() { compat::compile_with_history(&source)? } else { compat::compile(&source)? };
    if req["fold"] == true { program.fold_constants(); }
    let mut budget = Budget::default();
    if let Some(n) = req["budget"].as_u64() { budget.remaining = usize::try_from(n).map_err(|_| "budget")?; }
    if let Some(n) = req["maxOutput"].as_u64() { budget.max_output = usize::try_from(n).map_err(|_| "output-limit")?; }
    if req["cancelled"] == true { budget.cancel.store(true, std::sync::atomic::Ordering::Relaxed); }
    Ok(match compat::evaluate(&program, &input, &mut budget) {
        Outcome::Rejected(reason) => json!({"status":"rejected","reason":reason}),
        Outcome::Interrupted(reason) => json!({"status":"interrupted","reason":reason}),
        Outcome::Evaluated(value) => {
            let result = match &value.result {
                Ok(output) => json!({"output":output}),
                Err(error) => json!({"error":effect_error(error)}),
            };
            let mut response = json!({"status":"evaluated","result":result,
                "effects":value.effects.iter().map(|e| json!({"sequence":e.sequence,"key":effect_value(&e.key),"value":effect_value(&e.value)})).collect::<Vec<_>>(),
                "reads":value.reads.iter().map(|r| json!({"sequence":r.sequence,"key":effect_value(&r.key)})).collect::<Vec<_>>(),
                "dependencies":{"keys":value.dependencies.keys.iter().map(effect_value).collect::<Vec<_>>(),"untracked":value.dependencies.untracked},
                "overlay":value.overlay.iter().map(|(key,value)| json!([key,effect_value(value)])).collect::<Vec<_>>(),
                "draws":value.draws,"rngState":value.rng_state});
            if let Some(history) = value.history {
                let label = history.provenance;
                let mut provenance = json!({"captureId":label.capture_id,"characterId":label.character_id,"conversationId":label.conversation_id});
                if let Some(revision) = label.revision { provenance["revision"] = json!(revision.to_string()); }
                response["history"] = json!({"provenance":provenance,"count":history.count,"rows":history.rows});
            }
            response
        }
    })
}

fn main() {
    let mut programs: HashMap<String, (parser::Prepared, usize)> = HashMap::new();
    let mut ctx = Context::default();
    let mut ctx_functions: HashMap<Units, Units> = HashMap::new();
    let mut relaxed_programs: HashMap<String, (relaxed::Program, u64)> = HashMap::new();
    let mut revision = 0u64;
    let mut bytes = 0usize;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let start = Instant::now();
        let result = (|| -> Result<Value, String> {
            let req: Value = serde_json::from_str(&line).map_err(|_| "request")?;
            let op = req["op"].as_str().ok_or("op")?;
            match op {
                "evaluateEffects" => Ok(match evaluate_effects(&req) {
                    Ok(value) => value,
                    Err(reason) => json!({"status":"rejected","reason":reason}),
                }),
                "hash-core" => {
                    let mut budget = Budget::default();
                    let value = cbs::pick_hash_rand(hash_number(req.get("cid"))?, &text(&req["word"])?, &mut budget)?;
                    let mut sequence = Vec::new();
                    if let Some(seeds) = req["seeds"].as_array() {
                        if seeds.len() != 4 {
                            return Err("hash-seeds".into());
                        }
                        let seeds = [
                            hash_number(seeds.first())?, hash_number(seeds.get(1))?,
                            hash_number(seeds.get(2))?, hash_number(seeds.get(3))?,
                        ];
                        let count = req["count"].as_u64().unwrap_or(8);
                        if count > 64 {
                            return Err("hash-sequence-limit".into());
                        }
                        let mut random = cbs::Sfc32::new(seeds);
                        for _ in 0..count {
                            sequence.push(random.next_value());
                        }
                    }
                    Ok(json!({"value":value,"sequence":sequence}))
                }
                "relax-register" => {
                    let id = req["id"].as_str().ok_or("id")?.to_owned();
                    relaxed_programs.remove(&id);
                    let current = if req["context"].is_object() {
                        context(&req["context"])?
                    } else {
                        ctx.clone()
                    };
                    let p = relaxed::prepare(&text(&req["text"])?, &current)?;
                    let used: usize = relaxed_programs
                        .values()
                        .map(|(p, _)| p.retained_bytes)
                        .sum();
                    if relaxed_programs.len() >= 64 || used + p.retained_bytes > 8_388_608 {
                        return Err("rng-resident-bound".into());
                    }
                    let response = json!({"nodes":p.nodes,"leaves":p.leaves,"retainedBytes":p.retained_bytes,"revision":revision,"entropyWords":0});
                    relaxed_programs.insert(id, (p, revision));
                    Ok(response)
                }
                "relax-eval" => {
                    let result = (|| -> Result<Value, String> {
                        if req["cancelled"] == true {
                            return Err("cancelled".into());
                        }
                        let id = req["id"].as_str().ok_or("id")?;
                        let (p, prepared_revision) =
                            relaxed_programs.get(id).ok_or("missing-program")?;
                        if *prepared_revision != revision
                            || req["revision"].as_u64().is_some_and(|n| n != revision)
                        {
                            return Err("stale-context".into());
                        }
                        let tape: Option<Vec<f64>> = if req["tape"].is_array() {
                            Some(
                                serde_json::from_value(req["tape"].clone())
                                    .map_err(|_| "rng-tape-domain")?,
                            )
                        } else {
                            None
                        };
                        let sample = relaxed::sample(p, tape.as_deref())?;
                        let output = match String::from_utf16(&sample.output) {
                            Ok(s) => json!(s),
                            Err(_) => json!(sample.output),
                        };
                        Ok(
                            json!({"output":output,"choices":sample.choices,"entropyWords":sample.entropy_words,"revision":revision}),
                        )
                    })();
                    Ok(match result {
                        Ok(v) => v,
                        Err(error) => json!({"abort":error}),
                    })
                }
                "register" => {
                    let id = req["id"].as_str().ok_or("id")?.to_owned();
                    let source = text(&req["text"])?;
                    let p = parser::prepare(&source, req["fold"] == true, options(&req));
                    let folded = p.folded();
                    let size = p.retained_bytes();
                    let old = programs.get(&id).map(|(_, n)| *n).unwrap_or(0);
                    if programs.len() >= 64 && !programs.contains_key(&id)
                        || bytes - old + size > 8_388_608
                    {
                        return Err("resident-limit".into());
                    }
                    programs.insert(id, (p, size));
                    bytes = bytes - old + size;
                    Ok(json!({"programs":programs.len(),"retainedBytes":bytes,"folded":folded}))
                }
                "context" => {
                    (ctx, ctx_functions) = invocation(&req["context"])?;
                    relaxed_programs.clear();
                    revision += 1;
                    Ok(json!({"revision":revision}))
                }
                "eval" | "evaluate" => {
                    if let Some(expected) = req["revision"].as_u64() {
                        if expected != revision {
                            return Err("stale-context".into());
                        }
                    }
                    let (mut current, functions) = if req["context"].is_object() {
                        invocation(&req["context"])?
                    } else {
                        (ctx.clone(), ctx_functions.clone())
                    };
                    current.temp.clear();
                    current.draws = 0;
                    let mut budget = Budget::default();
                    if let Some(n) = req["budget"].as_u64() {
                        budget.remaining = n as usize
                    }
                    if req["cancelled"] == true {
                        budget
                            .cancel
                            .store(true, std::sync::atomic::Ordering::Relaxed)
                    }
                    let local;
                    let p = if let Some(id) = req["id"].as_str() {
                        &programs.get(id).ok_or("missing-program")?.0
                    } else {
                        local = parser::prepare(&text(&req["text"])?, req["fold"] == true, options(&req));
                        &local
                    };
                    let count = req["repeat"].as_u64().unwrap_or(1).min(10_000);
                    let initial = current.clone();
                    let mut out = Vec::new();
                    for _ in 0..count {
                        current = initial.clone();
                        out = parser::run(p, &mut current, &functions, &mut budget)?
                    }
                    let encoded = match String::from_utf16(&out) {
                        Ok(s) => json!(s),
                        Err(_) => json!(out),
                    };
                    Ok(
                        json!({"output":encoded,"draws":current.draws,"rngState":current.state,"revision":revision,"retainedBytes":bytes,"programs":programs.len()}),
                    )
                }
                "release" => {
                    programs.clear();
                    relaxed_programs.clear();
                    programs.shrink_to_fit();
                    ctx = Context::default();
                    ctx_functions.clear();
                    bytes = 0;
                    revision += 1;
                    Ok(json!({"programs":0,"retainedBytes":0,"variables":0,"revision":revision}))
                }
                "stats" => Ok(
                    json!({"programs":programs.len(),"retainedBytes":bytes,"variables":ctx.variables.len(),"revision":revision}),
                ),
                _ => Err("op".into()),
            }
        })();
        let mut result = match result {
            Ok(v) => v,
            Err(e) => json!({"fallback":e}),
        };
        result["coreMs"] = json!(start.elapsed().as_secs_f64() * 1000.0);
        if writeln!(output, "{}", response_json(&result)).is_err() {
            break;
        }
        if output.flush().is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_line_separators_survive_jsonl_framing() {
        let value = json!({"output": "\u{2028}\u{2029}"});
        let wire = response_json(&value);
        assert_eq!(wire, r#"{"output":"\u2028\u2029"}"#);
        assert_eq!(serde_json::from_str::<Value>(&wire).unwrap(), value);
        let value = json!({"output": "\r\n\0\u{85}\u{feff}😀"});
        assert_eq!(serde_json::from_str::<Value>(&response_json(&value)).unwrap(), value);
    }

    #[test]
    fn effect_wire_retains_undefined_and_partial_errors_without_output() {
        let request = json!({"text":"{{setvar}}{{getvar::fault}}{{setvar::later::no}}",
            "input":{"mode":"mutableMap","runVar":true,
                "variables":[[cbs::units("fault"),{"kind":"historyError","access":[0xd800]}]]}});
        let result = evaluate_effects(&request).unwrap();
        assert_eq!(result["status"], "evaluated");
        assert!(result["result"].get("output").is_none());
        assert_eq!(result["result"]["error"]["access"], json!([0xd800]));
        assert_eq!(result["effects"], json!([{"sequence":0,"key":{"kind":"undefined"},"value":{"kind":"undefined"}}]));
        assert_eq!(result["overlay"], json!([[cbs::units("undefined"),{"kind":"undefined"}]]));
        assert_eq!(serde_json::from_str::<Value>(&response_json(&result)).unwrap(), result);
        let rejected = json!({"text":"{{setvar::x::1}}", "input":{"mode":"snapshotOnly","runVar":true,"setChatVar":"custom"}});
        assert_eq!(evaluate_effects(&rejected).unwrap_err(), "binding-domain");
        for context in [json!({"chatVariables":{"x":"9"}}), json!({"getChatVar":"custom"}),
            json!({"setChatVar":"custom"}), json!({"chatVariables":{"x":"9"},"getChatVar":"custom"})] {
            let rejected = json!({"text":"{{setvar::x::1}}", "input":{"mode":"mutableMap","context":context}});
            assert_eq!(evaluate_effects(&rejected).unwrap_err(), "binding-domain");
        }
    }

    #[test]
    fn prepared_history_wire_keeps_exact_labels_and_rejects_nested_bindings() {
        let history = json!({"mode":"preparedAbsolute","length":10,
            "rows":[{"index":3,"data":[0xd800],"role":cbs::units("user")}],"selectedCharacter":true,
            "provenance":{"captureId":[0xdc00],"characterId":[],"conversationId":[],"revision":"18446744073709551615"},
            "limits":{"rows":10,"inputUnits":1000,"retainedBytes":10000,"dependencies":10}});
        let request = json!({"text":"{{setvar}}{{previouschatlog::3}}{{lastmessage}}",
            "input":{"mode":"mutableMap","runVar":true,"history":history}});
        let result = evaluate_effects(&request).unwrap();
        assert_eq!(result["status"], "evaluated");
        assert_eq!(result["result"]["error"]["access"], json!(cbs::units("row 9")));
        assert!(result["result"].get("output").is_none());
        assert_eq!(result["effects"], json!([{"sequence":0,"key":{"kind":"undefined"},"value":{"kind":"undefined"}}]));
        assert_eq!(result["history"], json!({"provenance":history["provenance"],"count":true,"rows":[3,9]}));
        assert_eq!(serde_json::from_str::<Value>(&response_json(&result)).unwrap(), result);
        for path in [vec!["historyOffset"], vec!["row"], vec!["provenance", "ownerPermit"], vec!["limits", "custom"]] {
            let mut invalid = request.clone();
            let mut target = &mut invalid["input"]["history"];
            for key in &path[..path.len()-1] { target = &mut target[*key]; }
            target[path[path.len()-1]] = json!("custom");
            assert_eq!(evaluate_effects(&invalid).unwrap_err(), "binding-domain");
        }
        for revision in [json!(9007199254740992u64), json!("01"), json!("18446744073709551616")] {
            let mut invalid = request.clone(); invalid["input"]["history"]["provenance"]["revision"] = revision;
            assert!(evaluate_effects(&invalid).is_err());
        }
        let mut invalid = request.clone(); invalid["input"]["history"]["rows"][0]["reader"] = json!("custom");
        assert_eq!(evaluate_effects(&invalid).unwrap_err(), "binding-domain");
        let mut invalid = request.clone(); invalid["input"]["history"]["mode"] = json!("live");
        assert_eq!(evaluate_effects(&invalid).unwrap_err(), "history-domain");
    }

    #[test]
    fn local_function_wire_is_opted_in_and_rejects_external_invocation_state() {
        let program = json!({"mode":"localFunctions","limits":{"functions":16,"frames":24,
            "sourceUnits":4096,"expandedUnits":16384,"retainedBytes":65536}});
        let request = json!({"text":"{{#func f}}before{{setvar}}{{return::R}}after{{/func}}A{{call::f}}B",
            "input":{"mode":"mutableMap","runVar":true,"program":program}});
        for fold in [false, true] {
            let mut request = request.clone(); request["fold"] = json!(fold);
            let result = evaluate_effects(&request).unwrap();
            assert_eq!(result["status"], "evaluated");
            assert_eq!(result["result"]["output"], json!(cbs::units("ARB")));
            assert_eq!(result["effects"], json!([{"sequence":0,"key":{"kind":"undefined"},"value":{"kind":"undefined"}}]));
        }
        for path in [vec!["functions"], vec!["callStack"], vec!["limits", "custom"]] {
            let mut invalid = request.clone();
            let mut target = &mut invalid["input"]["program"];
            for key in &path[..path.len()-1] { target = &mut target[*key]; }
            target[path[path.len()-1]] = json!("external");
            assert_eq!(evaluate_effects(&invalid).unwrap_err(), "binding-domain");
        }
        let mut invalid = request.clone(); invalid["input"]["program"]["mode"] = json!("liveFunctions");
        assert_eq!(evaluate_effects(&invalid).unwrap_err(), "program-domain");
        let mut invalid = request.clone(); invalid["input"]["program"]["limits"]["frames"] = json!(0);
        assert_eq!(evaluate_effects(&invalid).unwrap()["reason"], "invalid-program-limits");
        let mut omitted = request.clone(); omitted["input"].as_object_mut().unwrap().remove("program");
        assert_eq!(evaluate_effects(&omitted).unwrap_err(), "block-form");
    }
}
