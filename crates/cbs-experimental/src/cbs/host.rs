// The host registrations of src/ts/cbs.ts over a captured database: the selected character with its loaded
// conversation or the call's prepared history access, the prompt texts and settings, lorebooks, modules, assets
// and model metadata, the clock in the captured local time, chat variable writes and hash picks. Without a
// database the matcher declines every entry here, so a caller that captured only variable reads falls back as before.
use super::families::{self, Call, ChatVar, Out};
use super::local_time::{self, time_clip};
use super::parser;
use super::processor::{text_units, CallArgs, Database, LocalTime, Platform};
use super::{array_ops, json_string, make_array, number_string, pick_hash_rand, scalar, split, to_string, units, Context, Json, Units};
use risunest_ir::clock::Clock;
use risunest_ir::chat::MessageRole;
use risunest_ir::stage::{MessageEdit, StageError};
use risunest_ir::{Budget as StageBudget, Charge, Text, Value};
use risunest_records::edits::{at_index, relative_index};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

// A message row with a string role, as JSON.stringify writes it. `data` is the stored value, which a trigger or
// Lua edit need not make a string.
struct Row {
    role: Units,
    data: Json,
    // `${time ?? 0}`.
    time: Units,
    // `time`; None is undefined.
    time_value: Option<Json>,
    fields: Vec<(Units, Json)>,
    _held: Option<Charge>,
}

struct Character {
    name: Units,
    nickname: Units,
    personality: Option<Units>,
    description: Option<Units>,
    scenario: Option<Units>,
    example_message: Option<Units>,
    first_message: Option<Units>,
    alternate_greetings: Vec<Units>,
    first_message_index: i64,
    // `chat.note` as `authornote` reads it; see `note_text`.
    note: Option<Units>,
    // chaId + (chat.id ?? ''), the pickHashRand word.
    hash_word: Units,
    // chat.message.length.
    count: usize,
    // None when a row is not an object with a string role and a data value; the families that read rows decline it.
    rows: Option<Vec<Arc<Row>>>,
    window_start: Option<usize>,
    // globalLore then localLore; None entries are undefined.
    lore: Vec<Option<Json>>,
    emotion_images: Option<Vec<Json>>,
    additional_assets: Option<Vec<Json>>,
    prebuilt_asset_command: bool,
    prebuilt_asset_exclude: Option<Vec<Option<Json>>>,
}

struct Module {
    // None is undefined.
    namespace: Option<Json>,
    assets: Option<Vec<Json>>,
}

struct Data {
    character: Option<Character>,
    persona_prompt: Units,
    main_prompt: Option<Units>,
    jailbreak: Option<Units>,
    global_note: Option<Units>,
    author_notes: Vec<Units>,
    model: Option<Units>,
    sub_model: Option<Units>,
    jailbreak_toggle: bool,
    max_context: f64,
    trigger_id: Option<Units>,
    viewport: (u32, u32),
    modules: Vec<Module>,
    module_lorebooks: Vec<Option<Json>>,
    model_info: Option<Json>,
    language: Option<Units>,
    platform: Platform,
    local_time: Option<LocalTime>,
}

// createCbsHistoryAccess over rows converted like the loaded ones; a row that is not a regular message declines.
struct Access {
    length: usize,
    absolute: bool,
    rows: HashMap<usize, Arc<Row>>,
    complete: Option<Vec<Arc<Row>>>,
    searches: HashMap<(Units, i64, i64), Vec<Arc<Row>>>,
    irregular: bool,
    _held: Option<Charge>,
}

// The CallArgs of the current call.
#[derive(Default)]
struct Invocation {
    projected_chat_id: Option<i64>,
    history_offset: Option<i64>,
    access: Option<Arc<Access>>,
    current_time: Option<f64>,
    trigger_id: Option<Units>,
    _held: Option<Charge>,
}

struct Note {
    text: Option<Units>,
    _held: Charge,
}

// The matcherArg fields a process() call sets.
#[derive(Clone, Default)]
pub(super) struct Flags {
    pub consistant_char: bool,
    pub run_var: bool,
    pub rm_var: bool,
    pub first_message: bool,
    pub role: Option<Units>,
    pub chat_role: Option<Units>,
}

// One setChatVar call: the value, None for undefined, and the stored value it replaced.
#[derive(Clone)]
pub(super) struct Write {
    pub key: Units,
    pub value: Option<Units>,
    previous: Option<Units>,
    // Whether the write replaced a value getChatVar could not read.
    unreadable: bool,
}

#[derive(Clone)]
pub struct Host {
    data: Arc<Data>,
    pub(super) flags: Flags,
    writes: Vec<Write>,
    clock: Arc<dyn Clock + Send + Sync>,
    call: Arc<Invocation>,
    history: Option<Arc<Access>>,
    note: Option<Arc<Note>>,
}
impl Host {
    pub fn has_character(&self) -> bool {
        self.data.character.is_some()
    }
}

// Own properties in stored order; JSON.stringify leaves out undefined members and writes undefined elements as null.
fn json(value: &Value) -> Json {
    match value {
        Value::Undefined | Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => Json::Num(*n),
        Value::Text(t) => Json::Str(text_units(t)),
        Value::Array(values) => Json::Arr(values.as_slice().iter().map(json).collect()),
        Value::Object(fields) => Json::Obj(
            fields.as_slice().iter().filter(|(_, v)| !matches!(v, Value::Undefined)).map(|(k, v)| (text_units(k), json(v))).collect(),
        ),
    }
}
// `json`, with undefined kept apart from null.
fn defined(value: &Value) -> Option<Json> {
    (!matches!(value, Value::Undefined)).then(|| json(value))
}
fn row(value: &Value) -> Option<Arc<Row>> {
    let Json::Obj(fields) = json(value) else { return None };
    let get = |name: &str| {
        let name = units(name);
        fields.iter().find(|(k, _)| *k == name).map(|(_, v)| v)
    };
    let (Some(Json::Str(role)), Some(data)) = (get("role"), get("data")) else { return None };
    let time_value = get("time").cloned();
    let time = match &time_value {
        None | Some(Json::Null) => units("0"),
        Some(v) => to_string(v),
    };
    let (role, data) = (role.clone(), data.clone());
    Some(Arc::new(Row { role, data, time, time_value, fields, _held: None }))
}

// parseKeyValue: each line's text before the first '=' and up to the next one, kept when both are non-empty.
fn parse_key_value(text: &[u16]) -> Vec<(Units, Units)> {
    split(text, &[10])
        .into_iter()
        .filter_map(|line| {
            let mut parts = line.split(|&c| c == 61);
            let (key, value) = (parts.next()?, parts.next()?);
            (!key.is_empty() && !value.is_empty()).then(|| (key.to_vec(), value.to_vec()))
        })
        .collect()
}

// The invocation context over `database`: chat variables are the stored values and the defaults getChatVar falls
// back to, and global reads take the chat's own value unless it is empty or 'null'.
pub(super) fn context(database: Database, user: Units, consistant_char: bool, clock: Arc<dyn Clock + Send + Sync>) -> Context {
    let Database {
        character, persona_prompt, main_prompt, jailbreak, global_note, author_notes, template_default_variables,
        mut global_variables, model, sub_model, jailbreak_toggle, max_context, trigger_id, viewport, modules,
        module_lorebooks, model_info, language, platform, local_time,
    } = database;
    let mut variables = HashMap::new();
    let mut unreadable = HashSet::new();
    let mut defaults = HashMap::new();
    let character = match character {
        None => None,
        Some(c) => {
            for (key, value) in c.chat.local_variables {
                if !value.is_empty() && value != units("null") {
                    global_variables.insert(key, value);
                }
            }
            for (key, value) in parse_key_value(&c.default_variables).into_iter().chain(parse_key_value(&template_default_variables)) {
                defaults.entry(key).or_insert(value);
            }
            variables = c.chat.variables;
            unreadable = c.chat.unreadable_variables;
            let mut hash_word = c.id;
            hash_word.extend(c.chat.id.unwrap_or_default());
            let entries = |values: Vec<Value>| values.iter().map(json).collect::<Vec<_>>();
            Some(Character {
                name: c.name,
                nickname: c.nickname,
                personality: c.personality,
                description: c.description,
                scenario: c.scenario,
                example_message: c.example_message,
                first_message: c.first_message,
                alternate_greetings: c.alternate_greetings,
                first_message_index: c.chat.first_message_index,
                note: note_text(c.chat.note.as_ref()),
                hash_word,
                count: c.chat.messages.len(),
                rows: c.chat.messages.iter().map(row).collect(),
                window_start: c.chat.window_start,
                lore: c.global_lore.iter().chain(&c.chat.local_lore).map(defined).collect(),
                emotion_images: c.emotion_images.map(entries),
                additional_assets: c.additional_assets.map(entries),
                prebuilt_asset_command: c.prebuilt_asset_command,
                prebuilt_asset_exclude: c.prebuilt_asset_exclude.map(|values| values.iter().map(defined).collect()),
            })
        }
    };
    let modules = modules
        .into_iter()
        .map(|m| Module { namespace: defined(&m.namespace), assets: m.assets.map(|a| a.iter().map(json).collect()) })
        .collect();
    let data = Data {
        character, persona_prompt, main_prompt, jailbreak, global_note, author_notes, model, sub_model, jailbreak_toggle,
        max_context, trigger_id, viewport, modules, module_lorebooks: module_lorebooks.iter().map(defined).collect(),
        model_info: model_info.as_ref().map(json), language, platform, local_time,
    };
    let host = Host {
        data: Arc::new(data),
        flags: Flags { consistant_char, ..Flags::default() },
        writes: Vec::new(),
        clock,
        call: Arc::default(),
        history: None,
        note: None,
    };
    Context { variables, unreadable, defaults, globals: global_variables, user, host: Some(Box::new(host)), ..Context::default() }
}

// The arg fields of the following calls.
pub(super) fn set_call(host: &mut Host, call: CallArgs) {
    let CallArgs { projected_chat_id, history_offset, history_access, current_time, trigger_id } = call;
    let access = history_access.map(|h| {
        let mut irregular = false;
        let mut rows = |values: &[Value]| -> Vec<Arc<Row>> {
            values.iter().filter_map(|v| row(v).or_else(|| (irregular = true, None).1)).collect()
        };
        let complete = h.complete.as_deref().map(&mut rows);
        let searches = h.searches.iter().map(|s| ((s.role.clone(), s.before, s.count), rows(&s.rows))).collect();
        let indexed: Vec<(usize, &Value)> = h.rows.iter().map(|(i, v)| (*i, v)).collect();
        let mut prepared = HashMap::new();
        for (index, value) in indexed {
            match row(value) {
                Some(r) => {
                    prepared.insert(index, r);
                }
                None => irregular = true,
            }
        }
        Arc::new(Access { length: h.length, absolute: true, rows: prepared, complete, searches, irregular, _held: None })
    });
    if access.is_some() { host.history = None; }
    let access = access.or_else(|| host.history.clone());
    host.call = Arc::new(Invocation { projected_chat_id, history_offset, access, current_time, trigger_id, _held: None });
}

fn unsupported(name: &'static str) -> StageError { StageError::Unsupported(name.into()) }

fn units_bytes(value: &[u16], budget: &StageBudget) -> Result<usize, StageError> {
    budget.work(value.len().saturating_add(1))?;
    Ok(value.len().saturating_mul(size_of::<u16>()))
}
fn json_bytes(value: &Json, budget: &StageBudget) -> Result<usize, StageError> {
    budget.work(1)?;
    Ok(match value {
        Json::Str(value) => units_bytes(value, budget)?,
        Json::Arr(values) => {
            let mut bytes = values.len().saturating_mul(size_of::<Json>());
            for value in values { bytes = bytes.saturating_add(json_bytes(value, budget)?); }
            bytes
        }
        Json::Obj(fields) => {
            let mut bytes = fields.len().saturating_mul(size_of::<(Units, Json)>());
            for (key, value) in fields {
                bytes = bytes.saturating_add(units_bytes(key, budget)?).saturating_add(json_bytes(value, budget)?);
            }
            bytes
        }
        _ => 0,
    })
}

// `message.data = undefined` keeps the key with no value, which a JSON row cannot hold.
fn message_data(data: &Value) -> Result<Json, StageError> {
    if matches!(data, Value::Undefined) { return Err(unsupported("message-undefined")); }
    Ok(json(data))
}

fn edited_row(row: &Row, data: &Value, budget: &StageBudget) -> Result<Arc<Row>, StageError> {
    let data = message_data(data)?;
    let mut bytes = size_of::<Row>().saturating_add(2 * size_of::<usize>()).saturating_add(json_bytes(&data, budget)?)
        .saturating_add(units_bytes(&row.role, budget)?).saturating_add(units_bytes(&row.time, budget)?)
        .saturating_add(row.fields.len().saturating_mul(size_of::<(Units, Json)>()));
    if let Some(value) = &row.time_value { bytes = bytes.saturating_add(json_bytes(value, budget)?); }
    for (key, value) in &row.fields {
        bytes = bytes.saturating_add(units_bytes(key, budget)?).saturating_add(
            json_bytes(if key == &[100, 97, 116, 97] { &data } else { value }, budget)?);
    }
    let held = budget.reserve_owned(bytes)?;
    let fields = row.fields.iter().map(|(key, value)| (key.clone(),
        if key == &[100, 97, 116, 97] { data.clone() } else { value.clone() })).collect();
    Ok(Arc::new(Row { role: row.role.clone(), data, time: row.time.clone(), time_value: row.time_value.clone(), fields,
        _held: Some(held) }))
}

fn appended_row(role: MessageRole, data: &Value, chat_id: &Text, budget: &StageBudget) -> Result<Arc<Row>, StageError> {
    let data = message_data(data)?;
    let role = if role == MessageRole::User { "user" } else { "char" };
    let units = role.len().saturating_mul(2).saturating_add(chat_id.len_units()).saturating_add(4 + 4 + 6 + 1);
    budget.work(units)?;
    // The row and its `data` field each hold a copy.
    let data_bytes = json_bytes(&data, budget)?.saturating_mul(2);
    let held = budget.reserve_owned(size_of::<Row>().saturating_add(2 * size_of::<usize>())
        .saturating_add(3 * size_of::<(Units, Json)>()).saturating_add(units.saturating_mul(size_of::<u16>()))
        .saturating_add(data_bytes))?;
    let (role, chat_id) = (super::units(role), text_units(chat_id));
    let fields = vec![(super::units("role"), Json::Str(role.clone())), (super::units("data"), data.clone()),
        (super::units("chatId"), Json::Str(chat_id))];
    Ok(Arc::new(Row { role, data, time: super::units("0"), time_value: None, fields, _held: Some(held) }))
}

// `message.role = role`: the `role` field keeps its place.
fn role_row(row: &Row, role: MessageRole, budget: &StageBudget) -> Result<Arc<Row>, StageError> {
    let role = super::units(if role == MessageRole::User { "user" } else { "char" });
    let mut bytes = size_of::<Row>().saturating_add(2 * size_of::<usize>()).saturating_add(json_bytes(&row.data, budget)?)
        .saturating_add(units_bytes(&role, budget)?).saturating_add(units_bytes(&row.time, budget)?)
        .saturating_add(row.fields.len().saturating_mul(size_of::<(Units, Json)>()));
    if let Some(value) = &row.time_value { bytes = bytes.saturating_add(json_bytes(value, budget)?); }
    for (key, value) in &row.fields {
        bytes = bytes.saturating_add(units_bytes(key, budget)?).saturating_add(
            if key == &[114, 111, 108, 101] { units_bytes(&role, budget)? } else { json_bytes(value, budget)? });
    }
    let held = budget.reserve_owned(bytes)?;
    let fields = row.fields.iter().map(|(key, value)| (key.clone(),
        if key == &[114, 111, 108, 101] { Json::Str(role.clone()) } else { value.clone() })).collect();
    Ok(Arc::new(Row { role, data: row.data.clone(), time: row.time.clone(), time_value: row.time_value.clone(), fields,
        _held: Some(held) }))
}

// A message of an assigned array as `row` reads a loaded one, charged; None when it is not a regular message.
fn assigned_row(value: &Value, budget: &StageBudget) -> Result<Option<Arc<Row>>, StageError> {
    budget.work(1)?;
    let Some(row) = row(value) else { return Ok(None) };
    let mut row = Arc::into_inner(row).expect("a new row");
    let mut bytes = size_of::<Row>().saturating_add(2 * size_of::<usize>()).saturating_add(json_bytes(&row.data, budget)?)
        .saturating_add(units_bytes(&row.role, budget)?).saturating_add(units_bytes(&row.time, budget)?)
        .saturating_add(row.fields.len().saturating_mul(size_of::<(Units, Json)>()));
    if let Some(value) = &row.time_value { bytes = bytes.saturating_add(json_bytes(value, budget)?); }
    for (key, value) in &row.fields { bytes = bytes.saturating_add(units_bytes(key, budget)?).saturating_add(json_bytes(value, budget)?); }
    row._held = Some(budget.reserve_owned(bytes)?);
    Ok(Some(Arc::new(row)))
}

// Message rows are shared until one changes. Only the pointer array and changed or added rows are copied; captured
// searches are dropped because the complete updated history can answer them without stale results. An assigned array
// with a message that is not a regular one leaves a history whose rows decline as `history-row`, as a loaded one does.
pub(super) fn apply_message(host: &mut Host, edit: &MessageEdit, budget: &StageBudget) -> Result<(), StageError> {
    budget.check()?;
    let character = host.data.character.as_ref().ok_or_else(|| unsupported("character"))?;
    let access = host.call.access.as_deref();
    let source = match access {
        Some(access) => {
            if access.irregular { return Err(unsupported("history-row")); }
            let complete = access.complete.as_deref().ok_or_else(|| unsupported("history-unprepared"))?;
            if complete.len() != access.length { return Err(unsupported("history-unprepared")); }
            complete
        }
        None => {
            if character.window_start.unwrap_or(0) != 0 { return Err(unsupported("history-window")); }
            character.rows.as_deref().ok_or_else(|| unsupported("history-row"))?
        }
    };
    let length = source.len();
    let get = |i| access.and_then(|access| access.rows.get(&i)).unwrap_or(&source[i]);
    // The rows `start..end` keep, with `replacement` in place of one, `removed` left out and `inserted` before the row at
    // its index (after the last at `end`); or the `assigned` array instead.
    let (start, end, replacement, removed, mut inserted, assigned) = match edit {
        MessageEdit::SetData { index, data } => {
            let Some(index) = at_index(*index, length) else { return Ok(()) };
            (0, length, Some((index, edited_row(get(index), data, budget)?)), None, None, None)
        }
        MessageEdit::SetRole { index, role } => {
            let Some(index) = at_index(*index, length) else { return Ok(()) };
            (0, length, Some((index, role_row(get(index), *role, budget)?)), None, None, None)
        }
        MessageEdit::Cut { start, end } => {
            let start = relative_index(*start, length);
            let end = end.map_or(length, |end| relative_index(end, length)).max(start);
            (start, end, None, None, None, None)
        }
        // `splice(index, 1)` and `splice(index, 0, message)`.
        MessageEdit::Remove { index } => (0, length, None, Some(relative_index(*index, length)).filter(|&at| at < length), None, None),
        MessageEdit::Append { role, data, chat_id } =>
            (0, length, None, None, Some((length, appended_row(*role, data, chat_id, budget)?)), None),
        MessageEdit::Insert { index, role, data, chat_id } =>
            (0, length, None, None, Some((relative_index(*index, length), appended_row(*role, data, chat_id, budget)?)), None),
        MessageEdit::Replace { messages: Value::Array(values) } => (0, 0, None, None, None, Some(values.as_slice())),
        MessageEdit::Replace { .. } => return Err(StageError::Invalid("message-replace".into())),
        _ => return Err(unsupported("message-effect")),
    };
    let count = assigned.map_or(end - start + usize::from(inserted.is_some()) - usize::from(removed.is_some()), <[Value]>::len);
    budget.work(count)?;
    let held = budget.reserve_owned(size_of::<Access>().saturating_add(2 * size_of::<usize>())
        .saturating_add(count.saturating_mul(size_of::<Arc<Row>>())))?;
    let id_bytes = host.call.trigger_id.as_deref().map(|id| units_bytes(id, budget)).transpose()?.unwrap_or(0);
    let call_held = budget.reserve_owned(size_of::<Invocation>().saturating_add(2 * size_of::<usize>()).saturating_add(id_bytes))?;
    let mut complete = Vec::new();
    complete.try_reserve_exact(count).map_err(|_| StageError::Limit("allocation".into()))?;
    let mut irregular = false;
    for value in assigned.unwrap_or(&[]) {
        match assigned_row(value, budget)? {
            Some(row) => complete.push(row),
            None => irregular = true,
        }
    }
    for i in start..end {
        if inserted.as_ref().is_some_and(|(at, _)| *at == i) { complete.push(inserted.take().expect("inserted").1); }
        if removed != Some(i) { complete.push(get(i).clone()); }
    }
    if let Some((index, row)) = replacement { complete[index] = row; }
    if let Some((_, row)) = inserted { complete.push(row); }
    let access = Arc::new(Access { length: count, absolute: access.is_some_and(|access| access.absolute),
        rows: HashMap::new(), complete: (!irregular).then_some(complete), searches: HashMap::new(),
        irregular, _held: Some(held) });
    let call = Arc::new(Invocation { projected_chat_id: host.call.projected_chat_id, history_offset: host.call.history_offset,
        access: Some(access.clone()), current_time: host.call.current_time, trigger_id: host.call.trigger_id.clone(), _held: Some(call_held) });
    host.history = Some(access);
    host.call = call;
    Ok(())
}

pub(super) fn apply_note(host: &mut Host, value: &Value, budget: &StageBudget) -> Result<(), StageError> {
    if !host.has_character() { return Err(unsupported("character")); }
    let len = match value { Value::Text(text) => text.len_units(), _ => 0 };
    budget.work(len)?;
    let held = budget.reserve_owned(size_of::<Note>().saturating_add(2 * size_of::<usize>())
        .saturating_add(len.saturating_mul(size_of::<u16>())))?;
    host.note = Some(Arc::new(Note { text: note_text(Some(value)), _held: held }));
    Ok(())
}

// A note as `authornote` reads it: a falsy value is an empty note, and None is a truthy non-string.
fn note_text(value: Option<&Value>) -> Option<Units> {
    match value {
        Some(Value::Text(text)) => Some(text_units(text)),
        Some(value) if value.truthy() => None,
        _ => Some(Vec::new()),
    }
}

// The writes of the current call, in TS order.
pub(super) fn writes(context: &Context) -> &[Write] {
    context.host.as_deref().map_or(&[][..], |h| &h.writes[..])
}
// Ends a call: a kept call leaves its writes in the variables, a declined one restores the values they replaced.
pub(super) fn finish(context: &mut Context, keep: bool) {
    let Some(host) = context.host.as_deref_mut() else { return };
    for write in host.writes.drain(..).rev() {
        if keep {
            continue;
        }
        if write.unreadable {
            context.unreadable.insert(write.key.clone());
        }
        match write.previous {
            Some(value) => context.variables.insert(write.key, value),
            None => context.variables.remove(&write.key),
        };
    }
}

fn host<'c>(c: &'c Call) -> Result<&'c Host, String> {
    c.env.context.host.as_deref().ok_or_else(|| "unsupported-operator".into())
}
fn data(c: &Call) -> Result<Arc<Data>, String> {
    host(c).map(|h| h.data.clone())
}
fn invocation(c: &Call) -> Result<Arc<Invocation>, String> {
    host(c).map(|h| h.call.clone())
}
fn flags<'c>(c: &'c Call) -> Result<&'c Flags, String> {
    host(c).map(|h| &h.flags)
}
fn text(v: &[u16]) -> Result<Out, String> {
    Ok(Out::Text(v.to_vec()))
}
fn rows(c: &Character) -> Result<&[Arc<Row>], String> {
    c.rows.as_deref().ok_or_else(|| "history-row".into())
}
// risuChatParser(text, matcherArg).
fn reparse(c: &mut Call, text: Option<&[u16]>) -> Result<Out, String> {
    parser::reparse(c.env, c.matcher, text)
}
// chat.fmIndex === -1 ? firstMessage : alternateGreetings[fmIndex]; None is undefined.
fn greeting(c: &Character) -> Option<&Units> {
    match c.first_message_index {
        -1 => c.first_message.as_ref(),
        i => usize::try_from(i).ok().and_then(|i| c.alternate_greetings.get(i)),
    }
}
// The greeting the previous-message families fall back to, which reads the chat and throws without one.
fn greeting_or_preserve(character: Option<&Character>) -> Out {
    character.and_then(greeting).map_or(Out::Preserve, |g| Out::Text(g.clone()))
}

// --- the history a call reads ------------------------------------------------
//
// With a history access the rows, counts and searches come from it and an unprepared read declines as
// `history-unprepared`; otherwise they come from the loaded conversation, whose array starts at the window start.

impl Access {
    fn checked(&self) -> Result<&Self, String> {
        if self.irregular {
            return Err("history-row".into());
        }
        Ok(self)
    }
}
// historyRow(chat, index): row(index) of the access, else chat?.message[index].
fn history_row<'a>(access: Option<&'a Access>, character: Option<&'a Character>, index: f64) -> Result<Option<&'a Row>, String> {
    let integral = index.fract() == 0.0 && index >= 0.0;
    if let Some(access) = access {
        let access = access.checked()?;
        if !integral || index >= access.length as f64 {
            return Ok(None);
        }
        let i = index as usize;
        return match access.rows.get(&i).or_else(|| access.complete.as_ref().and_then(|c| c.get(i))) {
            Some(row) => Ok(Some(row)),
            None => Err("history-unprepared".into()),
        };
    }
    let Some(character) = character else { return Ok(None) };
    let rows = rows(character)?;
    // A property key of -0 is "0", and any other index outside the array reads undefined.
    Ok(if integral && index < rows.len() as f64 { Some(&rows[index as usize]) } else { None })
}
// historyRows(chat): all() of the access, else chat.message, which throws without a chat.
fn history_rows<'a>(access: Option<&'a Access>, character: Option<&'a Character>) -> Result<Option<&'a [Arc<Row>]>, String> {
    match (access, character) {
        (Some(access), _) => access.checked()?.complete.as_deref().map(Some).ok_or_else(|| "history-unprepared".into()),
        (None, Some(character)) => rows(character).map(Some),
        (None, None) => Ok(None),
    }
}
// historyCount: the access length, else the window start plus the loaded length; None throws.
fn history_count(access: Option<&Access>, character: Option<&Character>) -> Option<f64> {
    match (access, character) {
        (Some(access), _) => Some(access.length as f64),
        (None, Some(c)) => Some((c.window_start.unwrap_or(0) + c.count) as f64),
        (None, None) => None,
    }
}
// loadedHistoryCount: the access length, else the loaded length; None throws.
fn loaded_count(access: Option<&Access>, character: Option<&Character>) -> Option<usize> {
    match (access, character) {
        (Some(access), _) => Some(access.length),
        (None, c) => c.map(|c| c.count),
    }
}
fn window_start(character: Option<&Character>) -> i64 {
    character.and_then(|c| c.window_start).unwrap_or(0) as i64
}
// historyChatID: the chat index with an access, else projectedChatID ?? chatID less the window start, -1 kept.
fn history_chat_id(c: &Call, call: &Invocation, character: Option<&Character>) -> i64 {
    let index = c.env.context.index;
    if call.access.as_deref().is_some_and(|access| access.absolute) {
        return index;
    }
    match call.projected_chat_id.unwrap_or(index) {
        -1 => -1,
        id => id - window_start(character),
    }
}
// previousHistoryRows(chat, role, before, count). Ok(None) is a TypeError from reading a row past the end.
fn previous_rows<'a>(
    c: &mut Call,
    access: Option<&'a Access>,
    character: Option<&'a Character>,
    role: &str,
    before: i64,
    count: i64,
) -> Result<Option<Vec<&'a Row>>, String> {
    let role = units(role);
    let mut found = Vec::new();
    let (rows, mut index) = match access {
        Some(access) => {
            let access = access.checked()?;
            if before <= 0 {
                return Ok(Some(found));
            }
            if let Some(prepared) = access.searches.get(&(role.clone(), before, count)) {
                return Ok(Some(prepared.iter().map(AsRef::as_ref).collect()));
            }
            let complete = access.complete.as_deref().ok_or("history-unprepared")?;
            (Some(complete), if access.absolute { before.min(access.length as i64) - 1 } else { before.saturating_sub(1) })
        }
        None => (character.map(rows).transpose()?, before.saturating_sub(1)),
    };
    while index >= 0 && (found.len() as i64) < count {
        c.env.budget.tick()?;
        let Some(row) = rows.and_then(|rows| usize::try_from(index).ok().and_then(|i| rows.get(i))) else { return Ok(None) };
        if row.role == role {
            found.push(row.as_ref());
        }
        index -= 1;
    }
    Ok(Some(found))
}

// A message time: !message.time reads missing, and a time other than a number declines rather than coerce.
enum MessageTime {
    Missing,
    At(f64),
    Other,
}
fn message_time(row: &Row) -> MessageTime {
    match &row.time_value {
        None | Some(Json::Null) | Some(Json::Bool(false)) => MessageTime::Missing,
        Some(Json::Num(n)) if *n == 0.0 || n.is_nan() => MessageTime::Missing,
        Some(Json::Str(s)) if s.is_empty() => MessageTime::Missing,
        Some(Json::Num(n)) => MessageTime::At(*n),
        Some(_) => MessageTime::Other,
    }
}
const OLDER: &str = "[Cannot get time, message was sent in older version]";

// `data` returned as the callback result (parser.svelte.ts:2022-2032): a string is written and another falsy value
// keeps the tag. A truthy one is read as `{text, var}`: a missing `var` throws out of the parse, and one that holds
// a value declines.
fn stored_data(data: &Json) -> Result<Out, String> {
    Ok(match data {
        Json::Str(data) => Out::Text(data.clone()),
        Json::Null | Json::Bool(false) => Out::Preserve,
        Json::Num(n) if *n == 0.0 || n.is_nan() => Out::Preserve,
        Json::Obj(fields) if fields.iter().any(|(k, v)| k == &[118, 97, 114] && !matches!(v, Json::Null)) => {
            return Err("matcher-result-var".into());
        }
        _ => return Err("invalid:matcher-result".into()),
    })
}

// A cloned row with `data` replaced, as JSON.
fn row_json(fields: &[(Units, Json)], data: Units) -> Units {
    let key = units("data");
    let mut data = Some(data);
    let fields = fields
        .iter()
        .map(|(k, v)| if *k == key { (k.clone(), Json::Str(data.take().unwrap_or_default())) } else { (k.clone(), v.clone()) })
        .collect();
    json_string(&Json::Obj(fields))
}
// makeArray over `items` then the rows mapped by `item`, bounded by the output limit.
fn row_array(
    c: &mut Call,
    mut items: Vec<Units>,
    rows: &[&Row],
    mut item: impl FnMut(&mut Call, &Row) -> Result<Option<Units>, String>,
) -> Result<Out, String> {
    let mut total = items.iter().map(Vec::len).sum::<usize>();
    for row in rows {
        c.env.budget.tick()?;
        let Some(value) = item(c, row)? else { return Ok(Out::Preserve) };
        total = total.saturating_add(value.len());
        if total > c.env.budget.max_output {
            return Err("output-limit".into());
        }
        items.push(value);
    }
    Ok(Out::Text(make_array(items)))
}
// The row's data reparsed into the cloned row, as userhistory, charhistory and history write it. `risuChatParser`
// counts data other than a string toward the call stack limit as it does undefined, then throws on it.
fn parsed_row(c: &mut Call, row: &Row) -> Result<Option<Units>, String> {
    let data = match &row.data {
        Json::Str(data) => Some(data.as_slice()),
        _ => None,
    };
    Ok(match reparse(c, data)? {
        Out::Text(data) => Some(row_json(&row.fields, data)),
        _ => None,
    })
}

pub(super) fn char_(c: &mut Call) -> Result<Out, String> {
    if flags(c)?.consistant_char {
        return text(&units("botname"));
    }
    let data = data(c)?;
    Ok(match &data.character {
        Some(character) if character.nickname.is_empty() => Out::Text(character.name.clone()),
        Some(character) => Out::Text(character.nickname.clone()),
        // tokenizeAccurate makes the arg's chara 'bot' when no character is selected.
        None if c.env.options.tokenize_accurate => Out::Text(units("bot")),
        None => Out::Preserve,
    })
}
// The arg's triggerId once it sets currentTime, else CurrentTriggerIdStore.
pub(super) fn trigger_id(c: &mut Call) -> Result<Out, String> {
    if let Some(scope) = c.env.scope.as_deref() {
        return Ok(Out::Text(scope.trigger_id().unwrap_or_else(|| units("null"))));
    }
    let call = invocation(c)?;
    let id = if call.current_time.is_some() { call.trigger_id.clone() } else { data(c)?.trigger_id.clone() };
    Ok(Out::Text(id.unwrap_or_else(|| units("null"))))
}
pub(super) fn previouscharchat(c: &mut Call) -> Result<Out, String> {
    let (data, call) = (data(c)?, invocation(c)?);
    let (character, access) = (data.character.as_ref(), call.access.as_deref());
    let current = history_chat_id(c, &call, character);
    let before = match current {
        -1 => match loaded_count(access, character) {
            Some(count) => count as i64,
            None => return Ok(Out::Preserve),
        },
        index => index,
    };
    Ok(match previous_rows(c, access, character, "char", before, 1)? {
        None => Out::Preserve,
        Some(found) => match found.first() {
            Some(row) => stored_data(&row.data)?,
            None => greeting_or_preserve(character),
        },
    })
}
pub(super) fn previoususerchat(c: &mut Call) -> Result<Out, String> {
    let (data, call) = (data(c)?, invocation(c)?);
    let (character, access) = (data.character.as_ref(), call.access.as_deref());
    let chat_id = history_chat_id(c, &call, character);
    if chat_id == -1 {
        return text(&[]);
    }
    Ok(match previous_rows(c, access, character, "user", chat_id, 1)? {
        None => Out::Preserve,
        Some(found) => match found.first() {
            Some(row) => stored_data(&row.data)?,
            None => greeting_or_preserve(character),
        },
    })
}
fn character_text(c: &mut Call, field: fn(&Character) -> Option<&Units>) -> Result<Out, String> {
    let data = data(c)?;
    let Some(character) = &data.character else { return Ok(Out::Preserve) };
    reparse(c, field(character).map(Vec::as_slice))
}
pub(super) fn personality(c: &mut Call) -> Result<Out, String> {
    character_text(c, |c| c.personality.as_ref())
}
pub(super) fn description(c: &mut Call) -> Result<Out, String> {
    character_text(c, |c| c.description.as_ref())
}
pub(super) fn scenario(c: &mut Call) -> Result<Out, String> {
    character_text(c, |c| c.scenario.as_ref())
}
pub(super) fn exampledialogue(c: &mut Call) -> Result<Out, String> {
    character_text(c, |c| c.example_message.as_ref())
}
pub(super) fn persona(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    reparse(c, Some(&data.persona_prompt))
}
pub(super) fn mainprompt(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    reparse(c, data.main_prompt.as_deref())
}
pub(super) fn jb(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    reparse(c, data.jailbreak.as_deref())
}
pub(super) fn globalnote(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    reparse(c, data.global_note.as_deref())
}
pub(super) fn authornote(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    let applied = host(c)?.note.clone();
    let note = match applied.as_deref() {
        Some(note) => Some(note.text.as_ref()),
        None => data.character.as_ref().map(|c| c.note.as_ref()),
    };
    match note {
        // A truthy non-string: `risuChatParser` throws on it inside the callback, so the matcher keeps the tag.
        Some(None) => return Ok(Out::Preserve),
        Some(Some(note)) if !note.is_empty() => return reparse(c, Some(note)),
        _ => {}
    }
    match data.author_notes.iter().find(|t| !t.is_empty()) {
        Some(note) => reparse(c, Some(note)),
        None => text(&[]),
    }
}
fn history_by_role(c: &mut Call, role: &str) -> Result<Out, String> {
    let (data, call) = (data(c)?, invocation(c)?);
    let Some(all) = history_rows(call.access.as_deref(), data.character.as_ref())? else { return Ok(Out::Preserve) };
    let role = units(role);
    let selected: Vec<&Row> = all.iter().filter(|r| r.role == role).map(AsRef::as_ref).collect();
    row_array(c, Vec::new(), &selected, parsed_row)
}
pub(super) fn userhistory(c: &mut Call) -> Result<Out, String> {
    history_by_role(c, "user")
}
pub(super) fn charhistory(c: &mut Call) -> Result<Out, String> {
    history_by_role(c, "char")
}
pub(super) fn firstmsgindex(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    Ok(data.character.as_ref().map_or(Out::Preserve, |c| Out::Text(units(&c.first_message_index.to_string()))))
}
pub(super) fn messageunixtimearray(c: &mut Call) -> Result<Out, String> {
    let (data, call) = (data(c)?, invocation(c)?);
    let Some(all) = history_rows(call.access.as_deref(), data.character.as_ref())? else { return Ok(Out::Preserve) };
    let all: Vec<&Row> = all.iter().map(AsRef::as_ref).collect();
    row_array(c, Vec::new(), &all, |_, row| Ok(Some(row.time.clone())))
}
pub(super) fn model(c: &mut Call) -> Result<Out, String> {
    Ok(data(c)?.model.clone().map_or(Out::Preserve, Out::Text))
}
pub(super) fn axmodel(c: &mut Call) -> Result<Out, String> {
    Ok(data(c)?.sub_model.clone().map_or(Out::Preserve, Out::Text))
}
pub(super) fn role(c: &mut Call) -> Result<Out, String> {
    let flags = flags(c)?.clone();
    if let Some(role) = flags.chat_role.filter(|r| !r.is_empty()) {
        return Ok(Out::Text(role));
    }
    if flags.first_message {
        return text(&units("char"));
    }
    if c.env.context.index != -1 {
        let (data, call) = (data(c)?, invocation(c)?);
        let character = data.character.as_ref();
        let index = history_chat_id(c, &call, character) as f64;
        return Ok(history_row(call.access.as_deref(), character, index)?.map_or(Out::Preserve, |row| Out::Text(row.role.clone())));
    }
    Ok(Out::Text(flags.role.unwrap_or_else(|| units("null"))))
}
pub(super) fn isfirstmsg(c: &mut Call) -> Result<Out, String> {
    text(&units(if flags(c)?.first_message { "1" } else { "0" }))
}
pub(super) fn jbtoggled(c: &mut Call) -> Result<Out, String> {
    text(&units(if data(c)?.jailbreak_toggle { "1" } else { "0" }))
}
pub(super) fn maxcontext(c: &mut Call) -> Result<Out, String> {
    Ok(Out::Text(number_string(data(c)?.max_context)))
}
pub(super) fn lastmessage(c: &mut Call) -> Result<Out, String> {
    let (data, call) = (data(c)?, invocation(c)?);
    let Some(character) = &data.character else { return text(&[]) };
    let access = call.access.as_deref();
    let last = loaded_count(access, Some(character)).unwrap_or(0) as f64 - 1.0;
    match history_row(access, Some(character), last)? {
        Some(row) => stored_data(&row.data),
        None => Ok(Out::Preserve),
    }
}
pub(super) fn lastmessageid(c: &mut Call) -> Result<Out, String> {
    let (data, call) = (data(c)?, invocation(c)?);
    let Some(character) = &data.character else { return text(&[]) };
    let count = history_count(call.access.as_deref(), Some(character)).unwrap_or(0.0);
    Ok(Out::Text(number_string(count - 1.0)))
}

// --- chat variable writes ----------------------------------------------------

// rmVar answers '' and a call without runVar keeps the macro.
fn write_gate(c: &Call) -> Result<Option<Out>, String> {
    let flags = flags(c)?;
    Ok(if flags.rm_var {
        Some(Out::Text(Vec::new()))
    } else if !flags.run_var {
        Some(Out::Preserve)
    } else {
        None
    })
}
// setChatVar(key, value) on the selected chat, which throws when no character is selected; `'$' + undefined`
// stores under 'undefined'.
fn set_chat_var(c: &mut Call, key: Option<&Units>, value: Option<&Units>) -> Result<Out, String> {
    if let Some(scope) = c.env.scope.as_deref_mut() {
        scope.set(key.map(Vec::as_slice), value.map(Vec::as_slice))?;
        return text(&[]);
    }
    let context = &mut *c.env.context;
    let host = context.host.as_deref_mut().ok_or("unsupported-operator")?;
    if !host.has_character() {
        return Ok(Out::Preserve);
    }
    let key = key.cloned().unwrap_or_else(|| units("undefined"));
    let value = value.cloned();
    let previous = match &value {
        Some(value) => context.variables.insert(key.clone(), value.clone()),
        None => context.variables.remove(&key),
    };
    let unreadable = context.unreadable.remove(&key);
    host.writes.push(Write { key, value, previous, unreadable });
    text(&[])
}
pub(super) fn setvar(c: &mut Call) -> Result<Out, String> {
    if let Some(out) = write_gate(c)? {
        return Ok(out);
    }
    set_chat_var(c, c.args.first(), c.args.get(1))
}
pub(super) fn addvar(c: &mut Call) -> Result<Out, String> {
    if let Some(out) = write_gate(c)? {
        return Ok(out);
    }
    let current = match c.env.chat_var(c.args.first().map(Vec::as_slice))? {
        ChatVar::Text(current) => scalar::number(Some(&current), Some(c.env.budget))?,
        // `Number(value)` throws inside the matcher's `try`, which keeps the tag.
        ChatVar::Other { .. } => match c.env.chat_var_number(c.args.first().map(Vec::as_slice))? {
            Some(number) => number,
            None => return Ok(Out::Preserve),
        },
    };
    let sum = current + c.number(1)?;
    set_chat_var(c, c.args.first(), Some(&number_string(sum)))
}
pub(super) fn setdefaultvar(c: &mut Call) -> Result<Out, String> {
    if let Some(out) = write_gate(c)? {
        return Ok(out);
    }
    // `!currentValue || currentValue === 'null'`.
    let unset = match c.env.chat_var(c.args.first().map(Vec::as_slice))? {
        ChatVar::Text(current) => current.is_empty() || current == units("null"),
        ChatVar::Other { truthy, .. } => !truthy,
    };
    if unset {
        return set_chat_var(c, c.args.first(), c.args.get(1));
    }
    text(&[])
}
// historyRow(chat, projectedHistoryIndex(Number(arg)))?.data ?? 'Out of range'.
pub(super) fn previouschatlog(c: &mut Call) -> Result<Out, String> {
    let (data, call) = (data(c)?, invocation(c)?);
    let index = c.number(0)?;
    let (character, access) = (data.character.as_ref(), call.access.as_deref());
    let projected = if access.is_some_and(|access| access.absolute) { index }
        else { index - call.history_offset.unwrap_or(0) as f64 - window_start(character) as f64 };
    match history_row(access, character, projected)? {
        Some(row) if !matches!(row.data, Json::Null) => stored_data(&row.data),
        _ => Ok(Out::Text(units("Out of range"))),
    }
}
pub(super) fn history(c: &mut Call) -> Result<Out, String> {
    let (data, call) = (data(c)?, invocation(c)?);
    let (character, access) = (data.character.as_ref(), call.access.as_deref());
    if c.args.is_empty() {
        let Some(character) = character else { return Ok(Out::Preserve) };
        let Some(all) = history_rows(access, Some(character))? else { return Ok(Out::Preserve) };
        let all: Vec<&Row> = all.iter().map(AsRef::as_ref).collect();
        // A window that starts after the first message leaves the greeting out.
        if character.window_start.unwrap_or(0) > 0 {
            return row_array(c, Vec::new(), &all, parsed_row);
        }
        let Out::Text(first) = reparse(c, greeting(character).map(Vec::as_slice))? else { return Ok(Out::Preserve) };
        let greeting = json_string(&Json::Obj(vec![(units("role"), Json::Str(units("char"))), (units("data"), Json::Str(first))]));
        return row_array(c, vec![greeting], &all, parsed_row);
    }
    let Some(all) = history_rows(access, character)? else { return Ok(Out::Preserve) };
    let all: Vec<&Row> = all.iter().map(AsRef::as_ref).collect();
    let with_role = c.args.iter().any(|a| *a == units("role"));
    row_array(c, Vec::new(), &all, |_, row| {
        let data = data_text(&row.data)?;
        Ok(Some(if with_role { [&row.role[..], &units(": "), &data].concat() } else { data }))
    })
}
// `'' + data` for a string, number, boolean, null or an array of those. An object declines: whether its
// conversion throws depends on its own keys.
fn data_text(data: &Json) -> Result<Units, String> {
    fn holds_object(value: &Json) -> bool {
        match value {
            Json::Obj(_) => true,
            Json::Arr(values) => values.iter().any(holds_object),
            _ => false,
        }
    }
    if holds_object(data) {
        return Err("message-data-object".into());
    }
    Ok(to_string(data))
}

// --- clock and message times -------------------------------------------------

fn local(data: &Data) -> Result<&LocalTime, String> {
    data.local_time.as_ref().ok_or_else(|| "local-time".into())
}
// new Date(currentTime ?? Date.now()).
fn now(c: &Call, call: &Invocation) -> Result<f64, String> {
    let clock = &host(c)?.clock;
    Ok(time_clip(call.current_time.unwrap_or_else(|| clock.now_ms())))
}
// ${hours}:${minutes}:${seconds} or ${year}-${month + 1}-${date}, NaN for an invalid Date.
fn field_text(values: [f64; 3]) -> Units {
    let [a, b, c] = values.map(number_string);
    [a, units(":"), b, units(":"), c].concat()
}
fn date_text(values: [f64; 3]) -> Units {
    let [a, b, c] = values.map(number_string);
    [a, units("-"), b, units("-"), c].concat()
}
fn message_text(c: &mut Call, date: bool) -> Result<Out, String> {
    if c.env.options.tokenize_accurate {
        return text(&units("00:00:00"));
    }
    if c.env.context.index == -1 {
        return text(&units("[Cannot get time]"));
    }
    let (data, call) = (data(c)?, invocation(c)?);
    let character = data.character.as_ref();
    let index = history_chat_id(c, &call, character) as f64;
    let Some(row) = history_row(call.access.as_deref(), character, index)? else { return Ok(Out::Preserve) };
    let time = match message_time(row) {
        MessageTime::Missing => return text(&units(OLDER)),
        MessageTime::Other => return Err("time-value".into()),
        MessageTime::At(time) => time,
    };
    let local = local(&data)?;
    let pattern = if date { local.date_format.as_ref() } else { local.time_format.as_ref() };
    local_time::locale_string(&local.zone, pattern, time).map(Out::Text)
}
pub(super) fn messagetime(c: &mut Call) -> Result<Out, String> {
    message_text(c, false)
}
pub(super) fn messagedate(c: &mut Call) -> Result<Out, String> {
    message_text(c, true)
}
pub(super) fn unixtime(c: &mut Call) -> Result<Out, String> {
    let call = invocation(c)?;
    Ok(Out::Text(local_time::to_fixed0(now(c, &call)? / 1000.0)))
}
pub(super) fn isotime(c: &mut Call) -> Result<Out, String> {
    let call = invocation(c)?;
    let f = local_time::utc_fields(now(c, &call)?);
    Ok(Out::Text(field_text(f.map_or([f64::NAN; 3], |f| [f.hours, f.minutes, f.seconds]))))
}
pub(super) fn isodate(c: &mut Call) -> Result<Out, String> {
    let call = invocation(c)?;
    let f = local_time::utc_fields(now(c, &call)?);
    Ok(Out::Text(date_text(f.map_or([f64::NAN; 3], |f| [f.year, f.month + 1.0, f.day]))))
}
// dateTimeFormat(args[0], t || currentTime) with t the second argument in seconds; a zero time reads Date.now().
fn date_time_format(c: &mut Call) -> Result<Out, String> {
    let (data, call) = (data(c)?, invocation(c)?);
    let mut seconds = 0.0;
    if c.args.get(1).is_some_and(|a| !a.is_empty()) {
        seconds = c.number(1)? / 1000.0;
        if seconds.is_nan() {
            seconds = 0.0;
        }
    }
    let time = if seconds != 0.0 { Some(seconds) } else { call.current_time };
    let clock = &host(c)?.clock;
    let instant = time_clip(match time {
        Some(t) if t != 0.0 => t,
        _ => clock.now_ms(),
    });
    let local = local(&data)?;
    let format = c.args.first().map_or(&[][..], Vec::as_slice);
    Ok(local_time::date_time_format(&local.zone, format, instant)?.map_or(Out::Preserve, Out::Text))
}
// The second time registration; the first, which it shadows, is its argument-free form.
pub(super) fn time(c: &mut Call) -> Result<Out, String> {
    if !c.args.is_empty() {
        return date_time_format(c);
    }
    let (data, call) = (data(c)?, invocation(c)?);
    let f = local_time::local_fields(&local(&data)?.zone, now(c, &call)?)?;
    Ok(Out::Text(field_text(f.map_or([f64::NAN; 3], |f| [f.hours, f.minutes, f.seconds]))))
}
pub(super) fn date(c: &mut Call) -> Result<Out, String> {
    if !c.args.is_empty() {
        return date_time_format(c);
    }
    let (data, call) = (data(c)?, invocation(c)?);
    let f = local_time::local_fields(&local(&data)?.zone, now(c, &call)?)?;
    Ok(Out::Text(date_text(f.map_or([f64::NAN; 3], |f| [f.year, f.month + 1.0, f.day]))))
}
pub(super) fn messageidleduration(c: &mut Call) -> Result<Out, String> {
    if c.env.options.tokenize_accurate {
        return text(&units("00:00:00"));
    }
    if c.env.context.index == -1 {
        return text(&units("[Cannot get time]"));
    }
    let (data, call) = (data(c)?, invocation(c)?);
    let (character, access) = (data.character.as_ref(), call.access.as_deref());
    let before = history_chat_id(c, &call, character).saturating_add(1);
    let Some(found) = previous_rows(c, access, character, "user", before, 2)? else { return Ok(Out::Preserve) };
    let Some(message) = found.first() else { return text(&units("[No user message found]")) };
    let Some(previous) = found.get(1) else { return text(&units("[No previous user message found]")) };
    match (message_time(message), message_time(previous)) {
        (MessageTime::Missing, _) => text(&units(OLDER)),
        (_, MessageTime::Missing) => text(&units("[Cannot get time, previous message was sent in older version]")),
        (MessageTime::At(a), MessageTime::At(b)) => Ok(Out::Text(local_time::duration(a - b))),
        _ => Err("time-value".into()),
    }
}
pub(super) fn idleduration(c: &mut Call) -> Result<Out, String> {
    if c.env.options.tokenize_accurate {
        return text(&units("00:00:00"));
    }
    let (data, call) = (data(c)?, invocation(c)?);
    let (character, access) = (data.character.as_ref(), call.access.as_deref());
    let Some(length) = loaded_count(access, character) else { return Ok(Out::Preserve) };
    if length == 0 {
        return text(&units("00:00:00"));
    }
    let Some(last) = history_row(access, character, length as f64 - 1.0)? else { return Ok(Out::Preserve) };
    match message_time(last) {
        MessageTime::Missing => text(&units(OLDER)),
        MessageTime::Other => Err("time-value".into()),
        MessageTime::At(time) => Ok(Out::Text(local_time::duration(now(c, &call)? - time))),
    }
}

// --- lorebooks, modules, assets and metadata ---------------------------------

// makeArray(values), bounded by the output limit.
fn json_array(c: &mut Call, values: Vec<Json>) -> Result<Out, String> {
    let out = array_ops::make_array(values);
    if out.len() > c.env.budget.max_output {
        return Err("output-limit".into());
    }
    Ok(Out::Text(out))
}
// f[i] of an asset entry; Err is the TypeError of indexing null.
fn element(entry: &Json, i: usize) -> Result<Option<Json>, ()> {
    Ok(match entry {
        Json::Null => return Err(()),
        Json::Arr(values) => values.get(i).cloned(),
        Json::Str(text) => text.get(i).map(|&unit| Json::Str(vec![unit])),
        Json::Obj(fields) => {
            let key = units(&i.to_string());
            fields.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
        }
        Json::Num(_) | Json::Bool(_) => None,
    })
}
// makeArray(entries.map(f => f[0])), undefined names written as null.
fn names(c: &mut Call, entries: &[Json]) -> Result<Out, String> {
    let mut values = Vec::with_capacity(entries.len());
    for entry in entries {
        c.env.budget.tick()?;
        let Ok(name) = element(entry, 0) else { return Ok(Out::Preserve) };
        values.push(name.unwrap_or(Json::Null));
    }
    json_array(c, values)
}
// SameValueZero over JSON values; distinct arrays and objects never match.
fn same_value_zero(a: &Option<Json>, b: &Option<Json>) -> bool {
    match (a, b) {
        (None, None) | (Some(Json::Null), Some(Json::Null)) => true,
        (Some(Json::Bool(x)), Some(Json::Bool(y))) => x == y,
        (Some(Json::Num(x)), Some(Json::Num(y))) => x == y || (x.is_nan() && y.is_nan()),
        (Some(Json::Str(x)), Some(Json::Str(y))) => x == y,
        _ => false,
    }
}
// `module.namespace === args[0]`.
fn namespace_is(module: &Module, arg: Option<&Units>) -> bool {
    match (&module.namespace, arg) {
        (None, None) => true,
        (Some(Json::Str(namespace)), Some(arg)) => namespace == arg,
        _ => false,
    }
}
// The character's lore, then the chat's, then the modules', each JSON.stringify'd; no selected character throws.
pub(super) fn lorebook(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    let Some(character) = &data.character else { return Ok(Out::Preserve) };
    let mut values = Vec::new();
    for entry in character.lore.iter().chain(&data.module_lorebooks) {
        c.env.budget.tick()?;
        values.push(entry.as_ref().map_or(Json::Null, |entry| Json::Str(json_string(entry))));
    }
    json_array(c, values)
}
pub(super) fn moduleenabled(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    let found = data.modules.iter().any(|m| namespace_is(m, c.args.first()));
    text(&units(if found { "1" } else { "0" }))
}
pub(super) fn moduleassetlist(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    let Some(module) = data.modules.iter().find(|m| namespace_is(m, c.args.first())) else { return text(&[]) };
    match &module.assets {
        Some(assets) => names(c, assets),
        None => Ok(Out::Preserve),
    }
}
// makeArray of the entry names: '' without a selected character, and makeArray(undefined) throws.
fn character_names(c: &mut Call, list: fn(&Character) -> Option<&Vec<Json>>) -> Result<Out, String> {
    let data = data(c)?;
    let Some(character) = &data.character else { return text(&[]) };
    match list(character) {
        Some(entries) => names(c, entries),
        None => Ok(Out::Preserve),
    }
}
pub(super) fn emotionlist(c: &mut Call) -> Result<Out, String> {
    character_names(c, |c| c.emotion_images.as_ref())
}
pub(super) fn assetlist(c: &mut Call) -> Result<Out, String> {
    character_names(c, |c| c.additional_assets.as_ref())
}
// The additional asset names whose path prebuiltAssetExclude does not list, or [] without prebuiltAssetCommand.
pub(super) fn chardisplayasset(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    let Some(character) = &data.character else { return Ok(Out::Preserve) };
    if !character.prebuilt_asset_command {
        return text(&units("[]"));
    }
    let excludes = character.prebuilt_asset_exclude.as_deref().unwrap_or_default();
    let mut shown = Vec::new();
    for entry in character.additional_assets.as_deref().unwrap_or_default() {
        c.env.budget.tick()?;
        let Ok(path) = element(entry, 1) else { return Ok(Out::Preserve) };
        if !excludes.iter().any(|e| same_value_zero(e, &path)) {
            shown.push(entry.clone());
        }
    }
    names(c, &shown)
}
fn member<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    let Json::Obj(fields) = value else { return None };
    let key = units(key);
    fields.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
}
fn model_info(data: &Data) -> Result<&Json, String> {
    data.model_info.as_ref().ok_or_else(|| "model-info".into())
}
// The first of `keys` that is neither null nor undefined, as the callback returns it: a falsy value keeps the
// macro, and a value other than a string is not text the parser can append.
fn model_name(data: &Data, keys: &[&str]) -> Result<Out, String> {
    let info = model_info(data)?;
    match keys.iter().find_map(|k| member(info, k).filter(|v| !matches!(v, Json::Null))) {
        None | Some(Json::Bool(false)) => Ok(Out::Preserve),
        Some(Json::Num(n)) if *n == 0.0 || n.is_nan() => Ok(Out::Preserve),
        Some(Json::Str(name)) => text(name),
        Some(_) => Err("model-info".into()),
    }
}
// modelInfo[key].toString(), which throws for null and undefined.
fn model_field(data: &Data, key: &str) -> Result<Out, String> {
    match member(model_info(data)?, key) {
        None | Some(Json::Null) => Ok(Out::Preserve),
        Some(value @ (Json::Str(_) | Json::Num(_) | Json::Bool(_))) => Ok(Out::Text(to_string(value))),
        Some(_) => Err("model-info".into()),
    }
}
// Anthropic and AnthropicLegacy models report the hasPrefill flag; other models a 'claude' id.
pub(super) fn prefillsupported(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    let info = model_info(&data)?;
    let bit = |v: bool| text(&units(if v { "1" } else { "0" }));
    if matches!(member(info, "format"), Some(Json::Num(n)) if *n == 2.0 || *n == 3.0) {
        return match member(info, "flags") {
            None | Some(Json::Null) => Ok(Out::Preserve),
            Some(Json::Arr(flags)) => bit(flags.iter().any(|f| matches!(f, Json::Num(n) if *n == 4.0))),
            Some(_) => Err("model-info".into()),
        };
    }
    match &data.model {
        Some(model) => bit(model.starts_with(&units("claude"))),
        None => Ok(Out::Preserve),
    }
}
pub(super) fn metadata(c: &mut Call) -> Result<Out, String> {
    let data = data(c)?;
    let Some(key) = c.args.first() else { return Ok(Out::Preserve) };
    if c.env.options.tailored_case {
        return Err("case-locale".into());
    }
    let platform = &data.platform;
    let lowered = String::from_utf16_lossy(&super::text::lower(key));
    match lowered.as_str() {
        "mobile" | "node" => text(&units("0")),
        "local" => text(&units(if platform.tauri { "1" } else { "0" })),
        "version" => text(&platform.app_version),
        "majorversion" | "majorver" | "major" => text(&split(&platform.app_version, &[46])[0]),
        "language" | "locale" | "lang" => Ok(data.language.clone().map_or(Out::Preserve, Out::Text)),
        "browserlanguage" | "browserlocale" | "browserlang" => text(&platform.browser_language),
        "modelshortname" => model_name(&data, &["shortName", "name", "id"]),
        "modelname" => model_name(&data, &["name", "id"]),
        "modelinternalid" => model_name(&data, &["internalID", "id"]),
        "modelformat" => model_field(&data, "format"),
        "modelprovider" => model_field(&data, "provider"),
        "modeltokenizer" => model_field(&data, "tokenizer"),
        "imateapot" => text(&units("\u{1fad6}")),
        "risutype" => text(&units(if platform.tauri { "local" } else { "web" })),
        "maxcontext" => Ok(Out::Text(number_string(data.max_context))),
        _ => Ok(Out::Text([&units("Error: ")[..], key, &units(" is not a valid metadata key.")].concat())),
    }
}

// --- hash picks, display and declare -----------------------------------------

// pickHashRand(historyCount + offset, chaId + (chat.id ?? '')).
fn hash_rand(c: &mut Call, data: &Data, call: &Invocation, offset: f64) -> Result<Option<f64>, String> {
    let Some(character) = &data.character else { return Ok(None) };
    let count = history_count(call.access.as_deref(), Some(character)).unwrap_or(0.0);
    pick_hash_rand(count + offset, &character.hash_word, c.env.budget).map(Some)
}
pub(super) fn pick(c: &mut Call) -> Result<Out, String> {
    let (data, call) = (data(c)?, invocation(c)?);
    match hash_rand(c, &data, &call, 0.0)? {
        Some(rand) => families::pick_from(c, rand),
        None => Ok(Out::Preserve),
    }
}
pub(super) fn rollp(c: &mut Call) -> Result<Out, String> {
    let (count, sides) = match families::dice_notation(c)? {
        Ok(notation) => notation,
        Err(out) => return Ok(out),
    };
    if count > 1_000_000.0 {
        return Err("rng-loop-limit".into());
    }
    let (data, call) = (data(c)?, invocation(c)?);
    let (mut total, mut i) = (0.0, 0.0);
    while i < count {
        c.env.budget.tick()?;
        let Some(rand) = hash_rand(c, &data, &call, i * 15.0)? else { return Ok(Out::Preserve) };
        total += (rand * sides).floor() + 1.0;
        i += 1.0;
    }
    Ok(Out::Text(number_string(total)))
}
pub(super) fn screenwidth(c: &mut Call) -> Result<Out, String> {
    text(&units(&data(c)?.viewport.0.to_string()))
}
pub(super) fn screenheight(c: &mut Call) -> Result<Out, String> {
    text(&units(&data(c)?.viewport.1.to_string()))
}
// matcherArg.var is null, so the write throws.
pub(super) fn declare(_: &mut Call) -> Result<Out, String> {
    Ok(Out::Preserve)
}
