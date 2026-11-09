//! `lib/renderer.mjs` of markdown-it 14.3.2 with the default rules and `xhtmlOut: false`, `breaks: true`,
//! `langPrefix: 'language-'`. The `highlight` option is the app's `mdHighlight` placeholder or absent.

use crate::error::Result;
use crate::meter::Meter;
use crate::token::{Kind, Token};
use crate::utils::{escape_html, is_js_space, js_trim, s, unescape_all, Units};
use crate::Engine;

pub(crate) struct Renderer<'m, 'b> {
    pub(crate) engine: Engine,
    pub(crate) meter: &'m Meter<'b>,
    pub(crate) out: Units,
}

impl Renderer<'_, '_> {
    fn text(&mut self, text: &str) { self.out.extend(text.encode_utf16()); }

    fn check(&self) -> Result<()> {
        self.meter.text(self.out.len())?;
        Ok(())
    }

    /// `renderAttrs`.
    fn attrs(&mut self, attrs: &[(&'static str, Units)]) {
        for (name, value) in attrs {
            self.out.push(0x20);
            escape_html(&s(name), &mut self.out);
            self.text("=\"");
            escape_html(value, &mut self.out);
            self.out.push(0x22);
        }
    }

    /// `renderToken`.
    fn token(&mut self, tokens: &[Token], index: usize, attrs: &[(&'static str, Units)]) {
        let token = &tokens[index];
        if token.hidden { return; }
        if token.block && token.nesting != -1 && index > 0 && tokens[index - 1].hidden { self.out.push(0x0a); }
        self.text(if token.nesting == -1 { "</" } else { "<" });
        self.text(token.tag);
        self.attrs(attrs);
        let mut need_lf = false;
        if token.block {
            need_lf = true;
            if token.nesting == 1 {
                if let Some(next) = tokens.get(index + 1) {
                    if next.kind == Kind::Inline || next.hidden || (next.nesting == -1 && next.tag == token.tag) { need_lf = false; }
                }
            }
        }
        self.text(if need_lf { ">\n" } else { ">" });
    }

    /// `renderInlineAsText`.
    fn inline_as_text(tokens: &[Token], out: &mut Units) {
        for token in tokens {
            match token.kind {
                Kind::Text | Kind::HtmlInline | Kind::HtmlBlock => out.extend_from_slice(&token.content),
                Kind::Image => Self::inline_as_text(&token.children, out),
                Kind::Softbreak | Kind::Hardbreak => out.push(0x0a),
                _ => {}
            }
        }
    }

    /// The `fence` rule.
    fn fence(&mut self, token: &Token) -> Result<()> {
        let info = if token.info.is_empty() { Units::new() } else { js_trim(&unescape_all(&token.info, self.meter)?).to_vec() };
        let lang_name: &[u16] = match info.iter().position(|unit| is_js_space(*unit)) {
            Some(end) => &info[..end],
            None => &info,
        };
        if self.engine == Engine::Highlight && !lang_name.is_empty() {
            // `<pre-hljs-placeholder lang="${lang}">` + str + '</pre-hljs-placeholder>' starts with `<pre`.
            self.text("<pre-hljs-placeholder lang=\"");
            self.out.extend_from_slice(lang_name);
            self.text("\">");
            self.out.extend_from_slice(&token.content);
            self.text("</pre-hljs-placeholder>\n");
            return Ok(());
        }
        self.text("<pre><code");
        if !info.is_empty() {
            let mut class = s("language-");
            class.extend_from_slice(lang_name);
            self.attrs(&[("class", class)]);
        }
        self.text(">");
        escape_html(&token.content, &mut self.out);
        self.text("</code></pre>\n");
        Ok(())
    }

    /// `renderInline`.
    fn inline(&mut self, tokens: &[Token]) -> Result<()> {
        for index in 0..tokens.len() {
            let token = &tokens[index];
            self.meter.work(1)?;
            match token.kind {
                Kind::Text => escape_html(&token.content, &mut self.out),
                Kind::CodeInline => {
                    self.text("<code>");
                    escape_html(&token.content, &mut self.out);
                    self.text("</code>");
                }
                Kind::Softbreak | Kind::Hardbreak => self.text("<br>\n"),
                Kind::HtmlInline | Kind::HtmlBlock => self.out.extend_from_slice(&token.content),
                Kind::Image => {
                    let mut alt = Units::new();
                    Self::inline_as_text(&token.children, &mut alt);
                    let attrs: Vec<(&'static str, Units)> = token.attrs.iter()
                        .map(|(name, value)| (*name, if *name == "alt" { alt.clone() } else { value.clone() }))
                        .collect();
                    self.token(tokens, index, &attrs);
                }
                Kind::Fence => self.fence(token)?,
                _ => self.token(tokens, index, &token.attrs),
            }
            self.check()?;
        }
        Ok(())
    }

    /// `render`.
    pub(crate) fn render(&mut self, tokens: &[Token]) -> Result<()> {
        for index in 0..tokens.len() {
            let token = &tokens[index];
            self.meter.work(1)?;
            match token.kind {
                Kind::Inline => self.inline(&token.children)?,
                Kind::Fence => self.fence(token)?,
                Kind::HtmlBlock | Kind::HtmlInline => self.out.extend_from_slice(&token.content),
                Kind::Text => escape_html(&token.content, &mut self.out),
                _ => self.token(tokens, index, &token.attrs),
            }
            self.check()?;
        }
        Ok(())
    }
}
