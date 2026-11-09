//! Personas.

use crate::conversation::Conversation;
use crate::root::{Fields, Root};
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Text, Value};

/// `personaMirrorMap`: each root field the selected persona sets, with the persona field it comes from.
pub const MIRRORS: [(&str, &str); 4] = [("username", "name"), ("userIcon", "icon"), ("personaPrompt", "personaPrompt"), ("userNote", "note")];

/// A stored persona entry of `root.personas`.
#[derive(Debug, Clone, Copy)]
pub struct Persona<'a>(Fields<'a>);

impl<'a> Persona<'a> {
    pub fn new(value: &'a Value) -> Result<Self, StageError> { Ok(Self(Fields::new(value, "persona")?)) }
    pub fn fields(&self) -> Fields<'a> { self.0 }
    pub fn id(&self, budget: &Budget) -> Result<Option<&'a Text>, StageError> { self.0.text("id", budget) }
    /// The raw property, `None` when absent; `getUserName` and `getPersonaPrompt` read bound personas this way.
    pub fn get(&self, field: &str, budget: &Budget) -> Result<Option<&'a Value>, StageError> { self.0.get(field, budget) }
}

/// The persona whose `id` equals `id`, as `personas.find(v => v.id === id)` finds it.
pub fn by_id<'a>(root: Root<'a>, id: &Text, budget: &Budget) -> Result<Option<Persona<'a>>, StageError> {
    for value in root.personas(budget)? {
        budget.work(1)?;
        let Value::Object(_) = value else { continue };
        let persona = Persona::new(value)?;
        if persona.id(budget)?.is_some_and(|value| value.equals(id)) { return Ok(Some(persona)); }
    }
    Ok(None)
}

/// `checkPersonaBinded()`: the persona whose `id` is strictly equal to a truthy `chat.bindedPersona`. A lookup TS would
/// throw on, such as a missing persona list or a nullish entry before the match, binds none.
pub fn bound<'a>(root: Root<'a>, conversation: Conversation<'a>, budget: &Budget) -> Result<Option<Persona<'a>>, StageError> {
    crate::modules::bound_persona(root.value(), conversation.value(), budget)?.map(Persona::new).transpose()
}

/// The persona a load selects. `root.selectedPersona` names it by ID, and the load reads an ID no persona has, `""`
/// included, as index 0 (`projectSelectionIndexes`); an absent selection is index 0 (`selectedPersona ??= 0`). A number
/// is an index from a root that predates IDs, and a negative, fractional or other value selects none.
pub fn selected<'a>(root: Root<'a>, budget: &Budget) -> Result<Option<Persona<'a>>, StageError> {
    Ok(match selection(root, budget)? {
        Selection::Found(persona) => Some(persona),
        Selection::Index(index) => match root.personas(budget)?.get(index) {
            Some(value @ Value::Object(_)) => Some(Persona::new(value)?),
            _ => None,
        },
        Selection::None => None,
    })
}

/// Whether the load selects the first persona, the one `personas ??= [...]` creates for a root without a list.
pub fn selects_first(root: Root<'_>, budget: &Budget) -> Result<bool, StageError> {
    Ok(matches!(selection(root, budget)?, Selection::Index(0)))
}

enum Selection<'a> { Found(Persona<'a>), Index(usize), None }

fn selection<'a>(root: Root<'a>, budget: &Budget) -> Result<Selection<'a>, StageError> {
    let index = match root.selected_persona(budget)? {
        None => 0.0,
        Some(Value::Text(id)) => match by_id(root, id, budget)? {
            Some(persona) => return Ok(Selection::Found(persona)),
            None => 0.0,
        },
        Some(Value::Number(index)) => *index,
        Some(_) => return Ok(Selection::None),
    };
    if index < 0.0 || index.fract() != 0.0 { return Ok(Selection::None); }
    Ok(Selection::Index(index as usize))
}
