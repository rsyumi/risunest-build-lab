//! Example messages.

use crate::machine::{invalid, js, unsupported, EngineCase};
use risunest_ir::chat::Role;
use risunest_ir::stage::StageError;
use risunest_ir::Budget;
use risunest_records::character::Character;

/// One `exampleMessage` entry before its CBS pass.
#[derive(Debug, PartialEq)]
pub(crate) struct Example {
    pub(crate) role: Role,
    pub(crate) content: js::S,
    pub(crate) name: Option<&'static str>,
    pub(crate) memo: Option<&'static str>,
}

/// Whether `toLocaleLowerCase` under a tailored locale maps `units` differently from the root mapping: `I` and `İ`
/// (`tr`, `az`), `Ì`, `Í` and `Ĩ` (`lt`), and `J` or `Į` followed by a unit from U+0300 on, which may be a combining
/// mark above (`lt`), as the lorebook stage tells it.
fn tailored_difference(units: &[u16]) -> bool {
    units.iter().enumerate().any(|(at, unit)| match unit {
        0x49 | 0x130 | 0xcc | 0xcd | 0x128 => true,
        0x4a | 0x12e => units.get(at + 1).is_some_and(|next| *next >= 0x300),
        _ => false,
    })
}

/// Declines a line whose role checks the engine's `toLocaleLowerCase` could decide differently from Rust's. The checks
/// read at most `reach` units of the lowered line, and every code point lowers to at least one, so they read only the
/// lowering of its first `reach` code points; that of a capital sigma among them also depends on the text after it.
/// The tailoring check takes one unit more, the mark a `J` or `Į` may be followed by.
fn admit_lowering(line: &[u16], reach: usize, case: EngineCase) -> Result<(), StageError> {
    let mut head = 0;
    for _ in 0..reach {
        if head >= line.len() { break; }
        let pair = (0xd800..=0xdbff).contains(&line[head]) && line.get(head + 1).is_some_and(|next| (0xdc00..=0xdfff).contains(next));
        head += if pair { 2 } else { 1 };
    }
    if case.tailored && tailored_difference(&line[..(head + 1).min(line.len())]) { return Err(unsupported("example-case-locale")); }
    let read = if line[..head].contains(&0x3a3) { line } else { &line[..head] };
    if case.tables.differs(read) { return Err(unsupported("example-case-table")); }
    Ok(())
}

/// `exampleMessage(char, userName)` (`src/ts/process/exampleMessages.ts`) up to the final `risuChatParser` pass, which
/// the caller runs in this order. `case` is the engine's `toLocaleLowerCase`, which reads each line's role.
pub(crate) fn read(character: Character<'_>, case: EngineCase, budget: &Budget) -> Result<Vec<Example>, StageError> {
    let source = js::units(character.required_text("exampleMessage", budget)?, budget)?;
    if source.is_empty() { return Ok(Vec::new()); }
    // `lowered.startsWith(`${char.name}:`)` compares the lowered line with the stored name.
    let name = js::concat(&[&js::units(character.name(budget)?, budget)?, &js::s(":")]);
    let assistant = [js::s("{{char}}:"), js::s("<bot>:"), name];
    let user = [js::s("{{user}}:"), js::s("<user>:")];
    // One unit past `<start>` tells the whole line from it.
    let reach = assistant.iter().chain(&user).map(Vec::len).max().unwrap_or(0).max("<start>".len() + 1);
    let mut output = Vec::new();
    let mut current: Option<Example> = None;
    for line in source.split(|unit| *unit == 0x0a) {
        budget.work(line.len() + 1)?;
        let trimmed = js::trim(line, budget)?;
        admit_lowering(&trimmed, reach, case)?;
        let lowered = js::lower(&trimmed, budget)?;
        let role = if js::equals(&lowered, "<start>") {
            output.extend(current.take());
            output.push(Example { role: Role::System, content: js::s("[Start a new chat]"), name: None, memo: Some("NewChatExample") });
            continue;
        } else if assistant.iter().any(|prefix| lowered.starts_with(prefix)) {
            (Role::Assistant, "example_assistant")
        } else if user.iter().any(|prefix| lowered.starts_with(prefix)) {
            (Role::User, "example_user")
        } else {
            if let Some(current) = &mut current {
                current.content.push(0x0a);
                current.content.extend_from_slice(&trimmed);
            }
            continue;
        };
        output.extend(current.take());
        // `trimed.split(':', 2)[1].trimStart()`: the text between the first and second colon.
        let mut parts = trimmed.split(|unit| *unit == 0x3a);
        parts.next();
        let content = parts.next().ok_or_else(|| invalid("character.exampleMessage"))?;
        current = Some(Example { role: role.0, content: js::trim_start(content, budget)?, name: Some(role.1), memo: None });
    }
    output.extend(current);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::CaseTables;
    use risunest_ir::{parse_json, Value};

    const UNICODE_16: EngineCase = EngineCase { tables: CaseTables::Unicode16, tailored: false };

    fn read_with(name: &str, source: &str, case: EngineCase) -> Result<Vec<Example>, StageError> {
        let budget = crate::machine::tests::budget();
        let json = format!(r#"{{"name":{},"exampleMessage":{}}}"#, quote(name), quote(source));
        let value: Value = parse_json(&json, &budget).unwrap();
        read(Character::new(&value).unwrap(), case, &budget)
    }

    fn examples(name: &str, source: &str) -> Vec<(Role, String, Option<&'static str>, Option<&'static str>)> {
        read_with(name, source, UNICODE_16).unwrap().into_iter()
            .map(|example| (example.role, String::from_utf16(&example.content).unwrap(), example.name, example.memo)).collect()
    }
    fn quote(text: &str) -> String { format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")) }

    #[test]
    fn example_lines_split_on_speakers_and_starts() {
        let read = examples("Ann", "<START>\n{{char}}: Hi: there\n  more  \n<USER>:Yo\nAnn: lower name misses\nann: kept\n<start>");
        assert_eq!(read, vec![
            (Role::System, "[Start a new chat]".into(), None, Some("NewChatExample")),
            (Role::Assistant, "Hi\nmore".into(), Some("example_assistant"), None),
            (Role::User, "Yo\nAnn: lower name misses\nann: kept".into(), Some("example_user"), None),
            (Role::System, "[Start a new chat]".into(), None, Some("NewChatExample")),
        ]);
        assert_eq!(examples("ann", "Ann: kept")[0].1, "kept");
        assert!(examples("Ann", "").is_empty());
    }

    /// A line declines only where the engine's lowering could change what its role checks read: its first code points,
    /// as many as the longest prefix has units, and the rest of the line after a capital sigma among them.
    #[test]
    fn example_lines_decline_only_where_the_engine_could_read_their_role_differently() {
        let declined = |source: &str, tables, tailored| match read_with("Ann", source, EngineCase { tables, tailored }) {
            Ok(_) => None,
            Err(StageError::Unsupported(name)) => Some(name.into_owned()),
            Err(error) => panic!("{error:?}"),
        };
        let table = Some("example-case-table".to_owned());
        // Text past the prefixes reads alike in every engine.
        assert_eq!(declined("{{char}}: 안녕하세요 \u{a7d3}", CaseTables::Foreign, false), None);
        assert_eq!(declined("안녕: hi", CaseTables::Foreign, false), table);
        assert_eq!(declined("안녕: hi", CaseTables::Unicode16, false), None);
        // Unicode 17 gives U+A7D2 its small letter, which a Unicode 16 engine lacks.
        assert_eq!(declined("\u{a7d2}: hi", CaseTables::Unicode16, false), table);
        assert_eq!(declined("\u{a7d2}: hi", CaseTables::Native, false), None);
        assert_eq!(declined("ΑΣ: xxxxxxxxx \u{a7d3}", CaseTables::Unicode16, false), table);
        assert_eq!(declined("AB: xxxxxxxxx \u{a7d3}", CaseTables::Unicode16, false), None);
        // A tailored locale lowers `I` differently.
        assert_eq!(declined("Ivy: hi", CaseTables::Native, true), Some("example-case-locale".to_owned()));
        assert_eq!(declined("{{char}}: I think", CaseTables::Native, true), None);
    }
}
