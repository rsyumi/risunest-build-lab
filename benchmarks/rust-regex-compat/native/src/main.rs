use regex_compat_experimental::{CompiledPlan, ExecutionControl, Failure, FailureKind, Limits, RuleSpec};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io::{self, BufRead, Read, Write}, sync::atomic::AtomicBool, time::{Duration, Instant}};

const MAX_FRAME_BYTES: u64 = 16 * 1024 * 1024;
const ROUTE: &str = "nativeRustRegress";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule { pattern: Vec<u16>, flags: String, replacement: Vec<u16> }
impl From<Rule> for RuleSpec {
    fn from(value: Rule) -> Self { Self { pattern: value.pattern, flags: value.flags, replacement: value.replacement } }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Bounds { rules: usize, pattern_units: usize, source_units: usize, input_units: usize, output_units: usize, matches: usize }
impl Default for Bounds {
    fn default() -> Self {
        let l = Limits::default();
        Self { rules: l.rules, pattern_units: l.pattern_units, source_units: l.source_units, input_units: l.input_units, output_units: l.output_units, matches: l.matches }
    }
}
impl From<Bounds> for Limits {
    fn from(v: Bounds) -> Self { Self { rules: v.rules, pattern_units: v.pattern_units, source_units: v.source_units, input_units: v.input_units, output_units: v.output_units, matches: v.matches } }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct MatchBounds { work_units: u64, allocation_bytes: usize, lookaround_depth: usize }
impl Default for MatchBounds {
    fn default() -> Self { Self { work_units: 10_000_000, allocation_bytes: 64 * 1024 * 1024, lookaround_depth: 64 } }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    Prepare { rules: Vec<Rule>, input: Vec<u16>, #[serde(default)] limits: Bounds },
    Execute { generation: u64, #[serde(default)] cancelled: bool, deadline_ms: Option<u64> },
    Release { generation: u64 },
    Stats,
    Probe { rule: Rule, input: Vec<u16> },
    ControlledProbe { rule: Rule, input: Vec<u16>, #[serde(default)] start: usize,
        #[serde(default)] limits: MatchBounds, #[serde(default)] cancelled: bool, deadline_ms: Option<u64> },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope { request_id: u64, request: Value }
struct Session { plan: CompiledPlan, input: Vec<u16>, generation: u64 }
fn failed(error: Failure) -> Value {
    let status = match error.kind {
        FailureKind::Unsupported => "unsupported", FailureKind::EngineRejected => "engineRejected",
        FailureKind::ResourceLimit => "resourceLimit", FailureKind::Cancelled => "cancelled",
        FailureKind::DeadlineExceeded => "deadlineExceeded",
    };
    json!({ "status": status, "code": error.code, "ruleIndex": error.rule_index })
}
fn dispatch(request: Request, session: &mut Option<Session>, next_generation: &mut u64) -> Value {
    match request {
        Request::Prepare { rules, input, limits } => {
            let limits: Limits = limits.into();
            if input.len() > limits.input_units { return json!({ "status": "resourceLimit", "code": "input_units" }); }
            let rules: Vec<RuleSpec> = rules.into_iter().map(Into::into).collect();
            match CompiledPlan::compile(&rules, limits, &ExecutionControl::default()) {
                Ok(plan) => {
                    let Some(generation) = next_generation.checked_add(1) else { return json!({ "status": "resourceLimit", "code": "generation" }); };
                    *next_generation = generation;
                    let count = plan.rule_count();
                    *session = Some(Session { plan, input, generation });
                    json!({ "status": "prepared", "generation": generation, "rules": count })
                }
                Err(error) => failed(error),
            }
        }
        Request::Execute { generation, cancelled, deadline_ms } => {
            let Some(session) = session else { return json!({ "status": "notPrepared" }); };
            if session.generation != generation { return json!({ "status": "staleGeneration" }); }
            let cancel = AtomicBool::new(cancelled);
            let control = ExecutionControl { cancelled: Some(&cancel), deadline: deadline_ms.and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms))) };
            match session.plan.execute(&session.input, &control) {
                Ok(result) => json!({ "status": "nativeSuccess", "output": result.output, "matches": result.matches, "generation": generation }),
                Err(error) => failed(error),
            }
        }
        Request::Release { generation } => {
            let Some(current) = session else { return json!({ "status": "notPrepared" }); };
            if current.generation != generation { return json!({ "status": "staleGeneration" }); }
            *session = None;
            json!({ "status": "released" })
        }
        Request::Probe { rule, input } => {
            if rule.pattern.len() > 4096 || input.len() > 4096 || !["g", "gu"].contains(&rule.flags.as_str()) {
                return json!({ "status": "invalidRequest" });
            }
            let flags = regress::Flags::from(rule.flags.as_str());
            let points: Vec<u32> = if flags.unicode {
                char::decode_utf16(rule.pattern.iter().copied()).map(|point| point.map(u32::from).unwrap_or_else(|error| u32::from(error.unpaired_surrogate()))).collect()
            } else { rule.pattern.iter().copied().map(u32::from).collect() };
            match regress::Regex::from_unicode(points.into_iter(), flags) {
                Ok(regex) => {
                    let found = if flags.unicode { regex.find_from_utf16(&input, 0).next() } else { regex.find_from_ucs2(&input, 0).next() };
                    json!({ "status": "probeSuccess", "route": "nativeRustRegressRawProbe", "range": found.map(|m| [m.start(), m.end()]) })
                }
                Err(_) => json!({ "status": "engineRejected", "route": "nativeRustRegressRawProbe" }),
            }
        }
        Request::ControlledProbe { rule, input, start, limits, cancelled, deadline_ms } => {
            const CONTROLLED_ROUTE: &str = "nativeRustRegressControlledFirstMatch";
            // Verification inputs and bridge allocation are outside the matching budget.
            if rule.pattern.len() > 4096 || input.len() > 4096
                || !["g", "gu", "gui", "gm", "gmu", "gs", "gsu"].contains(&rule.flags.as_str()) {
                return json!({ "status": "invalidRequest", "route": CONTROLLED_ROUTE, "constructorControlled": false });
            }
            let flags = regress::Flags::from(rule.flags.as_str());
            let points: Vec<u32> = if flags.unicode {
                char::decode_utf16(rule.pattern.iter().copied()).map(|point| point.map(u32::from).unwrap_or_else(|error| u32::from(error.unpaired_surrogate()))).collect()
            } else { rule.pattern.iter().copied().map(u32::from).collect() };
            let regex = match regress::Regex::from_unicode(points.into_iter(), flags) {
                Ok(regex) => regex,
                Err(_) => return json!({ "status": "engineRejected", "route": CONTROLLED_ROUTE, "constructorControlled": false }),
            };
            let cancel = AtomicBool::new(cancelled);
            let deadline = deadline_ms.and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms)));
            let mut budget = regress::MatchBudget::new(regress::MatchLimits {
                work_units: limits.work_units, allocation_bytes: limits.allocation_bytes,
                lookaround_depth: limits.lookaround_depth,
            }, Some(&cancel), deadline);
            let outcome = if flags.unicode { regex.find_first_from_utf16_controlled(&input, start, &mut budget) }
                else { regex.find_first_from_ucs2_controlled(&input, start, &mut budget) };
            let mut response = match outcome {
                Ok(found) => json!({ "status": "controlledSuccess",
                    "range": found.as_ref().map(|m| [m.start(), m.end()]),
                    "captures": found.as_ref().map(|m| m.captures().iter().map(|r| r.as_ref().map(|r| [r.start, r.end])).collect::<Vec<_>>()),
                    "captureNames": found.as_ref().map(|m| m.capture_names().collect::<Vec<_>>()),
                }),
                Err(error) => json!({ "status": "matchAborted", "reason": format!("{error:?}") }),
            };
            let usage = budget.usage();
            response["route"] = json!(CONTROLLED_ROUTE);
            response["constructorControlled"] = json!(false);
            response["usage"] = json!({ "workUnits": usage.work_units,
                "chargedAllocationBytes": usage.charged_allocation_bytes,
                "requestedBytes": usage.requested_bytes, "observedBytes": usage.observed_bytes,
                "observationOverflow": usage.requested_bytes.is_none() || usage.observed_bytes.is_none(),
                "lastReservation": usage.last_reservation.map(|r| json!({ "requestedBytes": r.requested_bytes, "observedBytes": r.observed_bytes, "excessBytes": r.excess_bytes })),
                "activeLookaroundDepth": usage.active_lookaround_depth,
                "maximumLookaroundDepth": usage.maximum_lookaround_depth });
            response
        }
        Request::Stats => json!({ "status": "stats", "plans": usize::from(session.is_some()),
            "rules": session.as_ref().map_or(0, |s| s.plan.rule_count()),
            "sourceUnits": session.as_ref().map_or(0, |s| s.plan.source_units()),
            "inputUnits": session.as_ref().map_or(0, |s| s.input.len()) }),
    }
}
fn main() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    let mut session = None;
    let mut next_generation = 0;
    loop {
        let mut line = Vec::new();
        if (&mut input).take(MAX_FRAME_BYTES + 1).read_until(b'\n', &mut line)? == 0 { break; }
        let too_large = line.len() as u64 > MAX_FRAME_BYTES;
        let (request_id, mut response) = if too_large { (None, json!({ "status": "resourceLimit", "code": "frame_bytes" })) }
            else { match serde_json::from_slice::<Envelope>(&line) {
                Ok(envelope) => (Some(envelope.request_id), match serde_json::from_value::<Request>(envelope.request) {
                    Ok(request) => dispatch(request, &mut session, &mut next_generation),
                    Err(_) => json!({ "status": "invalidRequest" }),
                }),
                Err(_) => (None, json!({ "status": "invalidRequest" })),
            } };
        response["requestId"] = json!(request_id);
        if response.get("route").is_none() { response["route"] = json!(ROUTE); }
        writeln!(output, "{response}")?;
        output.flush()?;
        if too_large { break; }
    }
    Ok(())
}
