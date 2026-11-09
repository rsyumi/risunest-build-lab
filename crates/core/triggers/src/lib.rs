//! Character and module triggers (`runTrigger`, `src/ts/process/triggers.ts`) as a sans-IO stage.
//!
//! [`runner::TriggerRun`] runs the start, input, output and manual events over a [`input::TriggerInput`] copied from
//! the stored records. It runs the v1 and v2 effects that need no Lua, plugin or display host: variables and locals,
//! conditions and loops, chat reads and edits, strings, arrays and dictionaries, system prompts and nested
//! `v2RunTrigger` calls. Anything else declines in admission with a named `StageError::Unsupported`, which
//! [`runner::admit`] reports without running; a value the stage cannot reproduce, such as an object CBS would read as a
//! `{text, var}` result, declines when the run reaches it.

pub mod input;
pub mod runner;

mod js;
mod script;
mod vars;
