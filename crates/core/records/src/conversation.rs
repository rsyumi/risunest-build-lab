//! Conversations.

use crate::message::Message;
use crate::root::Fields;
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Text, Value};

/// A stored conversation record with its complete `message` array.
#[derive(Debug, Clone, Copy)]
pub struct Conversation<'a>(Fields<'a>);

impl<'a> Conversation<'a> {
    pub fn new(value: &'a Value) -> Result<Self, StageError> { Ok(Self(Fields::new(value, "conversation")?)) }
    pub fn fields(&self) -> Fields<'a> { self.0 }
    pub fn value(&self) -> &'a Value { self.0.value() }
    /// The author note.
    pub fn note(&self, budget: &Budget) -> Result<Option<&'a Text>, StageError> { self.0.text("note", budget) }
    /// `fmIndex`: -1 selects `firstMessage`, another value an alternate greeting.
    pub fn greeting_index(&self, budget: &Budget) -> Result<Option<f64>, StageError> { self.0.number("fmIndex", budget) }
    pub fn messages(&self, budget: &Budget) -> Result<Vec<Message<'a>>, StageError> {
        let values = self.0.array("message", budget)?.ok_or_else(|| self.0.invalid("message"))?;
        let mut messages = Vec::with_capacity(values.len());
        for value in values { budget.work(1)?; messages.push(Message::new(value)?); }
        Ok(messages)
    }
    pub fn get(&self, field: &str, budget: &Budget) -> Result<Option<&'a Value>, StageError> { self.0.get(field, budget) }
}
