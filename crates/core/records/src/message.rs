//! Conversation messages.

use crate::root::{is_text, Fields};
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Text, Value};

/// How a message's `disabled` field takes part in prompt history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disabled {
    /// Any value other than `true` and `'allBefore'`, including a missing field.
    No,
    /// `disabled === true`: the message is left out.
    Yes,
    /// `disabled === 'allBefore'`: the message and every earlier one are left out.
    AllBefore,
}

/// A stored message row.
#[derive(Debug, Clone, Copy)]
pub struct Message<'a>(Fields<'a>);

impl<'a> Message<'a> {
    pub fn new(value: &'a Value) -> Result<Self, StageError> { Ok(Self(Fields::new(value, "message")?)) }
    pub fn fields(&self) -> Fields<'a> { self.0 }
    /// `role`; TS sends `user` as a user message and every other role as an assistant message.
    pub fn role(&self, budget: &Budget) -> Result<&'a Text, StageError> { self.0.required_text("role", budget) }
    pub fn is_user(&self, budget: &Budget) -> Result<bool, StageError> { Ok(is_text(self.0.get("role", budget)?, "user")) }
    pub fn data(&self, budget: &Budget) -> Result<&'a Text, StageError> { self.0.required_text("data", budget) }
    /// `chatId`, as stored: absent or `null` when the message has none yet.
    pub fn id(&self, budget: &Budget) -> Result<Option<&'a Text>, StageError> { self.0.text("chatId", budget) }
    pub fn disabled(&self, budget: &Budget) -> Result<Disabled, StageError> {
        Ok(match self.0.get("disabled", budget)? {
            Some(Value::Bool(true)) => Disabled::Yes,
            value if is_text(value, "allBefore") => Disabled::AllBefore,
            _ => Disabled::No,
        })
    }
}
