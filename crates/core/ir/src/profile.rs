//! Execution profile reporting.

use crate::stage::HostKind;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Where one stage of a native operation ran, as its diagnostics report it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "class", content = "name", rename_all = "kebab-case")]
pub enum EngineClass {
    Rust,
    NativeRuntime(Cow<'static, str>),
    CompatRoute(Cow<'static, str>),
    /// A host callback answered the stage, such as Lua or a plugin hook.
    Host(HostKind),
}
