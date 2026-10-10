//! Provider-independent native external storage boundaries. No test provider is
//! compiled into the product; unimplemented services remain absent from the UI.
#[cfg(test)]
mod address_change_tests;
pub(crate) mod admission;
pub(crate) mod auth;
pub(crate) mod cache_hydration;
pub(crate) mod capabilities;
pub(crate) mod capture;
pub(crate) mod cleanup;
pub(crate) mod connection;
pub(crate) mod connection_commands;
pub(crate) mod connection_store;
pub(crate) mod content_store;
pub(crate) mod contract;
pub(crate) mod control;
#[cfg(test)]
mod daily_download_resume_tests;
pub(crate) mod descriptor;
pub(crate) mod durable_quota;
#[cfg(test)]
pub(crate) mod fake;
pub(crate) mod gc_store;
pub(crate) mod history;
pub(crate) mod history_deletion;
pub(crate) mod history_jobs;
pub(crate) mod http;
#[cfg(test)]
mod http_tests;
pub(crate) mod job_store;
pub(crate) mod leftovers;
pub(crate) mod journal;
pub(crate) mod leases;
pub(crate) mod lease_ledger;
pub(crate) mod oauth;
pub(crate) mod package_cache;
pub(crate) mod packaging;
pub(crate) mod phase_progress;
pub(crate) mod progress;
pub(crate) mod providers;
pub(crate) mod publication;
pub(crate) mod quota;
pub(crate) mod quota_profiles;
pub(crate) mod reachability;
pub(crate) mod recovery;
#[cfg(test)]
mod recovery_integration_tests;
pub(crate) mod registry;
pub(crate) mod repository_check;
pub(crate) mod runtime;
pub(crate) mod runtime_restore;
pub(crate) mod secrets;
pub(crate) mod sections;
pub(crate) mod snapshot_export;
pub(crate) mod snapshot_export_commands;
pub(crate) mod snapshot_restore;
#[cfg(test)]
pub(crate) mod worker_observation;
#[cfg(test)]
mod tests;
pub(crate) mod transfer;
pub(crate) mod transfer_job;
pub(crate) mod usage;
#[cfg(test)]
pub(crate) mod wire_fixture;
pub(crate) mod lww_segment;
pub(crate) mod lww_engine;
pub(crate) mod lww_checkpoint;
pub(crate) mod lww_compaction;

pub(crate) mod lww_residency;
pub(crate) mod lww_commands;
#[cfg(test)]
pub(crate) mod lww_tests;
#[cfg(test)]
mod lww_large_unit_tests;
#[cfg(test)]
pub(crate) mod previous_storage_tests;
