use risunest_lww_measurement::fixture::{generate, FixtureScale};
use risunest_lww_measurement::measurement::{NetworkProfile, ALL_SCENARIOS};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("plan") if args.len() == 1 => {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "schema":"risunest.lww-measurement-plan/v1", "status":"drivers required",
                "scales":[FixtureScale::small(),FixtureScale::target(),FixtureScale::above_target()],
                "scenarios":ALL_SCENARIOS, "smokeNetwork":NetworkProfile::loopback()
            }))?);
        }
        Some("generate") if args.len() == 3 || args.len() == 4 => {
            let scale = match args[1].as_str() {
                "small" => FixtureScale::small(), "target" => FixtureScale::target(),
                "above" => FixtureScale::above_target(), other => serde_json::from_str(other)?,
            };
            let bodies = match args.get(3).map(String::as_str) {
                None => false, Some("--bodies") => true, _ => return Err("expected --bodies".into()),
            };
            let receipt = generate(Path::new(&args[2]), scale, bodies)?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        _ => return Err("usage: risunest-lww-measurement plan | generate <small|target|above|scale-JSON> <new-output-directory> [--bodies]".into()),
    }
    Ok(())
}
