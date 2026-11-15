use super::{compat::copy_units, scalar, units, Budget, Units};
use std::mem::size_of;

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub capture_id: Units,
    pub character_id: Units,
    pub conversation_id: Units,
    pub revision: Option<u64>,
}
#[derive(Clone, Debug)]
pub struct Row {
    pub index: i64,
    pub data: Units,
    pub role: Units,
}
#[derive(Clone, Debug)]
pub struct Limits {
    pub rows: usize,
    pub input_units: usize,
    pub retained_bytes: usize,
    pub dependencies: usize,
}
#[derive(Clone, Debug)]
pub struct PreparedAbsoluteHistory {
    pub length: i64,
    pub rows: Vec<Row>,
    pub selected_character: bool,
    pub condition_chat_role: Option<Units>,
    pub firstmsg: bool,
    pub role: Option<Units>,
    pub provenance: Provenance,
    pub limits: Limits,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Observation {
    pub provenance: Provenance,
    pub count: bool,
    pub rows: Vec<i64>,
}
impl Observation {
    pub fn affected_by(&self, start: i64, end: i64, count_changed: bool) -> bool {
        (self.count && count_changed) || self.rows.iter().any(|&index| start <= index && index < end)
    }
}

pub(super) enum Reply {
    Value(Units),
    Preserve,
    Missing(Units),
}
pub(super) struct State<'a> {
    capture: &'a PreparedAbsoluteHistory,
    pub(super) observation: Observation,
    retained: usize,
}
impl<'a> State<'a> {
    pub(super) fn new(capture: &'a PreparedAbsoluteHistory, index: i64, budget: &mut Budget) -> Result<Self, String> {
        budget.tick()?;
        let limits = &capture.limits;
        if limits.rows == 0 || limits.input_units == 0 || limits.retained_bytes == 0 || limits.dependencies == 0 {
            return Err("invalid-history-limits".into());
        }
        if !(0..=MAX_SAFE_INTEGER).contains(&capture.length) || index.abs_diff(0) > MAX_SAFE_INTEGER as u64 {
            return Err("history-domain".into());
        }
        if capture.rows.len() > limits.rows { return Err("history-input-limit".into()); }
        let mut retained = size_of::<PreparedAbsoluteHistory>().saturating_add(size_of::<Observation>())
            .saturating_add(capture.rows.capacity().saturating_mul(size_of::<Row>()));
        let mut input_units = 0usize;
        let mut account = |value: &Units| -> Result<(), String> {
            budget.tick()?;
            for _ in value.chunks(256) { budget.tick()?; }
            input_units = input_units.saturating_add(value.len());
            retained = retained.saturating_add(value.capacity().saturating_mul(2));
            if input_units > limits.input_units || retained > limits.retained_bytes { return Err("history-input-limit".into()); }
            Ok(())
        };
        let label = &capture.provenance;
        for id in [&label.capture_id, &label.character_id, &label.conversation_id] {
            if id.len() > 256 { return Err("history-domain".into()); }
            account(id)?;
        }
        for value in [&capture.condition_chat_role, &capture.role].into_iter().flatten() { account(value)?; }
        for row in &capture.rows { account(&row.data)?; account(&row.role)?; }
        for (at, row) in capture.rows.iter().enumerate() {
            budget.tick()?;
            if row.index < 0 || row.index >= capture.length { return Err("history-domain".into()); }
            for previous in &capture.rows[..at] {
                budget.tick()?;
                if previous.index == row.index { return Err("history-domain".into()); }
            }
        }
        let provenance = Provenance {
            capture_id: copy_units(&label.capture_id, budget)?,
            character_id: copy_units(&label.character_id, budget)?,
            conversation_id: copy_units(&label.conversation_id, budget)?,
            revision: label.revision,
        };
        retained = retained.saturating_add(provenance.capture_id.capacity().saturating_mul(2))
            .saturating_add(provenance.character_id.capacity().saturating_mul(2))
            .saturating_add(provenance.conversation_id.capacity().saturating_mul(2));
        if retained > limits.retained_bytes { return Err("history-input-limit".into()); }
        Ok(Self { capture, observation: Observation { provenance, count: false, rows: Vec::new() }, retained })
    }
    fn length(&mut self, budget: &mut Budget) -> Result<i64, String> {
        budget.tick()?;
        self.observation.count = true;
        Ok(self.capture.length)
    }
    fn row(&mut self, index: f64, role: bool, budget: &mut Budget) -> Result<Reply, String> {
        let length = self.length(budget)?;
        if !index.is_finite() || index.fract() != 0.0 || index < 0.0 || index >= length as f64 { return Ok(Reply::Preserve); }
        let index = index as i64;
        let mut seen = false;
        for &existing in &self.observation.rows {
            budget.tick()?;
            if existing == index { seen = true; break; }
        }
        if !seen {
            if self.observation.rows.len() >= self.capture.limits.dependencies { return Err("history-dependency-limit".into()); }
            self.observation.rows.try_reserve_exact(1).map_err(|_| "effect-allocation")?;
            if self.retained.saturating_add(self.observation.rows.capacity().saturating_mul(size_of::<i64>())) > self.capture.limits.retained_bytes {
                return Err("history-retained-limit".into());
            }
            self.observation.rows.push(index);
        }
        for row in &self.capture.rows {
            budget.tick()?;
            if row.index == index {
                let value = if role { &row.role } else { &row.data };
                return self.value(value, budget);
            }
        }
        Ok(Reply::Missing(self.copy(&units(&format!("row {index}")), budget)?))
    }
    fn value(&self, value: &[u16], budget: &mut Budget) -> Result<Reply, String> {
        if value.len() > budget.max_output { return Err("output-limit".into()); }
        Ok(Reply::Value(self.copy(value, budget)?))
    }
    fn copy(&self, value: &[u16], budget: &mut Budget) -> Result<Units, String> {
        let bytes = self.retained.saturating_add(self.observation.rows.capacity().saturating_mul(size_of::<i64>()))
            .saturating_add(value.len().saturating_mul(2));
        if bytes > self.capture.limits.retained_bytes { return Err("history-retained-limit".into()); }
        copy_units(value, budget)
    }
    pub(super) fn callback(&mut self, name: &str, args: &[Units], index: i64, budget: &mut Budget) -> Result<Reply, String> {
        match name {
            "lastmessage" | "lastmessageid" | "lastmessageindex" => {
                if !self.capture.selected_character { return Ok(Reply::Value(Vec::new())); }
                let last = self.length(budget)? - 1;
                if name == "lastmessage" { self.row(last as f64, false, budget) }
                else { self.value(&units(&last.to_string()), budget) }
            }
            "previouschatlog" => {
                let index = scalar::number(args.first(), Some(&mut *budget))?;
                match self.row(index, false, budget)? {
                    Reply::Preserve => self.value(&units("Out of range"), budget),
                    reply => Ok(reply),
                }
            }
            "role" => {
                if let Some(role) = self.capture.condition_chat_role.as_ref().filter(|role| !role.is_empty()) {
                    return self.value(role, budget);
                }
                if self.capture.firstmsg { return self.value(&units("char"), budget); }
                if index != -1 { return self.row(index as f64, true, budget); }
                self.value(self.capture.role.as_deref().unwrap_or(&units("null")), budget)
            }
            _ => unreachable!(),
        }
    }
}
