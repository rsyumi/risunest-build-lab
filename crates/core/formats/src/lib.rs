//! Import and export mappings between stored records and interchange formats: the steps between an already decoded
//! container (PNG chunks, charx zip, RisuSave blocks, which stay in the app) and the stored record.
//!
//! Each mapping is a pure function over [`risunest_ir::Value`] that reproduces its TS source step for step, including
//! property order, so `json::stringify` of the result equals `JSON.stringify` of what TS returns. Random IDs and the
//! clock are inputs ([`ids::Ids`], `risunest_ir::clock::Clock`). A shape TS handles with JS semantics this crate does
//! not reproduce (a string iterated as characters, a property assigned on a primitive, a TypeError) is a decline,
//! `StageError::Unsupported(name)`, so the owner runs the TS mapping instead; an error TS raises on purpose is
//! `StageError::Invalid(name)`. A mapping returns before any output either way.

pub mod card_export;
pub mod card_import;
mod charbook;
pub mod database_defaults;
pub mod ids;
mod js;
pub mod module_export;
pub mod module_import;
pub mod persistence;
pub mod preset;
pub mod risusave;
