use risunest_ir::stage::StageError;
use risunest_ir::Error;
use std::borrow::Cow;

/// Why a template did not render.
#[derive(Debug, Clone, PartialEq)]
pub enum JinjaError {
    /// `new Template(source)` throws: the template does not tokenize or parse.
    Syntax(String),
    /// `render` throws.
    Runtime(String),
    /// `raise_exception(message)`, with the message as UTF-16 units.
    Raised(Vec<u16>),
    /// A JS behavior the port refuses on purpose, by name; the caller keeps the JS engine for the template.
    /// `jinja-depth` is nesting past the depth bounds, which JS renders until its own stack runs out.
    Unsupported(&'static str),
    /// A budget limit or cancellation.
    Budget(Error),
}

impl From<Error> for JinjaError {
    fn from(value: Error) -> Self { Self::Budget(value) }
}

impl From<JinjaError> for StageError {
    fn from(value: JinjaError) -> Self {
        match value {
            JinjaError::Syntax(_) => StageError::Invalid(Cow::Borrowed("jinja-syntax")),
            JinjaError::Runtime(_) => StageError::Invalid(Cow::Borrowed("jinja-runtime")),
            JinjaError::Raised(_) => StageError::Invalid(Cow::Borrowed("jinja-raise-exception")),
            JinjaError::Unsupported(name) => StageError::Unsupported(Cow::Borrowed(name)),
            JinjaError::Budget(error) => error.into(),
        }
    }
}

pub(crate) fn syntax(message: impl Into<String>) -> JinjaError { JinjaError::Syntax(message.into()) }
pub(crate) fn runtime(message: impl Into<String>) -> JinjaError { JinjaError::Runtime(message.into()) }
pub(crate) fn too_deep() -> JinjaError { JinjaError::Unsupported("jinja-depth") }
