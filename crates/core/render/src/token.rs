//! markdown-it `Token` (`lib/token.mjs`), with the token types the rules and the renderer distinguish. Every other
//! type renders through `renderToken` from its tag, nesting and attributes, so it is `Generic`.

use crate::utils::Units;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Text,
    TextSpecial,
    Inline,
    Softbreak,
    Hardbreak,
    CodeInline,
    Fence,
    HtmlBlock,
    HtmlInline,
    Image,
    LinkOpen,
    LinkClose,
    ParagraphOpen,
    SClose,
    Generic,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub(crate) kind: Kind,
    pub(crate) tag: &'static str,
    pub(crate) attrs: Vec<(&'static str, Units)>,
    pub(crate) nesting: i8,
    pub(crate) level: i32,
    pub(crate) children: Vec<Token>,
    pub(crate) content: Units,
    /// Fence info string.
    pub(crate) info: Units,
    /// `token.info === 'auto'` on autolink `link_open` and `link_close`.
    pub(crate) auto: bool,
    pub(crate) block: bool,
    pub(crate) hidden: bool,
}

impl Token {
    pub(crate) fn new(kind: Kind, tag: &'static str, nesting: i8) -> Self {
        Self { kind, tag, attrs: Vec::new(), nesting, level: 0, children: Vec::new(), content: Units::new(), info: Units::new(), auto: false, block: false, hidden: false }
    }

    /// Bytes this token holds, for the render's memory charge.
    pub(crate) fn weight(&self) -> usize {
        std::mem::size_of::<Self>() + 2 * (self.content.len() + self.info.len()) + self.attrs.iter().map(|(_, value)| 2 * value.len() + 24).sum::<usize>()
    }
}
