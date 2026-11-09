pub(crate) mod operation;
pub(crate) mod inputs;
pub(crate) mod prepare;
pub(crate) mod preparation_effects;
pub(crate) mod cbs;
mod scripts;
mod lore;
mod modules;
mod rng;
mod triggers;
pub(crate) mod service;
pub(crate) mod owner;
mod owner_response;
#[cfg(test)]
mod owner_tests;

#[cfg(test)]
mod ollama_transport_tests;
#[cfg(test)]
mod hypa_http_tests;
pub(crate) mod ollama_decode;
pub(crate) mod ollama_transport;

pub(crate) mod commands;
#[cfg(test)]
mod commands_tests;
