use cbs::{compile, evaluate, Budget, Context, Units};
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
fn text(v: &Value) -> Result<Units, String> {
    if let Some(s) = v.as_str() {
        Ok(cbs::units(s))
    } else {
        serde_json::from_value(v.clone()).map_err(|_| "text".into())
    }
}
fn main() {
    let mut programs: HashMap<String, (Vec<cbs::Node>, usize)> = HashMap::new();
    let mut ctx = Context::default();
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
                    let mut p = compile(&source)?;
                    let folded = if req["fold"] == true {
                        cbs::fold_constants(&mut p)
                    } else {
                        0
                    };
                    let size = cbs::retained_bytes(&p);
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
                    ctx = context(&req["context"])?;
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
                    let mut current = if req["context"].is_object() {
                        context(&req["context"])?
                    } else {
                        ctx.clone()
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
                        local = {
                            let mut nodes = compile(&text(&req["text"])?)?;
                            if req["fold"] == true {
                                cbs::fold_constants(&mut nodes);
                            }
                            nodes
                        };
                        &local
                    };
                    let count = req["repeat"].as_u64().unwrap_or(1).min(10_000);
                    let initial = current.clone();
                    let mut out = Vec::new();
                    for _ in 0..count {
                        current = initial.clone();
                        out = evaluate(p, &mut current, &mut budget)?
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
        if writeln!(output, "{}", result).is_err() {
            break;
        }
        if output.flush().is_err() {
            break;
        }
    }
}
