//! `parseAdditionalAssets` (`src/ts/parser/parser.svelte.ts`): the asset tags of `assetRegex` (`{{img::name}}` and
//! its siblings) become paths or media markup. The asset index of the character and the file lookups stay with the
//! host: the stage asks for the index lookups its tags make, then for the URLs of the paths it picked, and writes the
//! replacements.

use crate::display::substitute;
use crate::error::{RenderError, Result};
use crate::meter::Meter;
use crate::utils::{ascii_lower, s, starts_with, units_of, Units};
use risunest_ir::num::{number_to_text, to_int32};
use risunest_ir::{Budget, Text};

/// The tag names of `assetRegex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetTag { Raw, Path, Img, Image, Video, Audio, Bgm, Bg, Emotion, Asset, VideoImg, Source }

const TAGS: [(&str, AssetTag); 12] = [
    ("raw", AssetTag::Raw), ("path", AssetTag::Path), ("img", AssetTag::Img), ("image", AssetTag::Image), ("video", AssetTag::Video),
    ("audio", AssetTag::Audio), ("bgm", AssetTag::Bgm), ("bg", AssetTag::Bg), ("emotion", AssetTag::Emotion), ("asset", AssetTag::Asset),
    ("video-img", AssetTag::VideoImg), ("source", AssetTag::Source),
];

impl AssetTag {
    /// `imageCBS`, the tags `hideAllImages` empties.
    fn hidden_with_images(self) -> bool {
        matches!(self, AssetTag::Img | AssetTag::Image | AssetTag::Emotion | AssetTag::Asset | AssetTag::Bg | AssetTag::Raw | AssetTag::Path)
    }
    /// The tags whose path goes through `resolveImage` before `getFileSrc`.
    fn image_source(self) -> bool { matches!(self, AssetTag::Img | AssetTag::Image | AssetTag::Raw | AssetTag::Path | AssetTag::Asset) }
}

/// `parseAdditionalAssets`'s `mode`: `back` for a background render, `normal` for every other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetMode { Normal, Back }

/// What the stage reads about the character besides its asset index.
pub struct AssetCharacter<'a> {
    /// `renderContext.assetWidth ?? DBState.db.assetWidth`, `None` when both are undefined.
    pub asset_width: Option<f64>,
    /// The render's `chatID`, which with `cha_id` seeds the pick among several paths of one asset.
    pub chat_id: f64,
    /// `char.chaId`.
    pub cha_id: &'a Text,
    /// The paths `{{source::char}}` and `{{source::user}}` stand for: `renderContext.characterImageSource` and
    /// `userImageSource` when the first is defined, else `getCurrentCharacter().image` and `getUserIcon()`. `None` or
    /// empty for no image.
    pub character_image: Option<&'a Text>,
    pub user_image: Option<&'a Text>,
    /// Whether TS throws on a `source` tag: `renderContext.characterImageSource` is undefined and no character is
    /// selected, so `getCurrentCharacter().image` throws once the pass has replaced its tags. Such a pass declines as
    /// `asset-source`, and the app renders the message in TS, which throws as before.
    pub sources_throw: bool,
}

pub struct AssetInputs<'a> {
    pub character: &'a AssetCharacter<'a>,
    /// `renderContext.hideAllImages ?? DBState.db.hideAllImages`.
    pub hide_all_images: bool,
    pub mode: AssetMode,
}

/// One lookup a tag makes in the character's asset index, as written in the text. The host answers with what
/// `resolveEmotionAsset(index, name.toLocaleLowerCase())` returns for `emotion` and with what
/// `resolveAdditionalAsset(index, name.toLocaleLowerCase(), maxDifference)` returns for every other tag, made in this
/// order, since a fuzzy match is remembered in the index for later lookups.
#[derive(Debug, PartialEq)]
pub struct AssetLookup { pub tag: AssetTag, pub name: Text }

/// The `AssetPath` a lookup found.
#[derive(Debug)]
pub struct AssetPath { pub src_paths: Vec<Text>, pub ext: Option<Text> }

/// A path to turn into a URL: `(await resolveImage(path))?.url ?? await getFileSrc(path)` when `image`, else
/// `await getFileSrc(path)`. `resolveImage` resolves an image source only on Tauri and remembers it in the render's
/// inlay registry, which the host does when it answers from one.
#[derive(Debug, PartialEq)]
pub struct AssetUrlRequest { pub path: Text, pub image: bool }

/// The host's answers so far, each `None` until the stage has asked for it.
#[derive(Debug, Clone, Copy, Default)]
pub struct AssetAnswers<'a> {
    /// One result per [`AssetLookup`], in order.
    pub lookups: Option<&'a [Option<AssetPath>]>,
    /// One URL per [`AssetUrlRequest`], in the order of the requests, which the host may resolve in any order: it is
    /// not the order in which TS's concurrent replacers call `getFileSrc` and `getFileImageSource`.
    pub urls: Option<&'a [Text]>,
}

/// What the stage needs next, or its result.
#[derive(Debug)]
pub enum AssetStep {
    Lookups(Vec<AssetLookup>),
    Urls(Vec<AssetUrlRequest>),
    Done(Text),
}

struct Tag { start: usize, end: usize, tag: AssetTag, name: (usize, usize) }

/// The matches of `/{{(raw|path|…|source)::(.+?)}}/gms`. The name runs to the first `}}` one unit or more after `::`.
/// When no such `}}` follows an opener, no later opener has one either: the openers' prefixes hold no `{{`, so a later
/// one starts past this one's `::`.
fn scan(src: &[u16], meter: &Meter) -> Result<Vec<Tag>> {
    meter.work(src.len())?;
    let mut found = Vec::new();
    let mut index = 0;
    while index + 1 < src.len() {
        if src[index] != 0x7b || src[index + 1] != 0x7b {
            index += 1;
            continue;
        }
        let rest = &src[index + 2..];
        let Some(&(name, tag)) = TAGS.iter().find(|(name, _)| starts_with(rest, name) && starts_with(&rest[name.len()..], "::")) else {
            index += 1;
            continue;
        };
        let after = index + 2 + name.len() + 2;
        let close = (after + 1..src.len().saturating_sub(1)).find(|&at| src[at] == 0x7d && src[at + 1] == 0x7d);
        meter.work(close.map_or(src.len(), |close| close + 2) - index)?;
        let Some(close) = close else { break };
        found.push(Tag { start: index, end: close + 2, tag, name: (after, close) });
        index = close + 2;
    }
    Ok(found)
}

fn ascii_equals_ignore_case(src: &[u16], word: &str) -> bool { src.len() == word.len() && src.iter().zip(word.bytes()).all(|(unit, byte)| ascii_lower(*unit) == byte as u16) }

/// `pickHashRand(cid, word)` (`src/ts/util.ts`): a djb2-style hash of `word` taken four times in a row seeds `sfc32`,
/// which then skips `cid % 1000` values.
fn pick_hash_rand(cid: f64, word: &[u16], meter: &Meter) -> Result<f64> {
    let skip = cid % 1000.0;
    // `for (i = 0; i < v; i++)` runs `ceil(v)` times for a positive `v` and never for NaN.
    let skip = if skip > 0.0 { skip.ceil() as usize } else { 0 };
    meter.work(word.len().saturating_mul(4).saturating_add(skip))?;
    // `hashAddress` is a JS number: `<<` works on its Int32 value while `+` adds the full double.
    let mut hash = 5515.0f64;
    let mut state = [0i32; 4];
    for seed in &mut state {
        for &unit in word { hash = to_int32(hash).wrapping_shl(5) as f64 + hash + unit as f64; }
        *seed = to_int32(hash);
    }
    for _ in 0..skip { sfc32(&mut state); }
    Ok(sfc32(&mut state))
}

/// One `sfc32` draw (`src/ts/util.ts`).
fn sfc32(state: &mut [i32; 4]) -> f64 {
    let [mut a, mut b, mut c, mut d] = *state;
    let t = a.wrapping_add(b).wrapping_add(d);
    d = d.wrapping_add(1);
    a = b ^ ((b as u32) >> 9) as i32;
    b = c.wrapping_add(c.wrapping_shl(3));
    c = c.wrapping_shl(21) | ((c as u32) >> 11) as i32;
    c = c.wrapping_add(t);
    *state = [a, b, c, d];
    (t as u32) as f64 / 4294967296.0
}

/// `text.replace(/<needle>/g, replacement)` with a string replacement, for a needle without special characters.
fn replace_all(text: &[u16], needle: &[u16], replacement: &[u16], meter: &Meter) -> Result<Units> {
    meter.work(text.len())?;
    let mut out = Units::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if text[index..].starts_with(needle) {
            let before = out.len();
            substitute(replacement, text, index, index + needle.len(), &mut out);
            meter.alloc(2 * (out.len() - before))?;
            meter.text(out.len())?;
            index += needle.len();
        } else {
            out.push(text[index]);
            index += 1;
        }
    }
    Ok(out)
}

enum Plan { Hidden, Placeholder(&'static str), Lookup(usize) }

const CHAR_PLACEHOLDER: &str = "\u{e9b4}CHAR\u{e9b4}";
const USER_PLACEHOLDER: &str = "\u{e9b4}USER\u{e9b4}";
const VIDEO_EXTENSIONS: [&str; 5] = ["mp4", "webm", "avi", "m4p", "m4v"];

/// `parseAdditionalAssets(data, char, mode, { ch: chatID }, renderContext)`. Called without answers, it asks for the
/// lookups; with them, for the URLs; with both, it returns the text. Answers whose count differs from the requests
/// decline as `asset-lookups` or `asset-urls`, and a found asset without a path as `asset-paths`, since the app's index
/// never holds one and the app would read `undefined` as its path. A pass with a `source` tag declines as
/// `asset-source` when [`AssetCharacter::sources_throw`] holds.
pub fn parse_additional_assets(data: &Text, inputs: &AssetInputs, answers: &AssetAnswers, budget: &Budget) -> Result<AssetStep> {
    let meter = Meter::new(budget);
    let src: Units = units_of(&data);
    meter.alloc(4 * src.len())?;
    let tags = scan(&src, &meter)?;
    let mut lookups = Vec::new();
    let mut plans = Vec::with_capacity(tags.len());
    let mut sources = false;
    for tag in &tags {
        let name = &src[tag.name.0..tag.name.1];
        if inputs.hide_all_images && tag.tag.hidden_with_images() {
            plans.push(Plan::Hidden);
            continue;
        }
        if tag.tag == AssetTag::Source {
            sources = true;
            // `name.toLocaleLowerCase()` is `char` or `user` exactly for an ASCII spelling of it in any case: the only
            // code points that lowercase to ASCII are ASCII letters, U+212A KELVIN SIGN (`k`) and, under Turkish,
            // U+0130 (`i`), which the oracle records.
            if ascii_equals_ignore_case(name, "char") { plans.push(Plan::Placeholder(CHAR_PLACEHOLDER)); continue; }
            if ascii_equals_ignore_case(name, "user") { plans.push(Plan::Placeholder(USER_PLACEHOLDER)); continue; }
        }
        plans.push(Plan::Lookup(lookups.len()));
        meter.alloc(2 * name.len() + 32)?;
        lookups.push(AssetLookup { tag: tag.tag, name: Text::from_units(name, budget)? });
    }
    if sources && inputs.character.sources_throw { return Err(RenderError::Unsupported("asset-source")); }
    let found: &[Option<AssetPath>] = match answers.lookups {
        _ if lookups.is_empty() => &[],
        None => {
            meter.finish()?;
            return Ok(AssetStep::Lookups(lookups));
        }
        Some(found) if found.len() != lookups.len() => return Err(RenderError::Unsupported("asset-lookups")),
        Some(found) => found,
    };
    let character = inputs.character;
    let mut requests: Vec<AssetUrlRequest> = Vec::new();
    // For each lookup, the request whose URL it writes.
    let mut urls_of = Vec::with_capacity(lookups.len());
    let mut pick: Option<f64> = None;
    for (lookup, found) in lookups.iter().zip(found) {
        let path = match (lookup.tag, found) {
            (_, None) => None,
            (AssetTag::Emotion, Some(found)) => found.src_paths.first().filter(|path| path.len_units() > 0).map(|path| (path, true)),
            (tag, Some(found)) => {
                let mut index = 0;
                if found.src_paths.len() > 1 {
                    let value = match pick {
                        Some(value) => value,
                        None => {
                            let mut word: Units = if character.cha_id.len_units() == 0 { s("global") } else { character.cha_id.units().collect() };
                            word.extend(number_to_text(character.chat_id, budget)?.units());
                            let value = pick_hash_rand(character.chat_id, &word, &meter)?;
                            pick = Some(value);
                            value
                        }
                    };
                    index = (value * found.src_paths.len() as f64).floor() as usize;
                }
                let Some(path) = found.src_paths.get(index) else { return Err(RenderError::Unsupported("asset-paths")) };
                Some((path, tag.image_source()))
            }
        };
        urls_of.push(match path {
            Some((path, image)) => {
                requests.push(AssetUrlRequest { path: path.copy(budget)?, image });
                Some(requests.len() - 1)
            }
            None => None,
        });
    }
    let mut placeholders = Vec::new();
    if sources {
        for (placeholder, image) in [(CHAR_PLACEHOLDER, character.character_image), (USER_PLACEHOLDER, character.user_image)] {
            let request = match image.filter(|path| path.len_units() > 0) {
                Some(path) => {
                    requests.push(AssetUrlRequest { path: path.copy(budget)?, image: true });
                    Some(requests.len() - 1)
                }
                None => None,
            };
            placeholders.push((placeholder, request));
        }
    }
    let urls: &[Text] = match answers.urls {
        _ if requests.is_empty() => &[],
        None => {
            meter.finish()?;
            return Ok(AssetStep::Urls(requests));
        }
        Some(urls) if urls.len() != requests.len() => return Err(RenderError::Unsupported("asset-urls")),
        Some(urls) => urls,
    };
    let urls: Vec<Units> = urls.iter().map(|url| url.units().collect()).collect();
    let width = match character.asset_width {
        // `(assetWidth && assetWidth !== -1 || assetWidth === 0)`: every number but NaN and -1.
        Some(width) if !width.is_nan() && width != -1.0 => [&s("max-width:")[..], &number_to_text(width, budget)?.units().collect::<Units>(), &s("rem;")].concat(),
        _ => Units::new(),
    };
    let mut out = Units::with_capacity(src.len());
    let mut pos = 0;
    for (tag, plan) in tags.iter().zip(&plans) {
        out.extend_from_slice(&src[pos..tag.start]);
        pos = tag.end;
        let before = out.len();
        let lookup = match plan {
            Plan::Hidden => continue,
            Plan::Placeholder(placeholder) => {
                out.extend(s(placeholder));
                continue;
            }
            Plan::Lookup(lookup) => *lookup,
        };
        let Some(url) = urls_of[lookup].map(|request| &urls[request][..]) else { continue };
        let ext = found[lookup].as_ref().and_then(|found| found.ext.as_ref());
        let push = |out: &mut Units, parts: &[&[u16]]| for part in parts { out.extend_from_slice(part) };
        let image = |out: &mut Units, tail: &str| push(out, &[&s("<img src=\""), url, &s("\" alt=\""), url, &s("\" style=\""), &width, &s(tail)]);
        match lookups[lookup].tag {
            AssetTag::Emotion if url.is_empty() => {}
            AssetTag::Emotion | AssetTag::Img => image(&mut out, " \"/>"),
            AssetTag::Raw | AssetTag::Path => out.extend_from_slice(url),
            AssetTag::Image => {
                out.extend(s("<div class=\"risu-inlay-image\">"));
                image(&mut out, "\"/></div>\n");
            }
            AssetTag::Video => push(&mut out, &[&s("<video controls autoplay loop><source src=\""), url, &s("\" type=\"video/mp4\"></video>\n")]),
            AssetTag::VideoImg => push(&mut out, &[&s("<video autoplay muted loop><source src=\""), url, &s("\" type=\"video/mp4\"></video>\n")]),
            AssetTag::Audio => push(&mut out, &[&s("<audio controls autoplay loop><source src=\""), url, &s("\" type=\"audio/mpeg\"></audio>\n")]),
            AssetTag::Bg if inputs.mode == AssetMode::Back => push(&mut out, &[
                &s("<div style=\"width:100%;height:100%;background: linear-gradient(rgba(0, 0, 0, 0.8), rgba(0, 0, 0, 0.8)),url("), url,
                &s("); background-size: cover;\"></div>"),
            ]),
            AssetTag::Bg | AssetTag::Source => {}
            AssetTag::Asset if ext.is_some_and(|ext| VIDEO_EXTENSIONS.iter().any(|video| ext.units().eq(video.encode_utf16()))) => {
                push(&mut out, &[&s("<video autoplay muted loop><source src=\""), url, &s("\" type=\"video/mp4\"></video>\n")]);
            }
            AssetTag::Asset => image(&mut out, " \"/>\n"),
            AssetTag::Bgm => push(&mut out, &[&s("<div risu-ctrl=\"bgm___auto___"), url, &s("\" style=\"display:none;\"></div>\n")]),
        }
        meter.alloc(2 * (out.len() - before))?;
        meter.text(out.len())?;
    }
    out.extend_from_slice(&src[pos..]);
    for (placeholder, request) in placeholders {
        let url = request.map(|request| urls[request].clone()).unwrap_or_default();
        out = replace_all(&out, &s(placeholder), &url, &meter)?;
    }
    meter.text(out.len())?;
    meter.finish()?;
    Ok(AssetStep::Done(Text::from_units(&out, budget)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use std::sync::{atomic::AtomicBool, Arc};

    fn budget(max_work: usize) -> Budget {
        let limits = Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 24, max_text_units: 1 << 24, max_values: 1 << 22, max_depth: 128,
            max_work };
        Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits")
    }

    /// A line of 10 000 openers without `}}` costs work linear in its length, and so does one closed tag after
    /// 10 000 unmatched `{{` pairs.
    #[test]
    fn unclosed_openers_scan_each_unit_once() {
        for line in ["{{img::".repeat(10_000), "{{".repeat(10_000) + "{{raw::a}}"] {
            let budget = budget(8 * line.len());
            let meter = Meter::new(&budget);
            let src = s(&line);
            let tags = scan(&src, &meter).expect("linear work");
            meter.finish().expect("linear work");
            assert!(tags.len() <= 1);
        }
    }
}
