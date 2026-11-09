use risunest_ir::stage::StageError;
use risunest_ir::Error;
use std::borrow::Cow;

/// Why a Markdown source did not render.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderError {
    /// A markdown-it behavior the port does not reproduce, by name; the caller renders this source with markdown-it.
    Unsupported(&'static str),
    /// A budget limit or cancellation.
    Budget(Error),
}

impl From<Error> for RenderError {
    fn from(value: Error) -> Self { Self::Budget(value) }
}

impl From<RenderError> for StageError {
    fn from(value: RenderError) -> Self {
        match value {
            RenderError::Unsupported(name) => StageError::Unsupported(Cow::Borrowed(name)),
            RenderError::Budget(error) => error.into(),
        }
    }
}

pub(crate) type Result<T> = std::result::Result<T, RenderError>;

/// A state markdown-it reports by throwing (a rule that matched without advancing); the port declines instead.
pub(crate) fn invariant() -> RenderError { RenderError::Unsupported("markdown-invariant") }
