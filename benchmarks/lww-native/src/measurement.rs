use super::fixture::FixtureScale;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Scenario {
    SettingEdit, PresetSwitch, PersonaSwitch, Append, MiddleEdit, Insertion, Deletion,
    OversizedMessage, SparseBoundary, Burst, GenerationCompletion, OrdinaryResume,
    FullBootstrap, RestoreAllPresent, RestoreMissing, ConsolidatedSnapshot,
    IncomparableSnapshots, DuringCompaction, DuringBackup, DuringAssetTransfer,
}

pub const ROUTINE_SCENARIOS: &[Scenario] = &[
    Scenario::SettingEdit, Scenario::PresetSwitch, Scenario::PersonaSwitch,
    Scenario::Append, Scenario::MiddleEdit, Scenario::Burst, Scenario::GenerationCompletion,
];

pub const ALL_SCENARIOS: &[Scenario] = &[
    Scenario::SettingEdit, Scenario::PresetSwitch, Scenario::PersonaSwitch, Scenario::Append,
    Scenario::MiddleEdit, Scenario::Insertion, Scenario::Deletion, Scenario::OversizedMessage,
    Scenario::SparseBoundary, Scenario::Burst, Scenario::GenerationCompletion,
    Scenario::OrdinaryResume, Scenario::FullBootstrap, Scenario::RestoreAllPresent,
    Scenario::RestoreMissing, Scenario::ConsolidatedSnapshot, Scenario::IncomparableSnapshots,
    Scenario::DuringCompaction, Scenario::DuringBackup, Scenario::DuringAssetTransfer,
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FixedChange {
    pub character_id: String,
    pub conversation_id: String,
    pub message_index: u64,
    pub replacement_text: String,
    pub burst_changes: u32,
    pub preset_id: String,
    pub persona_id: String,
}

impl Default for FixedChange {
    fn default() -> Self {
        Self { character_id: "synthetic-character-0".into(), conversation_id: "synthetic-conversation-0".into(),
            message_index: 2048, replacement_text: "fixed synthetic edit".into(), burst_changes: 16,
            preset_id: "synthetic-preset-1".into(), persona_id: "synthetic-persona-1".into() }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkCounters {
    pub units_visited: u64,
    pub bytes_hashed: u64,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub request_count: u64,
    pub asset_bodies_read: u64,
    pub asset_body_bytes_read: u64,
    pub messages_visited: u64,
    pub pages_reused: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CounterEvidence {
    pub unit_visits_and_hash_bytes: bool,
    pub transfer_bytes_and_requests: bool,
    pub asset_body_reads: bool,
    pub message_page_work: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaintenanceCounters {
    pub catalog_entries_visited: u64,
    pub peak_temporary_disk_bytes: u64,
    pub uncached_input_bytes_read: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timings {
    pub durable_save_ms: Option<f64>,
    pub publish_ms: Option<f64>,
    pub foreground_apply_ms: Option<f64>,
    pub library_usable_ms: Option<f64>,
    pub all_bodies_local_ms: Option<f64>,
    pub elapsed_ms: f64,
}

/// Phase times are measured from the same operation start, before the edit is issued.
pub struct PhaseClock {
    start: Instant,
    timings: Timings,
}

impl Default for PhaseClock {
    fn default() -> Self {
        Self { start: Instant::now(), timings: Timings { durable_save_ms: None, publish_ms: None,
            foreground_apply_ms: None, library_usable_ms: None, all_bodies_local_ms: None, elapsed_ms: 0.0 } }
    }
}

impl PhaseClock {
    fn now(&self) -> f64 { self.start.elapsed().as_secs_f64() * 1000.0 }
    pub fn durable(&mut self) { self.timings.durable_save_ms = Some(self.now()); }
    pub fn published(&mut self) { self.timings.publish_ms = Some(self.now()); }
    pub fn foreground_applied(&mut self) { self.timings.foreground_apply_ms = Some(self.now()); }
    pub fn usable(&mut self) { self.timings.library_usable_ms = Some(self.now()); }
    pub fn bodies_local(&mut self) { self.timings.all_bodies_local_ms = Some(self.now()); }
    pub fn finish(mut self) -> Timings { self.timings.elapsed_ms = self.now(); self.timings }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkProfile {
    pub name: String,
    pub round_trip_ms: u64,
    pub upload_bytes_per_second: Option<u64>,
    pub download_bytes_per_second: Option<u64>,
    pub packet_loss_basis_points: u16,
}

impl NetworkProfile {
    pub fn loopback() -> Self {
        Self { name: "Windows loopback, no injected shaping".into(), round_trip_ms: 0,
            upload_bytes_per_second: None, download_bytes_per_second: None, packet_loss_basis_points: 0 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub scenario: Scenario,
    pub iteration: u32,
    pub counters: WorkCounters,
    pub evidence: CounterEvidence,
    pub timings: Timings,
    pub maintenance: Option<MaintenanceCounters>,
}

impl Sample {
    pub fn validate(&self) -> Result<(), String> {
        if !self.evidence.unit_visits_and_hash_bytes || !self.evidence.transfer_bytes_and_requests {
            return Err("unit/hash or transport counters were not instrumented".into());
        }
        if matches!(self.scenario, Scenario::Append | Scenario::MiddleEdit | Scenario::Insertion |
            Scenario::Deletion | Scenario::OversizedMessage | Scenario::SparseBoundary) && !self.evidence.message_page_work {
            return Err("message-page counters were not instrumented".into());
        }
        if matches!(self.scenario, Scenario::RestoreAllPresent | Scenario::RestoreMissing | Scenario::FullBootstrap)
            && !self.evidence.asset_body_reads {
            return Err("asset-body reads were not instrumented".into());
        }
        let t = &self.timings;
        for value in [t.durable_save_ms, t.publish_ms, t.foreground_apply_ms,
            t.library_usable_ms, t.all_bodies_local_ms, Some(t.elapsed_ms)].into_iter().flatten() {
            if !value.is_finite() || value < 0.0 || value > t.elapsed_ms {
                return Err("invalid or out-of-range timing".into());
            }
        }
        if ROUTINE_SCENARIOS.contains(&self.scenario) || matches!(self.scenario,
            Scenario::DuringCompaction | Scenario::DuringBackup | Scenario::DuringAssetTransfer) {
            let (Some(durable), Some(publish), Some(apply)) = (t.durable_save_ms, t.publish_ms, t.foreground_apply_ms) else {
                return Err("routine measurement lacks durable, publish or foreground apply evidence".into());
            };
            if durable > publish || publish > apply { return Err("phase times are out of order".into()); }
        }
        if matches!(self.scenario, Scenario::FullBootstrap | Scenario::RestoreAllPresent | Scenario::RestoreMissing
            | Scenario::ConsolidatedSnapshot | Scenario::IncomparableSnapshots) {
            let (Some(usable), Some(local)) = (t.library_usable_ms, t.all_bodies_local_ms) else {
                return Err("restore lacks library usable or all bodies local evidence".into());
            };
            if usable > local { return Err("restore phase times are out of order".into()); }
        }
        if self.scenario == Scenario::RestoreAllPresent &&
            (self.counters.asset_bodies_read != 0 || self.counters.asset_body_bytes_read != 0) {
            return Err("present-body restore read an asset body".into());
        }
        if self.scenario == Scenario::DuringCompaction && self.maintenance.is_none() {
            return Err("compaction overlap lacks separate maintenance counters".into());
        }
        Ok(())
    }
}

/// Drivers reset native/test-only counters before each operation and return observed work.
/// An uninstrumented driver must return an error instead of a fabricated zero sample.
pub trait MeasurementDriver {
    fn prepare(&mut self, scale: &FixtureScale, network: &NetworkProfile, change: &FixedChange) -> Result<(), String>;
    fn run(&mut self, scenario: Scenario, iteration: u32) -> Result<Sample, String>;
}

#[derive(Serialize, Deserialize)]
pub struct RunReport {
    pub schema: String,
    pub driver: String,
    pub platform: String,
    pub scale: FixtureScale,
    pub network: NetworkProfile,
    pub warmup_iterations: u32,
    pub fixed_change: FixedChange,
    pub samples: Vec<Sample>,
}

pub fn run<D: MeasurementDriver>(driver: &mut D, name: &str, scale: FixtureScale,
    network: NetworkProfile, scenarios: &[Scenario], warmup: u32, repetitions: u32) -> Result<RunReport, String> {
    if repetitions == 0 || scenarios.is_empty() { return Err("a run needs scenarios and measured repetitions".into()); }
    let fixed_change = FixedChange::default();
    driver.prepare(&scale, &network, &fixed_change)?;
    let mut samples = Vec::new();
    for &scenario in scenarios {
        for iteration in 0..warmup {
            let sample = driver.run(scenario, iteration)?;
            if sample.scenario != scenario || sample.iteration != iteration { return Err("driver returned a mismatched sample".into()); }
            sample.validate()?;
        }
        for iteration in 0..repetitions {
            let sample = driver.run(scenario, iteration)?;
            if sample.scenario != scenario || sample.iteration != iteration { return Err("driver returned a mismatched sample".into()); }
            sample.validate()?;
            samples.push(sample);
        }
    }
    Ok(RunReport { schema: "risunest.lww-measurement/v1".into(), driver: name.into(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        scale, network, warmup_iterations: warmup, fixed_change, samples })
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Percentiles { pub count: usize, pub p50: f64, pub p95: f64, pub p99: f64 }

#[derive(Debug, Serialize, Deserialize)]
pub struct TimingSummary {
    pub durable_save_ms: Option<Percentiles>,
    pub publish_ms: Option<Percentiles>,
    pub foreground_apply_ms: Option<Percentiles>,
    pub library_usable_ms: Option<Percentiles>,
    pub all_bodies_local_ms: Option<Percentiles>,
    pub elapsed_ms: Percentiles,
}

pub fn summarize(report: &RunReport, scenario: Scenario) -> Result<TimingSummary, String> {
    let samples: Vec<_> = report.samples.iter().filter(|s| s.scenario == scenario).collect();
    for sample in &samples { sample.validate()?; }
    let phase = |get: fn(&Timings) -> Option<f64>| -> Result<Option<Percentiles>, String> {
        let values: Vec<_> = samples.iter().filter_map(|s| get(&s.timings)).collect();
        if values.is_empty() { return Ok(None); }
        if values.len() != samples.len() { return Err("a phase was measured in only some iterations".into()); }
        percentiles(values).map(Some)
    };
    Ok(TimingSummary {
        durable_save_ms: phase(|t| t.durable_save_ms)?, publish_ms: phase(|t| t.publish_ms)?,
        foreground_apply_ms: phase(|t| t.foreground_apply_ms)?, library_usable_ms: phase(|t| t.library_usable_ms)?,
        all_bodies_local_ms: phase(|t| t.all_bodies_local_ms)?,
        elapsed_ms: percentiles(samples.iter().map(|s| s.timings.elapsed_ms))?,
    })
}

pub fn percentiles(values: impl IntoIterator<Item = f64>) -> Result<Percentiles, String> {
    let mut values: Vec<_> = values.into_iter().collect();
    if values.is_empty() || values.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Err("percentiles require finite nonnegative observations".into());
    }
    values.sort_by(f64::total_cmp);
    let rank = |p: f64| values[((values.len() as f64 * p).ceil() as usize).max(1) - 1];
    Ok(Percentiles { count: values.len(), p50: rank(0.50), p95: rank(0.95), p99: rank(0.99) })
}

/// Checks fixed-change work, without conflating latency variability with a library scan.
pub fn assert_bounded_work(small: &Sample, large: &Sample, allowance: &WorkCounters) -> Result<(), String> {
    small.validate()?;
    large.validate()?;
    if small.scenario != large.scenario { return Err("bounded-work samples have different scenarios".into()); }
    for (name, a, b, extra) in [
        ("units", small.counters.units_visited, large.counters.units_visited, allowance.units_visited),
        ("hash bytes", small.counters.bytes_hashed, large.counters.bytes_hashed, allowance.bytes_hashed),
        ("upload bytes", small.counters.bytes_uploaded, large.counters.bytes_uploaded, allowance.bytes_uploaded),
        ("download bytes", small.counters.bytes_downloaded, large.counters.bytes_downloaded, allowance.bytes_downloaded),
        ("requests", small.counters.request_count, large.counters.request_count, allowance.request_count),
    ] {
        if b > a.saturating_add(extra) { return Err(format!("{name} grew with total library size: {a} -> {b}, allowance {extra}")); }
    }
    Ok(())
}

pub fn duration_ms(duration: Duration) -> f64 { duration.as_secs_f64() * 1000.0 }

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(scenario: Scenario) -> Sample {
        Sample { scenario, iteration: 0, counters: WorkCounters::default(),
            evidence: CounterEvidence { unit_visits_and_hash_bytes: true, transfer_bytes_and_requests: true,
                asset_body_reads: true, message_page_work: true },
            timings: Timings { durable_save_ms: Some(1.0), publish_ms: Some(2.0), foreground_apply_ms: Some(3.0),
                library_usable_ms: Some(2.0), all_bodies_local_ms: Some(3.0), elapsed_ms: 4.0 }, maintenance: None }
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn incomplete_or_inconsistent_evidence_is_rejected() {
        let mut s = sample(Scenario::SettingEdit);
        s.evidence.unit_visits_and_hash_bytes = false;
        assert!(s.validate().is_err());
        s = sample(Scenario::SettingEdit);
        s.timings.publish_ms = None;
        assert!(s.validate().is_err());
        s = sample(Scenario::RestoreAllPresent);
        s.counters.asset_bodies_read = 1;
        assert!(s.validate().is_err());
        s = sample(Scenario::FullBootstrap);
        s.timings.library_usable_ms = Some(3.5);
        assert!(s.validate().is_err());
        assert!(sample(Scenario::DuringCompaction).validate().is_err());
        s = sample(Scenario::SettingEdit);
        s.timings.elapsed_ms = f64::NAN;
        assert!(s.validate().is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn percentiles_use_nearest_rank_and_reject_missing_values() {
        assert_eq!(percentiles((1..=100).map(f64::from)).unwrap(), Percentiles { count: 100, p50: 50.0, p95: 95.0, p99: 99.0 });
        assert!(percentiles([]).is_err());
        assert!(percentiles([f64::INFINITY]).is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn bounded_work_detects_library_scan() {
        let a = sample(Scenario::SettingEdit);
        let mut b = a.clone();
        b.counters.units_visited = 100_000;
        assert!(assert_bounded_work(&a, &b, &WorkCounters::default()).is_err());
        assert_bounded_work(&a, &a, &WorkCounters::default()).unwrap();
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn runner_excludes_warmup_and_propagates_driver_failure() {
        struct Driver { calls: u32, fail: bool }
        impl MeasurementDriver for Driver {
            fn prepare(&mut self, _: &FixtureScale, _: &NetworkProfile, _: &FixedChange) -> Result<(), String> { Ok(()) }
            fn run(&mut self, scenario: Scenario, iteration: u32) -> Result<Sample, String> {
                self.calls += 1;
                if self.fail { return Err("missing native counters".into()); }
                let mut s = sample(scenario); s.iteration = iteration; Ok(s)
            }
        }
        let mut driver = Driver { calls: 0, fail: false };
        let report = run(&mut driver, "test", FixtureScale::small(), NetworkProfile::loopback(), ROUTINE_SCENARIOS, 1, 2).unwrap();
        assert_eq!(report.samples.len(), 14);
        assert_eq!(driver.calls, 21);
        let summary = summarize(&report, Scenario::SettingEdit).unwrap();
        assert_eq!(summary.durable_save_ms.unwrap().p50, 1.0);
        assert_eq!(summary.publish_ms.unwrap().p50, 2.0);
        assert_eq!(summary.foreground_apply_ms.unwrap().p50, 3.0);
        driver.fail = true;
        assert!(run(&mut driver, "test", FixtureScale::small(), NetworkProfile::loopback(), ROUTINE_SCENARIOS, 0, 1).is_err());
    }
}
