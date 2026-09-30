#[tauri::command]
pub(crate) fn ios_bench_peak_rss() -> Result<serde_json::Value, String> {
    if !super::ios_bench_phase().starts_with("legacy-restore-") {
        return Err("Synthetic legacy restore measurement phase required".into());
    }
    #[cfg(target_os = "ios")]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // getrusage writes the initialized process-local result; Darwin reports ru_maxrss in bytes.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let peak = unsafe { usage.assume_init() }.ru_maxrss;
        if peak <= 0 { return Err("Native peak RSS unavailable".into()); }
        Ok(serde_json::json!({"peakRssBytes": peak, "source": "darwin-getrusage-process-lifetime-bytes"}))
    }
    #[cfg(not(target_os = "ios"))]
    Err("iOS benchmark process required".into())
}
