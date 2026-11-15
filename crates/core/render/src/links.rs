//! `lib/helpers/parse_link_destination.mjs` and `parse_link_title.mjs` of markdown-it 14.3.2.

use crate::error::Result;
use crate::meter::Meter;
use crate::utils::{code, unescape_all, Units};

pub(crate) struct Destination {
    pub(crate) ok: bool,
    pub(crate) pos: usize,
    pub(crate) text: Units,
}

/// `parseLinkDestination(str, start, max)`.
pub(crate) fn parse_link_destination(src: &[u16], start: usize, max: usize, meter: &Meter) -> Result<Destination> {
    let fail = Destination { ok: false, pos: 0, text: Units::new() };
    let mut pos = start;
    if code(src, pos) == Some(0x3c) {
        pos += 1;
        while pos < max {
            match src[pos] {
                0x0a | 0x3c => return Ok(fail),
                0x3e => return Ok(Destination { ok: true, pos: pos + 1, text: unescape_all(&src[start + 1..pos], meter)? }),
                0x5c if pos + 1 < max => {
                    pos += 2;
                    continue;
                }
                _ => {}
            }
            pos += 1;
        }
        return Ok(fail);
    }
    let mut level = 0;
    while pos < max {
        let unit = src[pos];
        if unit == 0x20 || unit < 0x20 || unit == 0x7f { break; }
        if unit == 0x5c && pos + 1 < max {
            if src[pos + 1] == 0x20 {
                pos += 1;
                continue;
            }
            pos += 2;
            continue;
        }
        if unit == 0x28 {
            level += 1;
            if level > 32 { return Ok(fail); }
        }
        if unit == 0x29 {
            if level == 0 { break; }
            level -= 1;
        }
        pos += 1;
    }
    if start == pos || level != 0 { return Ok(fail); }
    Ok(Destination { ok: true, pos, text: unescape_all(&src[start..pos], meter)? })
}

#[derive(Clone)]
pub(crate) struct Title {
    pub(crate) ok: bool,
    pub(crate) can_continue: bool,
    pub(crate) pos: usize,
    pub(crate) text: Units,
    marker: u16,
}

/// `parseLinkTitle(str, start, max, prev_state)`.
pub(crate) fn parse_link_title(src: &[u16], mut start: usize, max: usize, previous: Option<Title>, meter: &Meter) -> Result<Title> {
    let mut pos = start;
    let mut state = Title { ok: false, can_continue: false, pos: 0, text: Units::new(), marker: 0 };
    if let Some(previous) = previous {
        state.text = previous.text;
        state.marker = previous.marker;
    } else {
        if pos >= max { return Ok(state); }
        let mut marker = src[pos];
        if !matches!(marker, 0x22 | 0x27 | 0x28) { return Ok(state); }
        start += 1;
        pos += 1;
        if marker == 0x28 { marker = 0x29; }
        state.marker = marker;
    }
    while pos < max {
        let unit = src[pos];
        if unit == state.marker {
            state.pos = pos + 1;
            state.text.extend(unescape_all(&src[start..pos], meter)?);
            state.ok = true;
            return Ok(state);
        } else if unit == 0x28 && state.marker == 0x29 {
            return Ok(state);
        } else if unit == 0x5c && pos + 1 < max {
            pos += 1;
        }
        pos += 1;
    }
    state.can_continue = true;
    state.text.extend(unescape_all(&src[start.min(pos)..pos], meter)?);
    Ok(state)
}
