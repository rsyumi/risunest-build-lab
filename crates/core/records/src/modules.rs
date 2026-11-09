//! Module resolution and de-duplication by ID.
//!
//! Ports `selectConversationModules` (`src/ts/process/moduleSelection.ts`), `checkPersonaBinded`
//! (`src/ts/util.ts:169`) and `getModuleLorebooks` (`src/ts/process/modules.ts:516`) over stored records. Results
//! borrow from the records; a JS object is compared by identity, which is pointer identity here.

use risunest_ir::budget::{Budget, Error, OwnedVec};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;

/// What `selectConversationModules` reads for one conversation.
#[derive(Debug, Clone, Copy)]
pub struct ModuleSources<'a> {
    /// The stored root; `modules`, `enabledModules` and `moduleIntergration` are read from it.
    pub root: &'a Value,
    /// `chat.modules`.
    pub chat_modules: Option<&'a Value>,
    /// `character.modules`.
    pub character_modules: Option<&'a Value>,
    /// The persona `checkPersonaBinded` returns.
    pub bound_persona: Option<&'a Value>,
}

impl<'a> ModuleSources<'a> {
    /// The sources `getModules()` uses for `conversation` of `character`.
    pub fn for_conversation(root: &'a Value, character: &'a Value, conversation: &'a Value, budget: &Budget) -> Result<Self, Error> {
        Ok(Self {
            root,
            chat_modules: present(conversation.get(b"modules", budget)?),
            character_modules: present(character.get(b"modules", budget)?),
            bound_persona: bound_persona(root, conversation, budget)?,
        })
    }
}

/// One evaluation's canonical selected records, copied after identity-based selection.
pub struct SelectedModules { records: OwnedVec<Value> }

impl SelectedModules {
    /// `integration` is the effective setting; absence never falls back to the stored root mirror.
    pub fn capture(sources: &ModuleSources<'_>, integration: Option<&Value>, budget: &Budget) -> Result<Self, StageError> {
        let selected = select_conversation_modules_with_integration(sources, integration, budget)?;
        let mut records = OwnedVec::new(budget)?;
        for record in selected { records.push(crate::root::copy(record, budget)?)?; }
        Ok(Self { records })
    }

    pub fn records(&self) -> &[Value] { self.records.as_slice() }
}

fn present(value: Option<&Value>) -> Option<&Value> { value.filter(|value| !matches!(value, Value::Undefined)) }

/// `checkPersonaBinded()`: the persona whose `id` is `chat.bindedPersona`. A lookup TS would throw on returns
/// `None`, as its `catch` does.
pub fn bound_persona<'a>(root: &'a Value, conversation: &'a Value, budget: &Budget) -> Result<Option<&'a Value>, Error> {
    let Some(bound) = conversation.get(b"bindedPersona", budget)? else { return Ok(None) };
    if !bound.truthy() { return Ok(None); }
    let Some(personas) = root.get(b"personas", budget)?.and_then(Value::array) else { return Ok(None) };
    for persona in personas {
        budget.work(1)?;
        if matches!(persona, Value::Undefined | Value::Null) { return Ok(None); }
        if strict_equals(property(persona, b"id", budget)?, bound) { return Ok(Some(persona)); }
    }
    Ok(None)
}

/// `selectConversationModules`: the enabled, chat, character and `moduleIntergration` modules in stored order,
/// then the bound persona's `embeddedModule`, without a repeated `id`.
pub fn select_conversation_modules<'a>(sources: &ModuleSources<'a>, budget: &Budget) -> Result<Vec<&'a Value>, Error> {
    select_conversation_modules_with_integration(sources, sources.root.get(b"moduleIntergration", budget)?, budget)
}

/// Canonical selection with an explicit effective integration setting, including absent or nullish values.
pub fn select_conversation_modules_with_integration<'a>(sources: &ModuleSources<'a>, integration: Option<&'a Value>, budget: &Budget) -> Result<Vec<&'a Value>, Error> {
    let root = sources.root;
    let mut ids: Vec<Key<'a>> = Vec::new();
    match root.get(b"enabledModules", budget)? {
        None | Some(Value::Undefined | Value::Null) => {}
        Some(Value::Array(values)) => ids.extend(values.as_slice().iter().map(Key::Value)),
        Some(_) => return Err(Error::Type { operation: "enabledModules" }),
    }
    // `concat` spreads an array and appends any other value as one element.
    let concat = |value: &'a Value, ids: &mut Vec<Key<'a>>| match value {
        Value::Array(values) => ids.extend(values.as_slice().iter().map(Key::Value)),
        other => ids.push(Key::Value(other)),
    };
    if let Some(chat) = sources.chat_modules.filter(|value| !matches!(value, Value::Null)) { concat(chat, &mut ids); }
    if let Some(character) = sources.character_modules.filter(|value| value.truthy()) { concat(character, &mut ids); }
    if let Some(integration) = integration.filter(|value| value.truthy()) {
        let Value::Text(integration) = integration else { return Err(Error::Type { operation: "moduleIntergration" }) };
        for part in split_units(integration, b',' as u16, budget)? {
            ids.push(Key::Text(part.trim(budget)?));
        }
    }
    budget.work(ids.len())?;

    let mut selected = Vec::new();
    if let Some(modules) = root.get(b"modules", budget)? {
        match modules {
            Value::Undefined | Value::Null => {}
            Value::Array(modules) => {
                for module in modules.as_slice() {
                    if matches!(module, Value::Undefined | Value::Null) { return Err(Error::Type { operation: "modules" }); }
                    let id = property(module, b"id", budget)?;
                    let mut matched = contains(&ids, id, budget)?;
                    if !matched {
                        let namespace = property(module, b"namespace", budget)?;
                        matched = namespace.truthy() && contains(&ids, namespace, budget)?;
                    }
                    if matched { selected.push(module); }
                }
            }
            _ => return Err(Error::Type { operation: "modules" }),
        }
    }
    let mut modules = deduplicate(selected, budget)?;
    if let Some(persona) = sources.bound_persona {
        let embedded = property(persona, b"embeddedModule", budget)?;
        if embedded.truthy() { modules.push(embedded); }
    }
    deduplicate(modules, budget)
}

/// `getModuleLorebooks`: every module's `lorebook` entries in module order.
pub fn module_lorebooks<'a>(modules: &[&'a Value], budget: &Budget) -> Result<Vec<&'a Value>, Error> {
    let mut entries = Vec::new();
    for module in modules {
        if !module.truthy() { continue; }
        let lorebook = property(module, b"lorebook", budget)?;
        if !lorebook.truthy() { continue; }
        match lorebook {
            Value::Array(values) => {
                budget.work(values.as_slice().len())?;
                entries.extend(values.as_slice().iter());
            }
            other => entries.push(other),
        }
    }
    Ok(entries)
}

static UNDEFINED: Value = Value::Undefined;

/// A property read on a non-nullish JS value; a missing property and a primitive receiver read `undefined`.
fn property<'a>(value: &'a Value, key: &[u8], budget: &Budget) -> Result<&'a Value, Error> {
    Ok(value.get(key, budget)?.unwrap_or(&UNDEFINED))
}

enum Key<'a> { Value(&'a Value), Text(Text) }

fn contains(ids: &[Key<'_>], value: &Value, budget: &Budget) -> Result<bool, Error> {
    for id in ids {
        budget.work(1)?;
        let equal = match id {
            Key::Value(id) => same_value_zero(id, value),
            Key::Text(id) => matches!(value, Value::Text(value) if value == id),
        };
        if equal { return Ok(true); }
    }
    Ok(false)
}

fn deduplicate<'a>(modules: Vec<&'a Value>, budget: &Budget) -> Result<Vec<&'a Value>, Error> {
    let mut ids: Vec<&Value> = Vec::new();
    let mut unique = Vec::new();
    for module in modules {
        let id = property(module, b"id", budget)?;
        budget.work(ids.len().saturating_add(1))?;
        if ids.iter().any(|seen| same_value_zero(seen, id)) { continue; }
        ids.push(id);
        unique.push(module);
    }
    Ok(unique)
}

/// `SameValueZero`, as `Set.prototype.has` and `Array.prototype.includes` compare.
pub fn same_value_zero(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left == right || (left.is_nan() && right.is_nan()),
        _ => strict_equals(left, right),
    }
}

/// `===`.
pub fn strict_equals(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::Number(left), Value::Number(right)) => left == right,
        (Value::Text(left), Value::Text(right)) => left == right,
        (Value::Array(_), Value::Array(_)) | (Value::Object(_), Value::Object(_)) => std::ptr::eq(left, right),
        _ => false,
    }
}

fn split_units(text: &Text, separator: u16, budget: &Budget) -> Result<Vec<Text>, Error> {
    let units: Vec<u16> = text.units().collect();
    budget.work(units.len())?;
    units.split(|unit| *unit == separator).map(|part| Text::from_units(part, budget)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use risunest_ir::json::parse_json;
    use std::sync::{atomic::AtomicBool, Arc};

    fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 26, max_json_bytes: 1 << 24, max_text_units: 1 << 22, max_values: 1 << 20,
            max_depth: 64, max_work: 1 << 30 }, Arc::new(AtomicBool::new(false))).unwrap()
    }
    fn names(modules: &[&Value], budget: &Budget) -> Vec<String> {
        modules.iter().map(|module| match module.get(b"name", budget).unwrap() {
            Some(Value::Text(name)) => name.as_unicode().unwrap().to_owned(),
            _ => "?".to_owned(),
        }).collect()
    }
    fn select(json: &str) -> Vec<String> {
        let budget = budget();
        let database = parse_json(json, &budget).unwrap();
        let character = database.get(b"character", &budget).unwrap().unwrap();
        let chat = database.get(b"chat", &budget).unwrap().unwrap();
        let sources = ModuleSources::for_conversation(&database, character, chat, &budget).unwrap();
        names(&select_conversation_modules(&sources, &budget).unwrap(), &budget)
    }

    #[test]
    fn selects_enabled_chat_character_integration_and_persona_modules_once_each() {
        let modules = r#"[{"id":"e","name":"enabled"},{"id":"c","name":"chat"},{"id":"h","name":"character"},
            {"id":"i","name":"integration"},{"id":"x","namespace":"ns","name":"namespaced"},{"id":"e","name":"repeat"},
            {"id":"unused","name":"unused"},{"name":"no-id-a","namespace":"ns"},{"name":"no-id-b","namespace":"ns"}]"#;
        let json = format!(r#"{{"modules":{modules},"enabledModules":["e"],"moduleIntergration":" i , ns ,",
            "personas":[{{"id":"p","embeddedModule":{{"id":"persona","name":"persona"}}}}],
            "character":{{"modules":["h"]}},"chat":{{"modules":["c"],"bindedPersona":"p"}}}}"#);
        assert_eq!(select(&json), ["enabled", "chat", "character", "integration", "namespaced", "no-id-a", "persona"]);
    }

    #[test]
    fn the_bound_persona_module_is_dropped_when_its_id_is_already_selected() {
        let json = r#"{"modules":[{"id":"m","name":"stored"}],"enabledModules":["m"],
            "personas":[{"id":"p","embeddedModule":{"id":"m","name":"embedded"}}],"character":{},"chat":{"bindedPersona":"p"}}"#;
        assert_eq!(select(json), ["stored"]);
        let unbound = r#"{"modules":[],"personas":[{"id":"p","embeddedModule":{"id":"m","name":"embedded"}}],
            "character":{},"chat":{"bindedPersona":"q"}}"#;
        assert_eq!(select(unbound), Vec::<String>::new());
    }

    #[test]
    fn non_array_module_lists_concat_as_one_id() {
        let json = r#"{"modules":[{"id":"solo","name":"solo"},{"id":"s","name":"s"}],"character":{"modules":"solo"},"chat":{"modules":"s"}}"#;
        assert_eq!(select(json), ["solo", "s"]);
    }

    #[test]
    fn module_lorebooks_concatenate_in_module_order() {
        let budget = budget();
        let database = parse_json(r#"[{"lorebook":[{"key":"a"},{"key":"b"}]},{"lorebook":null},{},{"lorebook":[{"key":"c"}]}]"#, &budget).unwrap();
        let modules: Vec<&Value> = database.array().unwrap().iter().collect();
        let keys: Vec<String> = module_lorebooks(&modules, &budget).unwrap().iter().map(|entry| match entry.get(b"key", &budget).unwrap() {
            Some(Value::Text(key)) => key.as_unicode().unwrap().to_owned(),
            _ => "?".to_owned(),
        }).collect();
        assert_eq!(keys, ["a", "b", "c"]);
    }
    #[test]
    fn captured_selection_uses_explicit_integration_and_owns_canonical_order() {
        let budget = budget();
        let capture = {
            let database = parse_json(r#"{"modules":[{"id":7,"name":"number","lorebook":[{"key":"first"}]},{"id":"n","namespace":"alias","name":"namespace"},{"id":"stale","name":"stale"}],"enabledModules":["alias"],"moduleIntergration":"stale","character":{"modules":7},"chat":{"bindedPersona":"p"},"personas":[{"id":"p","embeddedModule":{"id":7,"name":"duplicate"}}]}"#, &budget).unwrap();
            let sources = ModuleSources::for_conversation(&database, database.get(b"character", &budget).unwrap().unwrap(),
                database.get(b"chat", &budget).unwrap().unwrap(), &budget).unwrap();
            assert_eq!(names(&select_conversation_modules(&sources, &budget).unwrap(), &budget), ["number", "namespace", "stale"]);
            let capture = SelectedModules::capture(&sources, None, &budget).unwrap();
            let explicit = parse_json(r#""stale""#, &budget).unwrap();
            assert_eq!(names(&select_conversation_modules_with_integration(&sources, Some(&explicit), &budget).unwrap(), &budget), ["number", "namespace", "stale"]);
            for absent in [Value::Undefined, Value::Null] {
                assert_eq!(names(&select_conversation_modules_with_integration(&sources, Some(&absent), &budget).unwrap(), &budget), ["number", "namespace"]);
            }
            capture
        };
        let records: Vec<&Value> = capture.records().iter().collect();
        assert_eq!(names(&records, &budget), ["number", "namespace"]);
        assert_eq!(module_lorebooks(&records, &budget).unwrap().len(), 1);
    }

}
