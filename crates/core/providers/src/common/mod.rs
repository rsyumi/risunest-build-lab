//! Helpers shared by provider families: JavaScript value semantics ([`js`]), request body serialization ([`wire`]),
//! `new URL(..).href` ([`url`]), `applyParameters` and `applyAdditionalParameters` ([`params`]), and the owner glue
//! every family uses ([`owner`]).

pub mod built;
pub mod js;
pub mod jsprop;
pub mod owner;
pub mod params;
pub mod reformat;
#[cfg(test)]
pub(crate) mod replay_kit;
pub mod schema;
pub mod tools;
pub mod settings;
pub(crate) mod sha256;
pub mod url;
pub mod wire;

use risunest_ir::{Error, Text};

/// Why a request could not be built.
#[derive(Debug)]
pub enum BuildError {
    /// An input or setting the native path does not run. Before a snapshot has reached the caller and before any tool
    /// has run, the owner falls back to the TS path; afterwards the request ends with a terminal failure
    /// ([`owner::settle`]).
    Unsupported(&'static str),
    /// TS returns `{ type: 'fail', result }` before any network call.
    Fail(Text),
    /// TS throws; the text is the error message.
    Thrown(Text),
    /// A budget, cancellation or text limit.
    Budget(Error),
}

impl From<Error> for BuildError {
    fn from(value: Error) -> Self { Self::Budget(value) }
}
