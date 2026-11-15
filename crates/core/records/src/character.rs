//! Characters.

use crate::root::Fields;
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Text, Value};

/// A stored character record with its fields as `characterFormatUpdate` leaves them.
#[derive(Debug, Clone, Copy)]
pub struct Character<'a>(Fields<'a>);

impl<'a> Character<'a> {
    pub fn new(value: &'a Value) -> Result<Self, StageError> { Ok(Self(Fields::new(value, "character")?)) }
    pub fn fields(&self) -> Fields<'a> { self.0 }
    pub fn value(&self) -> &'a Value { self.0.value() }
    pub fn id(&self, budget: &Budget) -> Result<Option<&'a Text>, StageError> { self.0.text("chaId", budget) }
    /// `type`; a group chat stores `group`.
    pub fn is_group(&self, budget: &Budget) -> Result<bool, StageError> {
        Ok(crate::root::is_text(self.0.get("type", budget)?, "group"))
    }
    pub fn name(&self, budget: &Budget) -> Result<&'a Text, StageError> { self.0.required_text("name", budget) }
    /// The name `{{char}}` reads: `nickname || name`.
    pub fn display_name(&self, budget: &Budget) -> Result<&'a Text, StageError> {
        match self.0.text("nickname", budget)? {
            Some(nickname) if nickname.len_units() != 0 => Ok(nickname),
            _ => self.name(budget),
        }
    }
    pub fn text(&self, field: &str, budget: &Budget) -> Result<Option<&'a Text>, StageError> { self.0.text(field, budget) }
    /// A string field TS reads unconditionally, so it must be present.
    pub fn required_text(&self, field: &str, budget: &Budget) -> Result<&'a Text, StageError> { self.0.required_text(field, budget) }
    pub fn truthy(&self, field: &str, budget: &Budget) -> Result<bool, StageError> { self.0.truthy(field, budget) }
    pub fn array(&self, field: &str, budget: &Budget) -> Result<Option<&'a [Value]>, StageError> { self.0.array(field, budget) }
    pub fn get(&self, field: &str, budget: &Budget) -> Result<Option<&'a Value>, StageError> { self.0.get(field, budget) }
    pub fn alternate_greetings(&self, budget: &Budget) -> Result<&'a [Value], StageError> {
        Ok(self.0.array("alternateGreetings", budget)?.unwrap_or_default())
    }
    /// `depth_prompt` as `{ depth, prompt }`.
    pub fn depth_prompt(&self, budget: &Budget) -> Result<Option<Fields<'a>>, StageError> {
        self.0.object("depth_prompt", "character.depth_prompt", budget)
    }
    /// `newGenData`, the inlay instructions of the emotion and image views.
    pub fn new_gen_data(&self, budget: &Budget) -> Result<Option<Fields<'a>>, StageError> {
        self.0.object("newGenData", "character.newGenData", budget)
    }
    /// Whether a character trigger runs Lua, as `runLuaEditTriggerBatch` checks: `effect[0].type === 'triggerlua'`.
    pub fn has_lua_trigger(&self, budget: &Budget) -> Result<bool, StageError> {
        for trigger in self.0.array("triggerscript", budget)?.unwrap_or_default() {
            budget.work(1)?;
            let Some(effects) = trigger.get(b"effect", budget)?.and_then(Value::array) else { continue };
            let Some(first) = effects.first() else { continue };
            if crate::root::is_text(first.get(b"type", budget)?, "triggerlua") { return Ok(true); }
        }
        Ok(false)
    }
}
