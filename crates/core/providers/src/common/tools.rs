//! Pieces of tool handling the families share: the `<tool_call>` marker scan and what `decodeToolCall` returns.

use super::js;
use risunest_ir::{Text, Value};

/// One entry of a tool response, as `callTool` and `decodeToolCall` give it.
#[derive(Debug)]
pub enum ToolResponsePart {
    Text(Text),
    Image { mime_type: Text, data: Text },
    /// A part of another type; the request carries `Unsupported tool response type: <type>`.
    Other(Text),
}

/// `decodeToolCall(...)` for a `<tool_call>` marker inside message content.
#[derive(Debug)]
pub struct DecodedToolCall {
    pub id: Text,
    pub name: Text,
    pub arg: Value,
    pub response: Vec<ToolResponsePart>,
}


fn is_line_terminator(unit: u16) -> bool { matches!(unit, 10 | 13 | 0x2028 | 0x2029) }


/// The matches of `/<tool_call>(.*?)<\/tool_call>/g` as `(start, inner_start, inner_end, end)` in UTF-16 units.
pub(crate) fn tool_call_matches(all: &[u16]) -> Vec<(usize, usize, usize, usize)> {
    let open = js::utf16("<tool_call>");
    let close = js::utf16("</tool_call>");
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(start) = js::find_units(all, &open, at) {
        let inner = start + open.len();
        let mut cursor = inner;
        let mut matched = None;
        while cursor <= all.len() {
            if all[cursor..].starts_with(&close) { matched = Some(cursor); break; }
            if cursor == all.len() || is_line_terminator(all[cursor]) { break; }
            cursor += 1;
        }
        match matched {
            Some(end) => { found.push((start, inner, end, end + close.len())); at = end + close.len(); }
            None => at = start + 1,
        }
    }
    found
}

