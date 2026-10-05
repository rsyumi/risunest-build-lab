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
        let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info>::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        // task_info fills the current resident size, which the restore gate compares against its start.
        #[allow(deprecated)]
        let status = unsafe {
            libc::task_info(libc::mach_task_self(), libc::MACH_TASK_BASIC_INFO, info.as_mut_ptr() as libc::task_info_t, &mut count)
        };
        if status != libc::KERN_SUCCESS { return Err(format!("task_info failed: {status}")); }
        let resident = unsafe { info.assume_init() }.resident_size;
        if resident == 0 { return Err("Native resident size unavailable".into()); }
        Ok(serde_json::json!({"peakRssBytes": peak, "residentBytes": resident, "source": "darwin-task-resident-and-getrusage-lifetime-bytes"}))
    }
    #[cfg(not(target_os = "ios"))]
    Err("iOS benchmark process required".into())
}
