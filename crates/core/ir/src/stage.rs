//! Stage traits and the host protocol.
//!
//! [`TextProcessor`] and [`PatternEngine`] are synchronous. Every other stage can reach a host callback, so its entry
//! call returns [`Step::NeedsHost`] or [`Step::Done`], and the owner passes each [`HostReply`] to the same stage's
//! `resume` until it is done. A stage keeps at most one call in flight; `resume` without an outstanding request is
//! [`StageError::Invalid`].
//!
//! Stage objects must be `Send` so an owner can move them between threads; they need not be `Sync`.
//! `Value` derives only `Debug`, so tests compare values through `json::stringify`.

use crate::budget::{Budget, Error, OwnedVec};
use crate::chat::{copy_value, MessageRole, PromptMessage, Role};
use crate::provider::{HttpRequestSpec, ProviderFailure, ProviderOutput};
use crate::text::Text;
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::ops::Range;

#[derive(Debug)]
#[must_use]
pub enum Step<T> {
    NeedsHost(HostRequest),
    Done(T),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StageError {
    /// An input or setting the native stage does not run yet, named by its field.
    Unsupported(Cow<'static, str>),
    /// A malformed input, named by its field.
    Invalid(Cow<'static, str>),
    /// A resource limit, named like the `Budget` limits.
    Limit(Cow<'static, str>),
    Cancelled,
    /// A host callback failed with this message, as UTF-16 code units (`HostReply::Failed`).
    Host(Vec<u16>),
}

impl From<Error> for StageError {
    fn from(value: Error) -> Self {
        match value {
            Error::Cancelled => Self::Cancelled,
            Error::Limit(name) => Self::Limit(Cow::Borrowed(name)),
            Error::Allocation => Self::Limit(Cow::Borrowed("allocation")),
            Error::Syntax { .. } => Self::Invalid(Cow::Borrowed("json-syntax")),
            Error::Type { operation } => Self::Invalid(Cow::Borrowed(operation)),
            Error::IsolatedUtf16 => Self::Invalid(Cow::Borrowed("isolated-utf16")),
            Error::DecoderFinished => Self::Invalid(Cow::Borrowed("decoder-finished")),
            Error::Invariant(name) => Self::Invalid(Cow::Borrowed(name)),
        }
    }
}

/// `ScriptMode` (`src/ts/process/scripts.ts:52`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptMode { EditInput, EditOutput, EditProcess, EditDisplay }

/// `triggerMode` (`src/ts/process/triggers.ts:124`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerMode { Start, Manual, Output, Input, Display, Request }

/// A conversation change a stage returns instead of writing it.
#[derive(Debug)]
#[non_exhaustive]
pub enum Effect {
    /// `setChatVar(name, value)` on the selected chat; TS can store a non-string such as `undefined`.
    ChatVariable { name: Text, value: Value },
    /// Replaces a stored chat field such as `hypaV3Data`, `supaMemoryData` or `lastMemory`.
    ChatField { field: Text, value: Value },
    /// An edit of the selected chat's `message` array.
    Message(MessageEdit),
    /// `updateCapturedCharacterField(field, value)` (`src/ts/process/scriptings.ts:371-404`) from the Lua `setName`,
    /// `setDescription`, `setCharacterFirstMessage` and `setBackgroundEmbedding` (`:1026-1108`): `field` is the
    /// character property TS writes, `name`, `desc`, `firstMessage` or `backgroundHTML`. A producer emits it only when TS
    /// writes, not when that function returns `false`.
    CharacterField { field: Text, value: Text },
}

/// One edit of the selected chat's `message` array. Indices count over the whole conversation, loaded or not, as the
/// `src/ts/process/historyWindowIndex.ts` helpers count them, and are the JS numbers the TS call received after
/// `Number()`; each variant names the rule its applier resolves them by. A producer whose TS site reads an index by
/// another rule emits only the edits this rule resolves to the same message. Message ids are carried here: TS draws
/// them with `v4()` inside the call (`crypto.randomUUID`, not `Math.random`), and a script can read them back through
/// `getFullChatMain` before the run ends (`src/ts/process/luaHistoryJSON.ts:11-13`), so the applier never makes one.
#[derive(Debug)]
#[non_exhaustive]
pub enum MessageEdit {
    /// `message.data = data` on the message `Array.prototype.at(index)` finds (`getHistoryMessageAt`,
    /// `historyWindowIndex.ts:52-59`; `editMessage`, `src/ts/process/scriptConversationAccess.ts:159-177`), and no edit
    /// when it finds none. Lua `setChat` (`scriptings.ts:503-510`) passes its index as is. Trigger `modifychat` and
    /// `v2ModifyChat` (`getHistoryMessage`, `src/ts/process/triggers.ts:1593-1600`, `:2021-2029`) and `@@inject`
    /// (`chat.message[chatID]`, `src/ts/process/scripts.ts:971-993`) index the array directly, so they emit only a
    /// non-negative integer index: `at` would resolve `NaN` to 0, a negative index from the end and 1.5 to 1, where
    /// those sites edit nothing. `data` is stored as is: a trigger or Lua value need not be a string.
    SetData { index: f64, data: Value },
    /// `message.role = role` under the [`MessageEdit::SetData`] rule (Lua `setChatRole`, `scriptings.ts:511-518`).
    SetRole { index: f64, role: MessageRole },
    /// `message = message.slice(start, end)` (`sliceHistory`, `historyWindowIndex.ts:61-83`): Lua `cutChat`
    /// (`scriptings.ts:519-525`) and trigger `cutchat` and `v2CutChat` (`triggers.ts:1586-1591`, `:2007-2019`). `end`
    /// is `None` for `undefined`, which keeps the messages to the end where `NaN` keeps none; an absent `start` is
    /// `NaN`, which starts at 0 as `undefined` does.
    Cut { start: f64, end: Option<f64> },
    /// `message.splice(index, 1)` with `index` as a `splice` start (Lua `removeChat`, `scriptings.ts:526-534`, through
    /// `toHistorySpliceIndex`, `historyWindowIndex.ts:85-94`).
    Remove { index: f64 },
    /// `message.push({ role, data, chatId })`: Lua `addChat` (`scriptings.ts:535-546`) and trigger `impersonate` and
    /// `v2Impersonate` (`triggers.ts:1544-1553`, `:2035-2044`).
    Append { role: MessageRole, data: Value, chat_id: Text },
    /// `message.splice(index, 0, { role, data, chatId })` under the [`MessageEdit::Remove`] index rule (Lua
    /// `insertChat`, `scriptings.ts:547-556`).
    Insert { index: f64, role: MessageRole, data: Value, chat_id: Text },
    /// `message = messages` (Lua `setFullChatMain`, `scriptings.ts:600-633`): the array TS assigns, after its `chatId`
    /// merge, with the new ids drawn and each kept message's stored properties spread in. The producer does the merge,
    /// reading the stored messages, since which messages keep their stored properties depends on the id counts of the
    /// parsed input, which the result no longer shows.
    Replace { messages: Value },
}

impl Effect {
    /// A deep copy, charged to `budget`, for an owner that keeps its effects and hands copies to a [`LuaView`].
    pub fn copy(&self, budget: &Budget) -> Result<Self, Error> {
        Ok(match self {
            Self::ChatVariable { name, value } => Self::ChatVariable { name: name.copy(budget)?, value: copy_value(value, 0, budget)? },
            Self::ChatField { field, value } => Self::ChatField { field: field.copy(budget)?, value: copy_value(value, 0, budget)? },
            Self::Message(edit) => Self::Message(edit.copy(budget)?),
            Self::CharacterField { field, value } => Self::CharacterField { field: field.copy(budget)?, value: value.copy(budget)? },
        })
    }
}

impl MessageEdit {
    fn copy(&self, budget: &Budget) -> Result<Self, Error> {
        Ok(match self {
            Self::SetData { index, data } => Self::SetData { index: *index, data: copy_value(data, 0, budget)? },
            Self::SetRole { index, role } => Self::SetRole { index: *index, role: *role },
            Self::Cut { start, end } => Self::Cut { start: *start, end: *end },
            Self::Remove { index } => Self::Remove { index: *index },
            Self::Append { role, data, chat_id } => Self::Append { role: *role, data: copy_value(data, 0, budget)?, chat_id: chat_id.copy(budget)? },
            Self::Insert { index, role, data, chat_id } => Self::Insert {
                index: *index, role: *role, data: copy_value(data, 0, budget)?, chat_id: chat_id.copy(budget)?,
            },
            Self::Replace { messages } => Self::Replace { messages: copy_value(messages, 0, budget)? },
        })
    }
}

// Host protocol.

#[derive(Debug)]
pub enum LuaTrigger {
    /// The edit listeners `runLuaEditTrigger` calls; `index` is `meta.index`, `None` when TS passes no meta.
    Edit { index: Option<i64> },
    /// The `triggerlua` effect at `effect` of the trigger at `trigger` in the list `runTrigger` runs: the character's
    /// triggers, then each selected module's in module order (`char.triggerscript.map(...).concat(moduleTriggers)`,
    /// `src/ts/process/triggers.ts:1196-1199`; `getModuleTriggers`, `src/ts/process/modules.ts:549-567`).
    Effect { trigger: usize, effect: usize, call: LuaCall },
}

/// How `runTrigger` runs a `triggerlua` effect (`runScripted(effect.code, ...)`, `src/ts/process/triggers.ts:1729-1747`).
#[derive(Debug)]
pub struct LuaCall {
    /// `effect.code`.
    pub code: Text,
    /// The `mode` `runScripted` receives: the event for `start`, `output` and `input`, the manual name for a manual run
    /// (`mode === 'manual' ? arg.manualName : mode`, `triggers.ts:1732`), and `manual` when that name is undefined
    /// (`src/ts/process/scriptings.ts:236`). It picks the function as `runScripted`'s switch does (`:1475-1534`:
    /// `start`, `output` and `input` call `onStart`, `onOutput` and `onInput`; a manual name equal to another case label,
    /// such as `editOutput` or `onButtonClick`, takes that case; any other name calls the global of that name), and it
    /// keys the engine cache with the code and the deadline (`:1649`).
    pub entry: Text,
    /// `trigger.lowLevelAccess`: the character's `lowLevelAccess` for its own triggers (`triggers.ts:1189`,
    /// `:1196-1199`), the module's for a module's (`modules.ts:549-567`).
    pub low_level: bool,
    pub deadline: LuaDeadline,
}

/// The deadline of each synchronous Lua segment, `functionTimeout` (`src/ts/process/scriptings.ts:245`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LuaDeadline {
    /// `LUA_LISTENER_TIMEOUT_MS`, 5 s (`scriptings.ts:73`): an edit listener, or a trigger effect outside a manual run.
    Listener,
    /// `LUA_USER_HANDLER_TIMEOUT_MS`, 60 s (`scriptings.ts:75`): `userStarted`, which `runTrigger` sets for every manual run
    /// (`src/ts/process/triggers.ts:1739`), including a `runtrigger` nested under `onStart`.
    UserStarted,
}

/// What a Lua call reads besides the stored records: the host answers its reads from the stored root, character and
/// conversation with `effects` applied in order, as TS's callbacks read the working copy that already holds them.
#[derive(Debug, Default)]
pub struct LuaView {
    /// The changes the send has made that the store does not hold yet, in the order they were made within each kind
    /// (variables, chat and character fields, message edits); across kinds the order may differ, which the result does
    /// not observe because each kind changes separate state. A stage lists the changes it holds that its owner has not
    /// taken, leaving out those [`TriggerRunner::take_effects`] hands over before
    /// the request goes out; each owner that forwards the request puts the changes it holds before them.
    pub effects: Vec<Effect>,
    /// For a `triggerlua` effect, the trigger locals the trigger's `getVar` reads before the chat variables
    /// (`getLocalVar`, `src/ts/process/triggers.ts:1265-1280`; `getVar`, `:1338-1343`): each name visible at the current
    /// indent once, with the value of the innermost indent that holds it. `setChatVar` on one of these names writes the
    /// local (`setVar`, `:1370-1373`). Empty for an edit listener, which reads no locals.
    pub locals: Vec<(Text, Value)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LuaMode { Script(ScriptMode), EditRequest, Trigger(TriggerMode) }

#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub enum PluginHookKind {
    /// `pluginV2[mode]` script hooks.
    Script(ScriptMode),
    /// `replacerbeforeRequest`, with the request's `ModelModeExtended`.
    ReplacerBeforeRequest { model_mode: Text },
    /// `replacerafterRequest`, with the request's `ModelModeExtended`.
    ReplacerAfterRequest { model_mode: Text },
}

/// An auxiliary model call such as a memory summary, emotion or translation.
#[derive(Debug)]
pub struct SubRequest {
    /// `ModelModeExtended` of the call (`memory`, `emotion`, `submodel`, ...).
    pub mode: Text,
    pub messages: Vec<PromptMessage>,
    /// The other `requestChatData` arguments as TS passes them.
    pub options: Value,
}

#[derive(Debug)]
pub struct EmbedRequest {
    pub model: Text,
    pub texts: Vec<Text>,
}

/// A read of the selected conversation that a stage was not built with. The owner answers from the conversation as it
/// stands, after every effect it has applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ConversationRead {
    /// `@@repeat_back`'s `lastChat` for the message at `chat_index` (`src/ts/process/scripts.ts:1034-1052`): the `data`
    /// of the nearest earlier message with that message's role, else the selected greeting (`firstMessage`, or
    /// `alternateGreetings[fmIndex]`); the owner walks the roles and applies `projectedChatID` where TS has one. The reply
    /// is `HostReply::Value` holding that text, `Value::Undefined` where TS reads `undefined`, or `HostReply::Failed`
    /// with what TS throws: the TypeError when the walk reaches a missing message, or `ConversationSessionInactiveError`
    /// without a character. It is read at the rule rather than carried in the request because the Lua edit listener and
    /// the display trigger run first in the same call (`scripts.ts:670-701`) and can edit those messages.
    RepeatBackSource { chat_index: i64 },
}

#[derive(Debug)]
#[non_exhaustive]
pub enum HostRequest {
    /// `payload` is the string or prompt array the listener receives, and `Value::Undefined` for a trigger effect, which
    /// TS calls without data.
    Lua { trigger: LuaTrigger, mode: LuaMode, payload: Value, view: LuaView },
    /// Called once per registered hook, in registration order.
    PluginHook { kind: PluginHookKind, payload: Value },
    /// The plugin provider's `tokenizerFunc`.
    PluginTokenizer { text: Text },
    Llm(SubRequest),
    Embed(EmbedRequest),
    Conversation(ConversationRead),
    /// A network call the stage built itself, sent as plain `fetch` sends it, such as the `countTokens` POST of
    /// `tokenizeGoogleCloud` (`src/ts/tokenizer.ts:232-266`). The reply is [`HostReply::Http`] for any completed
    /// response, `HostReply::Failed` with the message of what `fetch` threw, or `HostReply::Cancelled`. `Debug` shows the
    /// spec without its URL, header values or body.
    Http(HttpRequestSpec),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum HostKind { Lua, PluginHook, PluginTokenizer, Llm, Embed, Conversation, Http }

impl HostRequest {
    pub fn kind(&self) -> HostKind {
        match self {
            Self::Lua { .. } => HostKind::Lua,
            Self::PluginHook { .. } => HostKind::PluginHook,
            Self::PluginTokenizer { .. } => HostKind::PluginTokenizer,
            Self::Llm(_) => HostKind::Llm,
            Self::Embed(_) => HostKind::Embed,
            Self::Conversation(_) => HostKind::Conversation,
            Self::Http(_) => HostKind::Http,
        }
    }
}

impl HostKind {
    /// Whether TS, running the same work again from the start after a decline, would repeat something visible: a Lua
    /// listener, a plugin hook and a model, embedding or network call run code or requests of their own, while a plugin
    /// tokenizer count and a conversation read do not. TS memoizes plugin tokenizer counts only when
    /// `db.useTokenizerCaching` is set (`encode`, `src/ts/tokenizer.ts:94-108`) and otherwise calls the plugin again for
    /// the same tokens, so a native adapter must answer these counts through `encode()`, never the plugin's tokenizer
    /// directly. A stage may decline in favor of TS until a request that commits has gone out.
    pub fn commits(self) -> bool { !matches!(self, Self::PluginTokenizer | Self::Conversation) }
}

/// What a Lua callback returned and wrote (`runScripted`, `src/ts/process/scriptings.ts:171`).
#[derive(Debug)]
pub struct LuaReply {
    /// The listener batch result of an edit trigger (the edited string or prompt array), or the handler's first return
    /// value; `Value::Undefined` when it returned nothing.
    pub value: Value,
    /// The writes the callback made, in the order TS makes them. The owner feeds each to `TextProcessor::apply` and to
    /// its own conversation and trigger state.
    pub effects: Vec<Effect>,
    /// `stopChat()` ran or the handler returned `false` (`scriptings.ts:429-434`, `:1526-1528`).
    pub stop_sending: bool,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum HostReply {
    /// A plugin hook result, or a conversation read; `Value::Undefined` when the callback returned nothing. A `Value`
    /// reply to `HostRequest::Lua` reads as a [`LuaReply`] with that value, no effects and no stop.
    Value(Value),
    /// The reply to `HostRequest::Lua`.
    Lua(LuaReply),
    /// Plugin tokenizer ids.
    Tokens(Vec<f64>),
    Llm(Result<ProviderOutput, ProviderFailure>),
    /// One vector per requested text.
    Embeddings(Vec<Vec<f64>>),
    /// A completed response to `HostRequest::Http`, whatever its status: the body as `res.json()` or `res.text()` reads
    /// it, after content decoding.
    Http { status: u16, body: Vec<u8> },
    /// The callback threw; `message` is the error message TS observes. For a `triggerlua` call only an error in the
    /// script's top level is `Failed`: `runScripted` catches an error inside the handler, logs it and returns
    /// (`src/ts/process/scriptings.ts:1538-1551`), so the host answers that case with a [`LuaReply`] holding the writes
    /// made before the error.
    Failed { message: Text },
    /// The host cannot serve this request natively yet.
    Unsupported(Cow<'static, str>),
    Cancelled,
}

// Synchronous stages.

/// `CbsConditions` (`src/ts/parser/parser.svelte.ts:1199`).
#[derive(Debug, Default)]
pub struct CbsConditions {
    pub first_message: Option<bool>,
    pub chat_role: Option<Text>,
}

/// The `risuChatParser` options a caller sets per text.
#[derive(Debug)]
pub struct TextContext {
    /// `chatID`; -1 when the text belongs to no message.
    pub chat_index: i64,
    /// `role` of the message the text belongs to.
    pub role: Option<Text>,
    pub run_var: bool,
    pub rm_var: bool,
    pub visualize: bool,
    pub conditions: CbsConditions,
    pub chara: Chara,
}
impl Default for TextContext {
    fn default() -> Self {
        Self { chat_index: -1, role: None, run_var: false, rm_var: false, visualize: false, conditions: CbsConditions::default(),
            chara: Chara::default() }
    }
}

/// `risuChatParser`'s `chara` option (`src/ts/parser/parser.svelte.ts:1778`, `:1804-1815`) as `{{char}}` and the
/// character field functions read it (`src/ts/cbs.ts:226-246`, `:310-390`).
#[derive(Debug, Default)]
#[non_exhaustive]
pub enum Chara {
    /// No `chara`, a falsy one such as `''`, or the selected character's own object, which every reader treats alike:
    /// CBS reads `db.characters[selectedCharID]`. `parseChatML` (`src/ts/parser/chatML.ts:55`) passes none, and the
    /// send passes its current character.
    #[default]
    Selected,
    /// A name string (`chara: name`, `src/ts/process/index.svelte.ts:2006`): `{{char}}` reads it only when no
    /// character is selected, and the character field functions still read the selected character. An empty name is
    /// [`Chara::Selected`].
    Name(Text),
}

#[derive(Debug)]
pub struct TextOutput {
    pub text: Text,
    /// Variable writes in the order TS makes them, such as `setvar` under `run_var`.
    pub effects: Vec<Effect>,
}

/// CBS evaluation (`risuChatParser`). Later `process` calls read the processor's own writes and every effect
/// passed to `apply`.
pub trait TextProcessor {
    fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError>;
    /// Feeds a write made elsewhere (a trigger, the lorebook or a host callback) into the variables CBS reads.
    fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError>;
}

/// A caller-owned variable overlay for one synchronous CBS invocation. Setters change only the overlay;
/// the caller publishes it after success and discards it after a decline or resource interruption.
pub trait TextVariables {
    /// None is a missing argument, distinct from the string `undefined` when resolving defaults.
    fn get(&mut self, key: Option<&Text>, budget: &Budget) -> Result<Text, StageError>;
    /// What `getChatVar` returns, which can be a value a trigger local or pending write keeps as it was written.
    fn get_value(&mut self, key: Option<&Text>, budget: &Budget) -> Result<Value, StageError> { self.get(key, budget).map(Value::Text) }
    fn set(&mut self, key: Option<&Text>, value: Option<&Text>, budget: &Budget) -> Result<(), StageError>;
}

pub struct TextScope<'a> {
    pub variables: &'a mut dyn TextVariables,
    /// The effective trigger ID for this call. None explicitly reads as `null`.
    pub trigger_id: Option<&'a Text>,
}

/// CBS against a borrowed invocation scope. The processor retains no scope or trigger ID after return.
pub trait ScopedTextProcessor: TextProcessor {
    /// Writes belong to `scope.variables`, so they are not returned again as TextOutput effects. An error does
    /// not publish the overlay; callers retain their previous trigger ID and completed earlier effects.
    fn process_scoped(&mut self, input: &Text, context: &TextContext, scope: TextScope<'_>, budget: &Budget)
        -> Result<Text, StageError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternMatch {
    /// UTF-16 ranges of the whole match (index 0) and each capture group; `None` for a group that did not participate.
    pub groups: Vec<Option<Range<usize>>>,
}

pub trait Pattern {
    /// `RegExp.prototype.exec` with `lastIndex` set to `last_index`; as in JS, it is ignored unless the pattern is
    /// global or sticky. The caller keeps `lastIndex`.
    fn exec(&self, input: &Text, last_index: usize, budget: &Budget) -> Result<Option<PatternMatch>, StageError>;
    /// Named groups as `(name, group index)` in pattern order.
    fn group_names(&self) -> &[(Text, usize)];
}

pub trait PatternEngine {
    /// `new RegExp(source, flags)`. A pattern JS rejects is `Invalid`; one this engine cannot run is `Unsupported`.
    fn compile(&mut self, source: &Text, flags: &Text, budget: &Budget) -> Result<Box<dyn Pattern + Send>, StageError>;
}

// Resumable stages.

#[derive(Debug, Clone, Copy)]
pub enum TokenInput<'a> {
    /// `tokenize(text)`.
    Text(&'a Text),
    /// `tokenizeChat(chat)`, including its per-message accounting.
    Message(&'a PromptMessage),
    /// `tokenizeChats(chats)`.
    Messages(&'a [PromptMessage]),
}

/// Token counts under the active tokenizer. Built-in tokenizers finish without the host.
pub trait TokenCounter {
    fn count(&mut self, input: TokenInput<'_>, budget: &Budget) -> Result<Step<u32>, StageError>;
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<u32>, StageError>;
    /// The `Unsupported` name every count returns, known without counting, so that an owner can decline before its
    /// first host request; `None` when a count may succeed.
    fn decline(&self) -> Option<&'static str> { None }
}

#[derive(Debug)]
pub struct ScriptRequest {
    pub mode: ScriptMode,
    pub input: Text,
    /// `chatID`; -1 when the text belongs to no message.
    pub chat_index: i64,
    pub conditions: CbsConditions,
}

#[derive(Debug)]
pub struct ScriptOutput {
    pub text: Text,
    /// `emoChanged`.
    pub emotion_changed: bool,
    pub effects: Vec<Effect>,
}

/// `processScriptFull` over the scripts the runner was built with.
pub trait ScriptRunner {
    fn run(&mut self, request: ScriptRequest, budget: &Budget) -> Result<Step<ScriptOutput>, StageError>;
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<ScriptOutput>, StageError>;
}

/// One active entry of `loadLoreBookV3Prompt`'s `actives`.
#[derive(Debug)]
pub struct LoreFragment {
    /// `pos`: empty, a named position, or `pt_<name>` for `{{position::name}}`.
    pub position: Text,
    pub prompt: Text,
    pub role: Role,
    pub depth: f64,
    pub order: f64,
    pub priority: f64,
    pub tokens: u32,
    pub source: Text,
    pub inject: Option<LoreInject>,
}

/// The `@@inject_*` decorators of an entry (`src/ts/process/lorebook.svelte.ts:335-339`).
#[derive(Debug)]
pub struct LoreInject {
    pub operation: InjectOperation,
    pub location: Text,
    pub param: Text,
    /// `@@inject_lore`: the target is another lore entry rather than a prompt position.
    pub lore: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InjectOperation { Append, Prepend, Replace }

/// One `matchLog` entry (`src/ts/process/lorebook.svelte.ts:236-310`): which key matched which scanned text.
#[derive(Debug)]
pub struct LoreMatch {
    /// The scanned prompt: normalized for a keyword match, as built for a regex match.
    pub prompt: Text,
    /// The scanned text's label, such as `message 0 by user`.
    pub source: Text,
    /// The key as written.
    pub activated: Text,
}

#[derive(Debug)]
pub struct LoreOutput {
    /// In the order TS returns them.
    pub fragments: Vec<LoreFragment>,
    /// Chat-variable writes in the order TS makes them.
    pub effects: Vec<Effect>,
    /// `matchLog` in the order TS appends it, which the `test_lorebook` command and the developer tools show
    /// (`src/ts/process/command.ts:242-246`, `src/lib/SideBars/DevTool.svelte:322-344`).
    pub match_log: Vec<LoreMatch>,
}

/// `loadLoreBookV3Prompt` over the history and lore the selector was built with.
pub trait LoreSelector {
    fn select(&mut self, budget: &Budget) -> Result<Step<LoreOutput>, StageError>;
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<LoreOutput>, StageError>;
    /// Completed effects not already returned in a successful output.
    fn take_effects(&mut self) -> Vec<Effect> { Vec::new() }
}

#[derive(Debug)]
pub struct MemoryInput {
    pub chats: Vec<PromptMessage>,
    pub current_tokens: f64,
    pub max_context_tokens: f64,
}

#[derive(Debug)]
pub struct MemoryOutput {
    pub chats: Vec<PromptMessage>,
    pub current_tokens: f64,
    /// Stored memory updates, applied even when `error` is set.
    pub effects: Vec<Effect>,
    /// The message TS raises through `throwError`, failing the send.
    pub error: Option<Text>,
}

/// The selected memory mode (`hypaMemoryV3`, `hypaMemoryV2`, `supaMemory` or `hanuraiMemory`) over the conversation
/// the stage was built with.
pub trait MemoryStage {
    fn run(&mut self, input: MemoryInput, budget: &Budget) -> Result<Step<MemoryOutput>, StageError>;
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<MemoryOutput>, StageError>;
}

#[derive(Debug, Clone, Copy)]
pub enum TriggerCbsVariables {
    /// Default selected-conversation CBS reads and writes, without trigger locals or display temporary values.
    Conversation,
    /// The operation/access callbacks resolve trigger local, stored, default and display temporary variables.
    TriggerScope,
}

#[derive(Debug)]
pub struct TriggerRequest {
    pub mode: TriggerMode,
    /// `displayData` for the display and request modes.
    pub display: Option<Text>,
    /// `manualName`: the triggers whose `comment` is this name run instead of those of `mode`
    /// (`src/ts/process/triggers.ts:1179-1182`), and a Lua effect calls the function of that name in manual mode
    /// (`:1729-1740`).
    pub manual_name: Option<Text>,
    /// `displayMode`, independent of the event in `mode`.
    pub display_mode: bool,
    /// The supplied `tempVars` object; None remains distinct from a supplied empty object.
    pub temp_vars: Option<Value>,
    /// The ID argument for this invocation, separate from the captured previous current trigger ID.
    pub trigger_id: Option<Text>,
    pub cbs_variables: TriggerCbsVariables,
}

#[derive(Debug)]
pub struct TriggerOutput {
    /// `additonalSysPrompt.start`.
    pub system_prompt_start: Text,
    /// `additonalSysPrompt.historyend`.
    pub system_prompt_history_end: Text,
    /// `additonalSysPrompt.promptend`.
    pub system_prompt_end: Text,
    /// Undelivered conversation and variable changes, in order. Earlier take_effects prefixes are absent.
    pub effects: Vec<Effect>,
    pub tokens: u32,
    pub stop_sending: bool,
    pub send_ai_prompt: bool,
    /// `displayData` after a display or request trigger.
    pub display: Option<Text>,
    pub temp_vars: Option<Value>,
    /// After success, manual calls keep their new ID and other calls restore the captured previous ID.
    /// On a failed invocation the caller retains its previous ID instead.
    pub current_trigger_id: Option<Text>,
}

/// `runTrigger` over the character triggers the runner was built with.
pub trait TriggerRunner {
    fn run(&mut self, request: TriggerRequest, budget: &Budget) -> Result<Step<TriggerOutput>, StageError>;
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<TriggerOutput>, StageError>;
    /// The declines `run(request)` makes before its first effect or host request, decided from the records the runner
    /// holds without running anything. An owner asks before its own first host request, so that such a decline still
    /// leaves the send to TS. The default admits every request: a runner without admission declines when it runs.
    fn admit(&self, _request: &TriggerRequest, _budget: &Budget) -> Result<(), StageError> { Ok(()) }
    /// Feeds a change another stage made before `run` (a chat variable, a chat field or a message edit) into the
    /// conversation the run reads, as TS's trigger reads the conversation as it stands when it starts. An owner calls it
    /// only before `run`, in the order the changes were made. The default ignores it, which suits only a runner that
    /// reads no conversation state; a runner that copies the records when it is built must override it.
    fn apply(&mut self, _effect: &Effect, _budget: &Budget) -> Result<(), StageError> { Ok(()) }
    /// Moves the completed, undelivered prefix out before a host request or after a terminal error. Admission
    /// declines return none. Done carries only the remaining suffix and leaves this empty; callers never feed a
    /// delivered effect back into this runner. Draining remains available after budget exhaustion/cancellation.
    fn take_effects(&mut self) -> Vec<Effect> { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Limits;
    use crate::parse_json;
    use std::sync::{atomic::AtomicBool, Arc};

    fn budget() -> Budget {
        let limits = Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 16, max_text_units: 1 << 16, max_values: 4096,
            max_depth: 64, max_work: 1 << 20 };
        Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap()
    }

    // Every part of an effect: texts by their units, numbers by their bits, so that `-0`, `NaN` and isolated surrogates count.
    fn units(text: &Text) -> String { format!("{:?}", text.units().collect::<Vec<_>>()) }
    fn bits(number: f64) -> String { format!("#{:016x}", number.to_bits()) }
    fn value(item: &Value) -> String {
        match item {
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(flag) => flag.to_string(),
            Value::Number(number) => bits(*number),
            Value::Text(text) => units(text),
            Value::Array(items) => format!("[{}]", items.as_slice().iter().map(value).collect::<Vec<_>>().join(",")),
            Value::Object(entries) => format!("{{{}}}", entries.as_slice().iter().map(|(key, item)| format!("{}:{}", units(key), value(item)))
                .collect::<Vec<_>>().join(",")),
        }
    }
    fn effect(effect: &Effect) -> String {
        match effect {
            Effect::ChatVariable { name, value: item } => format!("variable {} {}", units(name), value(item)),
            Effect::ChatField { field, value: item } => format!("field {} {}", units(field), value(item)),
            Effect::CharacterField { field, value: text } => format!("character {} {}", units(field), units(text)),
            Effect::Message(MessageEdit::SetData { index, data }) => format!("set-data {} {}", bits(*index), value(data)),
            Effect::Message(MessageEdit::SetRole { index, role }) => format!("set-role {} {role:?}", bits(*index)),
            Effect::Message(MessageEdit::Cut { start, end }) => format!("cut {} {:?}", bits(*start), end.map(bits)),
            Effect::Message(MessageEdit::Remove { index }) => format!("remove {}", bits(*index)),
            Effect::Message(MessageEdit::Append { role, data, chat_id }) => format!("append {role:?} {} {}", value(data), units(chat_id)),
            Effect::Message(MessageEdit::Insert { index, role, data, chat_id }) =>
                format!("insert {} {role:?} {} {}", bits(*index), value(data), units(chat_id)),
            Effect::Message(MessageEdit::Replace { messages }) => format!("replace {}", value(messages)),
        }
    }

    #[test]
    fn a_copied_effect_keeps_every_field() {
        let budget = budget();
        let text = |item: &str| Text::from_str(item, &budget).unwrap();
        let json = |item: &str| parse_json(item, &budget).unwrap();
        let effects = [
            Effect::ChatVariable { name: text("a"), value: Value::Undefined },
            Effect::ChatVariable { name: Text::from_units(&[0x61, 0xd800], &budget).unwrap(), value: Value::Number(-0.0) },
            Effect::ChatField { field: text("hypaV3Data"), value: json(r#"{"1":[true,null,2.5],"b":{"c":"d"}}"#) },
            Effect::CharacterField { field: text("name"), value: text("N") },
            Effect::Message(MessageEdit::SetData { index: -1.0, data: Value::Number(f64::NAN) }),
            Effect::Message(MessageEdit::SetRole { index: 2.0, role: MessageRole::User }),
            Effect::Message(MessageEdit::Cut { start: f64::NAN, end: Some(3.0) }),
            Effect::Message(MessageEdit::Cut { start: 1.0, end: None }),
            Effect::Message(MessageEdit::Remove { index: 1.5 }),
            Effect::Message(MessageEdit::Append { role: MessageRole::Char, data: Value::Text(text("x")), chat_id: text("id-1") }),
            Effect::Message(MessageEdit::Insert { index: 0.0, role: MessageRole::User, data: json("[1,{}]"), chat_id: text("id-2") }),
            Effect::Message(MessageEdit::Replace { messages: json(r#"[{"role":"user","data":"y","chatId":"id-3"}]"#) }),
        ];
        for original in &effects { assert_eq!(effect(&original.copy(&budget).unwrap()), effect(original)); }
    }
}
