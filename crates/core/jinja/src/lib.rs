//! A port of `@huggingface/jinja` 0.5.10, the engine `new Template(source).render(context)` runs for chat
//! templates (`src/ts/process/templates/chatTemplate.ts`). Lexing, parsing and rendering follow the JS sources
//! (`lexer.ts`, `parser.ts`, `runtime.ts`, `utils.ts`) step for step, including their JS value semantics: loose `==`,
//! JS string conversion of runtime values, UTF-16 lengths and code-point slicing. A template the JS engine rejects is
//! rejected here; error text is not matched. The few JS behaviors this port refuses on purpose are
//! [`JinjaError::Unsupported`], so the caller can keep the JS engine for that template.
//!
//! Every allocation and step is charged to the [`Budget`], and nesting is bounded by [`Limits::max_depth`]: parse
//! nesting and the nesting of a value turned into text by `max_depth`, evaluation depth by four times that. At
//! `max_depth` 128 a debug build needs up to about 4 MiB of stack, so these bounds assume a stage thread with at least
//! an 8 MiB stack. A template that nests deeper is declined as `Unsupported("jinja-depth")`,
//! so the caller keeps the JS engine for it.
//!
//! [`Limits::max_depth`]: risunest_ir::Limits

mod ast;
mod error;
mod lexer;
mod parser;
mod runtime;
mod units;
mod value;

use risunest_ir::{Budget, Text, Value};

pub use error::JinjaError;

/// The local date and time `strftime_now` formats (`new Date()` in the WebView's time zone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    /// 1 to 12.
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

/// `Intl.DateTimeFormat(undefined, { month: "long" | "short" })` names for `%B` and `%b`, January first.
#[derive(Debug)]
pub struct MonthNames {
    pub long: [Text; 12],
    pub short: [Text; 12],
}

/// The tables of the JS engine's `toUpperCase` and `toLowerCase`, from the host's probes of its Unicode version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseTables {
    /// Rust's own tables: an engine whose case mapping is the Unicode version of Rust's.
    Native,
    /// Unicode 16: Rust's tables except where `risunest_ir::text::unicode16_case_differs` finds a difference.
    Unicode16,
    /// Tables the port does not reproduce, or an engine that was not probed: only ASCII maps alike.
    #[default]
    Foreign,
}

impl CaseTables {
    /// Whether the engine's tables can map `units`, or the case context around them, differently from Rust's.
    pub fn differs(self, units: &[u16]) -> bool {
        match self {
            CaseTables::Native => false,
            CaseTables::Unicode16 => risunest_ir::text::unicode16_case_differs(units),
            CaseTables::Foreign => units.iter().any(|unit| *unit >= 0x80),
        }
    }
}

/// Host values the JS engine reads from its environment.
#[derive(Debug, Default)]
pub struct RenderOptions {
    /// `strftime_now` declines without it.
    pub now: Option<LocalTime>,
    /// `%b` and `%B` decline without it.
    pub month_names: Option<MonthNames>,
    /// `upper`, `lower` and `capitalize`, the `upper` and `lower` tests and case-insensitive sorting decline a text these
    /// tables could map differently, as `jinja-case-table`.
    pub case_tables: CaseTables,
}

/// A parsed template: `new Template(source)`, which tokenizes with `lstrip_blocks` and `trim_blocks`.
pub struct Template {
    program: Vec<ast::Node>,
}

impl Template {
    pub fn new(source: &Text, budget: &Budget) -> Result<Self, JinjaError> {
        let units = units::collect(source, budget)?;
        let tokens = lexer::tokenize(&units, budget)?;
        let program = parser::parse(tokens, budget)?;
        Ok(Self { program })
    }

    /// `template.render(context)`. `context` is an object whose own properties become global variables in order, as
    /// `Object.entries(items)` gives them; a name the globals already declare (`true`, `range`, `namespace`, ...) is the
    /// JS "Variable already declared" error.
    pub fn render(&self, context: &Value, options: &RenderOptions, budget: &Budget) -> Result<Text, JinjaError> {
        let output = runtime::render(&self.program, context, options, budget)?;
        Ok(Text::from_units(&output, budget)?)
    }
}

/// `new Template(source).render(context)`.
pub fn render(source: &Text, context: &Value, options: &RenderOptions, budget: &Budget) -> Result<Text, JinjaError> {
    Template::new(source, budget)?.render(context, options, budget)
}
