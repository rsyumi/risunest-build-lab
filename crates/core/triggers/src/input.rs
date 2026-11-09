//! What a trigger run reads, copied from the stored records, and the message ids it draws.

use crate::js::{copy_value, unsupported};
use risunest_cbs_experimental::cbs::processor::CaseTables;
use risunest_ir::budget::{Budget, OwnedVec};
use risunest_ir::rng::Rng;
use risunest_ir::stage::{Effect, StageError};
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use risunest_records::edits::edit_messages;
use risunest_records::modules::{select_conversation_modules, ModuleSources, SelectedModules};

/// One `triggerscript` list and the `lowLevelAccess` `runTrigger` gives each of its triggers.
#[derive(Debug)]
pub struct TriggerList {
    pub triggers: Value,
    pub low_level: bool,
}

/// The records `runTrigger` reads for the selected conversation (`src/ts/process/triggers.ts:1142-1260`), as stored.
/// A field TS reads but the record leaves out is `Value::Undefined`; the runner checks shapes when it runs.
#[derive(Debug)]
pub struct TriggerInput {
    /// `char.triggerscript` with `char.lowLevelAccess ?? false`, then the `trigger` list of each module
    /// `getModuleTriggers()` reads (`src/ts/process/modules.ts:545-561`), with that module's `lowLevelAccess`.
    pub lists: Vec<TriggerList>,
    /// The conversation's `message`, `scriptstate`, `note` and `fmIndex`, every message loaded.
    pub messages: Value,
    pub scriptstate: Value,
    pub note: Value,
    pub first_message_index: Value,
    /// The character's `firstMessage`, `alternateGreetings` and `defaultVariables`.
    pub first_message: Value,
    pub alternate_greetings: Value,
    pub default_variables: Value,
    /// The root `templateDefaultVariables`.
    pub template_default_variables: Value,
    /// `CurrentTriggerIdStore` before the run; None is null.
    pub previous_trigger_id: Option<Text>,
    /// The default locale tailors case mapping (`tr`, `az`, `lt`, ...), so `≒` on text declines (`case-locale`).
    pub tailored_case: bool,
    /// The Unicode tables of the engine's `toLowerCase` and `toUpperCase`, as the host probes them. Text these tables
    /// could map other than Rust's declines (`trigger-case-table`) where an effect or condition maps its case.
    pub case_tables: CaseTables,
}

impl TriggerInput {
    /// The input for `conversation` of `character` under `root`, with no previous trigger ID, untailored case mapping
    /// and unprobed case tables, for tests. It reads the stored root mirrors (`moduleIntergration`,
    /// `templateDefaultVariables`), which the effective preset can override, so an owner builds its input with
    /// [`TriggerInput::from_records_with_modules`].
    pub fn from_records(root: &Value, character: &Value, conversation: &Value, budget: &Budget) -> Result<Self, StageError> {
        let sources = ModuleSources::for_conversation(root, character, conversation, budget)?;
        let modules = select_conversation_modules(&sources, budget)?;
        Self::from_selected(root, character, conversation, &modules, budget)
    }

    /// [`TriggerInput::from_records`] over the module records the owner's other preparation stages use, selected with
    /// the effective `moduleIntergration`, which the stored root mirror can differ from. Of `root` only
    /// `templateDefaultVariables` is read, so the owner passes a root that holds the effective value.
    pub fn from_records_with_modules(root: &Value, character: &Value, conversation: &Value, modules: &SelectedModules, budget: &Budget)
        -> Result<Self, StageError> {
        let modules: Vec<&Value> = modules.records().iter().collect();
        Self::from_selected(root, character, conversation, &modules, budget)
    }

    fn from_selected(root: &Value, character: &Value, conversation: &Value, modules: &[&Value], budget: &Budget) -> Result<Self, StageError> {
        let field = |value: &Value, key: &str| -> Result<Value, StageError> {
            Ok(match value.get(key.as_bytes(), budget)? { Some(value) => copy_value(value, budget)?, None => Value::Undefined })
        };
        let truthy = |value: &Value, key: &str| -> Result<bool, StageError> {
            Ok(value.get(key.as_bytes(), budget)?.is_some_and(Value::truthy))
        };
        let mut lists = vec![TriggerList { triggers: field(character, "triggerscript")?, low_level: truthy(character, "lowLevelAccess")? }];
        for &module in modules {
            if !module.truthy() { continue; }
            let Some(triggers) = module.get(b"trigger", budget)? else { continue };
            if !triggers.truthy() { continue; }
            lists.push(TriggerList { triggers: copy_value(triggers, budget)?, low_level: truthy(module, "lowLevelAccess")? });
        }
        Ok(Self {
            lists,
            messages: field(conversation, "message")?,
            scriptstate: field(conversation, "scriptstate")?,
            note: field(conversation, "note")?,
            first_message_index: field(conversation, "fmIndex")?,
            first_message: field(character, "firstMessage")?,
            alternate_greetings: field(character, "alternateGreetings")?,
            default_variables: field(character, "defaultVariables")?,
            template_default_variables: field(root, "templateDefaultVariables")?,
            previous_trigger_id: None,
            tailored_case: false,
            case_tables: CaseTables::Foreign,
        })
    }

    /// A change another stage made before the run, stored as TS stores it before the trigger reads the chat: a chat
    /// variable goes to `scriptstate['$' + name]` (`updateChatVariables`,
    /// `src/ts/storage/activeConversationSession.ts:1910-1937`), the note replaces the conversation's, and a message edit
    /// changes `message` by its [`MessageEdit`] rule (`risunest_records::edits::edit_messages`). A field or character
    /// property the run does not read changes nothing. No stage writes `scriptstate`, `message` or `fmIndex` as a chat
    /// field, so those decline as `trigger-applied-effect`.
    pub fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
        match effect {
            Effect::ChatVariable { name, value } => {
                if matches!(self.scriptstate, Value::Undefined | Value::Null) { self.scriptstate = Value::Object(OwnedVec::new(budget)?); }
                if !matches!(self.scriptstate, Value::Object(_)) { return Err(unsupported("trigger-conversation-shape")); }
                let mut key = Text::from_str("$", budget)?;
                key.append(name)?;
                self.scriptstate.define(key, copy_value(value, budget)?, budget)?;
            }
            Effect::ChatField { field, value } => {
                if field.equals_ascii(b"note") { self.note = copy_value(value, budget)?; }
                if [b"scriptstate".as_slice(), b"message", b"fmIndex"].iter().any(|read| field.equals_ascii(read)) {
                    return Err(unsupported("trigger-applied-effect"));
                }
            }
            Effect::Message(edit) => {
                if !matches!(self.messages, Value::Array(_)) { return Err(unsupported("trigger-conversation-shape")); }
                edit_messages(&mut self.messages, edit, budget)?;
            }
            Effect::CharacterField { field, value } => {
                if field.equals_ascii(b"firstMessage") { self.first_message = Value::Text(value.copy(budget)?); }
            }
            _ => return Err(unsupported("trigger-applied-effect")),
        }
        Ok(())
    }
}

/// The `chatId` of each message a trigger appends. TS draws it with `v4()` (`crypto.randomUUID`), not `Math.random`.
pub trait MessageIds {
    fn next_id(&mut self, budget: &Budget) -> Result<Text, StageError>;
}

/// Version 4 UUIDs from an entropy source such as `risunest_ir::rng::OsEntropy`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomIds<R>(pub R);

impl<R: Rng> MessageIds for RandomIds<R> {
    fn next_id(&mut self, budget: &Budget) -> Result<Text, StageError> {
        // Each draw carries 53 uniform bits.
        let mut bits: u128 = 0;
        for _ in 0..3 { bits = (bits << 53) | (self.0.random()? * (1u64 << 53) as f64) as u128; }
        let mut bytes = bits.to_be_bytes();
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let id = format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]);
        Ok(Text::from_str(&id, budget)?)
    }
}
