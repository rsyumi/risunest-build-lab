use super::{JobSource, NativeJobError, OpenedJobSource};
use crate::asset_repository::exact_file_identity;
use std::{
    collections::HashMap,
    fs::File,
    io::{Seek, SeekFrom},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    Android,
    Ios,
}
type Release = Arc<dyn Fn(Option<&str>) -> Result<(), NativeJobError> + Send + Sync>;
type Identity = Box<dyn Fn(&File) -> Result<bool, NativeJobError> + Send + Sync>;
type Claim = Arc<dyn Fn(&str) -> Result<(), NativeJobError> + Send + Sync>;
type Check = Arc<dyn Fn() -> Result<ProbeGuard, NativeJobError> + Send + Sync>;
struct ProbeGuard {
    finish: Option<Box<dyn Fn() -> Result<(), NativeJobError> + Send + Sync>>,
}
impl ProbeGuard {
    fn finish(mut self) -> Result<(), NativeJobError> {
        if let Some(finish) = self.finish.as_ref() {
            finish()?;
        }
        self.finish = None;
        Ok(())
    }
}
impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if let Some(finish) = self.finish.take() {
            let _ = finish();
        }
    }
}
struct Source {
    file: Option<File>,
    identity: Identity,
    platform: Platform,
    claimed: Option<String>,
    release: Option<Release>,
    claim: Option<Claim>,
    check: Option<Check>,
    release_gate: Arc<Mutex<()>>,
    releasing: bool,
    format: Option<super::BackupSourceFormat>,
}
static SOURCES: OnceLock<Mutex<HashMap<String, Source>>> = OnceLock::new();
static RELEASED: OnceLock<Mutex<Vec<(String, Platform)>>> = OnceLock::new();
fn was_released(token: &str, platform: Platform) -> bool {
    RELEASED
        .get_or_init(Default::default)
        .lock()
        .map(|tokens| {
            tokens
                .iter()
                .any(|entry| entry.0 == token && entry.1 == platform)
        })
        .unwrap_or(false)
}
#[cfg(target_os = "android")]
static ANDROID: OnceLock<(jni::JavaVM, jni::objects::GlobalRef)> = OnceLock::new();
fn sources() -> &'static Mutex<HashMap<String, Source>> {
    SOURCES.get_or_init(Default::default)
}
fn error(message: &str) -> NativeJobError {
    NativeJobError::new("source-reselect-required", message)
}
fn parts(source: &JobSource) -> Result<(&str, Platform), NativeJobError> {
    match source {
        JobSource::AndroidSeekable { token } => Ok((token, Platform::Android)),
        JobSource::IosScoped { token } => Ok((token, Platform::Ios)),
        _ => Err(error("Source is not a portable custody token")),
    }
}
fn validate(file: &mut File) -> Result<(), NativeJobError> {
    let metadata = file
        .metadata()
        .map_err(|_| error("Source metadata is unavailable"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(error("Portable source must be a nonempty regular file"));
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| {
        NativeJobError::new(
            "source-not-seekable",
            "Portable source must support random access",
        )
    })?;
    Ok(())
}
fn register(
    token: &str,
    platform: Platform,
    mut file: File,
    release: Option<Release>,
) -> Result<(), NativeJobError> {
    super::parse_android_spool_token(token)?;
    validate(&mut file)?;
    let captured =
        exact_file_identity(&file).map_err(|_| error("Source identity is unavailable"))?;
    let identity: Identity = Box::new(move |file| {
        exact_file_identity(file)
            .map(|current| current == captured)
            .map_err(|_| error("Source identity is unavailable"))
    });
    let mut selected = sources()
        .lock()
        .map_err(|_| error("Source custody is unavailable"))?;
    if selected.contains_key(token) || selected.len() >= 16 {
        return Err(error("Source token is already owned or custody is full"));
    }
    selected.insert(
        token.to_owned(),
        Source {
            file: Some(file),
            identity,
            platform,
            claimed: None,
            release,
            claim: None,
            check: None,
            release_gate: Arc::new(Mutex::new(())),
            releasing: false,
            format: None,
        },
    );
    Ok(())
}
#[cfg(test)]
fn probe(source: &JobSource) -> Result<OpenedJobSource, NativeJobError> {
    let (token, platform) = parts(source)?;
    let mut selected = sources()
        .lock()
        .map_err(|_| error("Source custody is unavailable"))?;
    let owned = selected
        .get_mut(token)
        .ok_or_else(|| error("Selected source is detached; select it again"))?;
    if owned.platform != platform || owned.claimed.is_some() || owned.releasing {
        return Err(error("Source token is foreign or already claimed"));
    }
    let _probe = owned.check.as_ref().map(|check| check()).transpose()?;
    let file = owned
        .file
        .as_mut()
        .ok_or_else(|| error("Source descriptor is unavailable"))?;
    if !(owned.identity)(file)? {
        return Err(error("Selected source changed"));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| error("Source is no longer seekable"))?;
    let file = file
        .try_clone()
        .map_err(|_| error("Source descriptor cannot be duplicated"))?;
    let total_bytes = file
        .metadata()
        .map_err(|_| error("Source size is unavailable"))?
        .len();
    Ok(OpenedJobSource {
        file,
        total_bytes,
        custody: None,
    })
}
pub(crate) fn claim(source: &JobSource, job_id: &str) -> Result<OpenedJobSource, NativeJobError> {
    super::parse_android_spool_token(job_id)?;
    let (token, platform) = parts(source)?;
    let mut selected = sources()
        .lock()
        .map_err(|_| error("Source custody is unavailable"))?;
    let owned = selected
        .get_mut(token)
        .ok_or_else(|| error("Selected source is detached; select it again"))?;
    if owned.platform != platform || owned.claimed.is_some() || owned.releasing {
        return Err(error("Source token is foreign or already claimed"));
    }
    if owned.format != Some(super::BackupSourceFormat::Portable) {
        return Err(error("Portable metadata has not been probed"));
    }
    let file = owned
        .file
        .as_mut()
        .ok_or_else(|| error("Source descriptor is unavailable"))?;
    validate(file)?;
    if !(owned.identity)(file)? {
        return Err(error("Selected source changed"));
    }
    let total_bytes = file
        .metadata()
        .map_err(|_| error("Source size is unavailable"))?
        .len();
    owned.claimed = Some(job_id.to_owned());
    let lease = PortableSourceCustodyLease {
        token: token.to_owned(),
        job_id: job_id.to_owned(),
        finished: false,
    };
    if let Some(claim) = &owned.claim {
        if let Err(failure) = claim(job_id) {
            drop(selected);
            lease.finish()?;
            return Err(failure);
        }
    }
    let file = owned
        .file
        .take()
        .ok_or_else(|| error("Source descriptor is unavailable"))?;
    Ok(OpenedJobSource {
        file,
        total_bytes,
        custody: Some(lease),
    })
}
pub(crate) fn discard(source: &JobSource) -> Result<bool, NativeJobError> {
    let (token, platform) = parts(source)?;
    super::parse_android_spool_token(token)?;
    release(token, platform, None)
}
pub(crate) fn probe_format(
    source: &JobSource,
) -> Result<super::BackupSourceFormat, NativeJobError> {
    let (token, platform) = parts(source)?;
    let mut selected = sources()
        .lock()
        .map_err(|_| error("Source custody is unavailable"))?;
    let owned = selected
        .get_mut(token)
        .ok_or_else(|| error("Source is detached"))?;
    if owned.platform != platform || owned.claimed.is_some() || owned.releasing {
        return Err(error("Source probe ownership changed"));
    }
    let probe = owned.check.as_ref().map(|check| check()).transpose()?;
    let file = owned
        .file
        .as_mut()
        .ok_or_else(|| error("Source descriptor is unavailable"))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| error("Source is no longer seekable"))?;
    let format = super::backup_source::detect(
        file.try_clone()
            .map_err(|_| error("Source descriptor is unavailable"))?,
    )?;
    if !(owned.identity)(file)? {
        return Err(error("Selected source changed during probe"));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| error("Source is no longer seekable"))?;
    if let Some(probe) = probe {
        probe.finish()?;
    }
    owned.format = Some(format);
    Ok(format)
}
pub(crate) fn confirm_platform_probe(
    app: &tauri::AppHandle,
    source: &JobSource,
    format: super::BackupSourceFormat,
) -> Result<(), NativeJobError> {
    let (_, platform) = parts(source)?;
    #[cfg(target_os = "ios")]
    if platform == Platform::Ios {
        use tauri_plugin_ios_native::IosNativeExt;
        let kind = match format {
            super::BackupSourceFormat::Portable => "portable",
            super::BackupSourceFormat::BlockRisuSave => "block-risu-save",
            super::BackupSourceFormat::LocalBackup => "local-backup",
        };
        let (token, _) = parts(source)?;
        app.ios_native()
            .confirm_portable_source_format(token, kind)
            .map_err(|_| error("Source probe confirmation failed"))?;
    }
    let _ = (app, platform, format);
    Ok(())
}
fn release(token: &str, platform: Platform, job_id: Option<&str>) -> Result<bool, NativeJobError> {
    let (cleanup, gate) = {
        let mut selected = sources()
            .lock()
            .map_err(|_| error("Source custody is unavailable"))?;
        let Some(owned) = selected.get_mut(token) else {
            return Ok(was_released(token, platform));
        };
        if owned.platform != platform || owned.claimed.as_deref() != job_id {
            return Ok(false);
        }
        owned.releasing = true;
        (owned.release.clone(), owned.release_gate.clone())
    };
    let _cleanup = gate
        .lock()
        .map_err(|_| error("Source cleanup is unavailable"))?;
    {
        let selected = sources()
            .lock()
            .map_err(|_| error("Source custody is unavailable"))?;
        let Some(owned) = selected.get(token) else {
            return Ok(was_released(token, platform));
        };
        if owned.platform != platform || owned.claimed.as_deref() != job_id {
            return Ok(false);
        }
    }
    if let Some(cleanup) = cleanup {
        cleanup(job_id)?;
    }
    let mut selected = sources()
        .lock()
        .map_err(|_| error("Source custody is unavailable"))?;
    if selected
        .get(token)
        .is_some_and(|owned| owned.platform == platform && owned.claimed.as_deref() == job_id)
    {
        selected.remove(token);
        let mut tokens = RELEASED
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| error("Source release receipt is unavailable"))?;
        tokens.push((token.to_owned(), platform));
        if tokens.len() > 32 {
            tokens.remove(0);
        }
    }
    Ok(true)
}

pub(crate) fn ensure_platform_source(
    app: &tauri::AppHandle,
    source: &JobSource,
) -> Result<(), NativeJobError> {
    let (token, platform) = parts(source)?;
    super::parse_android_spool_token(token)?;
    if was_released(token, platform) {
        return Ok(());
    }
    if sources()
        .lock()
        .map_err(|_| error("Source custody is unavailable"))?
        .contains_key(token)
    {
        return Ok(());
    }
    #[cfg(target_os = "ios")]
    if platform == Platform::Ios {
        use tauri_plugin_ios_native::IosNativeExt;
        let broker = app.ios_native().clone();
        let pending = Arc::new(Mutex::new(None));
        let initial_probe = ios_probe(&broker, token, &pending)?;
        let descriptor = broker
            .portable_source_descriptor(token, None)
            .map_err(|_| error("Scoped source is detached; select it again"))?;
        let file = duplicate_descriptor(descriptor.fd)?;
        if file
            .metadata()
            .map_err(|_| error("Source metadata is unavailable"))?
            .len()
            != descriptor.bytes
        {
            return Err(error("Scoped source size changed"));
        }
        let release_broker = broker.clone();
        let release_token = token.to_owned();
        let release_pending = pending.clone();
        let release: Release = Arc::new(move |job_id| {
            ios_finish_probe(&release_broker, &release_token, &release_pending)?;
            match release_broker.release_portable_source(&release_token, job_id) {
                Ok(true) => Ok(()),
                _ => Err(NativeJobError::new(
                    "cleanup-failed",
                    "Scoped source release did not complete",
                )),
            }
        });
        register(token, platform, file, Some(release))?;
        let check_broker = broker.clone();
        let check_token = token.to_owned();
        let check_pending = pending.clone();
        let check: Check = Arc::new(move || ios_probe(&check_broker, &check_token, &check_pending));
        let claim_token = token.to_owned();
        let claim: Claim = Arc::new(move |job_id| {
            broker
                .portable_source_descriptor(&claim_token, Some(job_id))
                .map(|_| ())
                .map_err(|_| error("Scoped source claim did not complete"))
        });
        let mut selected = sources()
            .lock()
            .map_err(|_| error("Source custody is unavailable"))?;
        let owned = selected
            .get_mut(token)
            .ok_or_else(|| error("Source ownership changed"))?;
        owned.claim = Some(claim);
        owned.check = Some(check);
        initial_probe.finish()?;
        return Ok(());
    }
    let _ = (app, platform);
    Err(error("Selected source is detached; select it again"))
}
#[cfg(target_os = "ios")]
fn ios_finish_probe(
    broker: &tauri_plugin_ios_native::IosNative<tauri::Wry>,
    token: &str,
    pending: &Arc<Mutex<Option<String>>>,
) -> Result<(), NativeJobError> {
    let mut pending = pending
        .lock()
        .map_err(|_| error("Scoped probe ownership is unavailable"))?;
    if let Some(probe) = pending.as_ref() {
        match broker.end_portable_source_probe(token, probe) {
            Ok(true) => *pending = None,
            _ => {
                return Err(NativeJobError::new(
                    "cleanup-failed",
                    "Scoped probe completion could not be confirmed",
                ))
            }
        }
    }
    Ok(())
}
#[cfg(target_os = "ios")]
fn ios_probe(
    broker: &tauri_plugin_ios_native::IosNative<tauri::Wry>,
    token: &str,
    pending: &Arc<Mutex<Option<String>>>,
) -> Result<ProbeGuard, NativeJobError> {
    ios_finish_probe(broker, token, pending)?;
    let probe = uuid::Uuid::new_v4().to_string();
    *pending
        .lock()
        .map_err(|_| error("Scoped probe ownership is unavailable"))? = Some(probe.clone());
    let finish_broker = broker.clone();
    let finish_token = token.to_owned();
    let finish_pending = pending.clone();
    let mut guard = ProbeGuard {
        finish: Some(Box::new(move || {
            ios_finish_probe(&finish_broker, &finish_token, &finish_pending)
        })),
    };
    if !broker
        .begin_portable_source_probe(token, &probe)
        .map_err(|_| error("Scoped source probe outcome is unavailable"))?
    {
        *pending
            .lock()
            .map_err(|_| error("Scoped probe ownership is unavailable"))? = None;
        guard.finish = None;
        return Err(error("Scoped source owner has retired; select it again"));
    }
    Ok(guard)
}
/// Releases the sources an earlier page picked but never imported, and returns how many there were.
pub(crate) fn cleanup_orphans(app: &tauri::AppHandle) -> Result<usize, NativeJobError> {
    #[cfg(target_os = "ios")]
    {
        use tauri_plugin_ios_native::IosNativeExt;
        let retired = app
            .ios_native()
            .portable_source_orphans()
            .map_err(|_| error("Source owner reset could not be confirmed"))?;
        cleanup_orphan_tokens(&retired, |token| {
            app.ios_native()
                .acknowledge_portable_source_orphan(token)
                .map_err(|_| error("Source orphan cleanup acknowledgement is unavailable"))
        })
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = app;
        Ok(0)
    }
}
#[cfg(any(target_os = "ios", test))]
fn cleanup_orphan_tokens(
    tokens: &[String],
    mut acknowledge: impl FnMut(&str) -> Result<bool, NativeJobError>,
) -> Result<usize, NativeJobError> {
    for token in tokens {
        cleanup_orphan(token, &mut acknowledge)?;
    }
    Ok(tokens.len())
}
#[cfg(any(target_os = "ios", test))]
fn cleanup_orphan(
    token: &str,
    acknowledge: impl FnOnce(&str) -> Result<bool, NativeJobError>,
) -> Result<(), NativeJobError> {
    super::parse_android_spool_token(token)?;
    release(token, Platform::Ios, None)?;
    let selected = sources()
        .lock()
        .map_err(|_| error("Source custody is unavailable"))?;
    if selected.contains_key(token) {
        return Err(NativeJobError::new(
            "cleanup-failed",
            "Source orphan still has a native owner",
        ));
    }
    // Hold registration ownership until the exact platform receipt is consumed.
    if !acknowledge(token)? {
        return Err(NativeJobError::new(
            "cleanup-failed",
            "Source orphan cleanup acknowledgement did not complete",
        ));
    }
    Ok(())
}
#[derive(Debug)]
pub(crate) struct PortableSourceCustodyLease {
    token: String,
    job_id: String,
    finished: bool,
}
impl PortableSourceCustodyLease {
    pub(crate) fn finish(mut self) -> Result<(), NativeJobError> {
        self.release()?;
        self.finished = true;
        Ok(())
    }
    fn release(&self) -> Result<(), NativeJobError> {
        let platform = sources()
            .lock()
            .map_err(|_| error("Source custody is unavailable"))?
            .get(&self.token)
            .map(|owned| owned.platform)
            .ok_or_else(|| error("Claimed source ownership is unavailable"))?;
        if release(&self.token, platform, Some(&self.job_id))? {
            Ok(())
        } else {
            Err(NativeJobError::new(
                "cleanup-failed",
                "Claimed source cleanup ownership changed",
            ))
        }
    }
}
impl Drop for PortableSourceCustodyLease {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.release();
        }
    }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn duplicate_descriptor(fd: i32) -> Result<File, NativeJobError> {
    use std::os::fd::FromRawFd;
    if fd < 0 {
        return Err(error("Source descriptor is invalid"));
    }
    // Only the native platform broker supplies this borrowed descriptor.
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated < 0 {
        return Err(error("Source descriptor cannot be duplicated"));
    }
    Ok(unsafe { File::from_raw_fd(duplicated) })
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_io_github_rsyumi_risunest_PortableSourceNative_register(
    mut env: jni::JNIEnv,
    class: jni::objects::JClass,
    token: jni::objects::JString,
    fd: jni::sys::jint,
) -> jni::sys::jboolean {
    let accepted = (|| {
        if ANDROID.get().is_none() {
            let _ = ANDROID.set((env.get_java_vm().ok()?, env.new_global_ref(&class).ok()?));
        }
        let token: String = env.get_string(&token).ok()?.into();
        let cleanup_token = token.clone();
        let release: Release = Arc::new(move |_| {
            let (vm, class) = ANDROID
                .get()
                .ok_or_else(|| error("Android source broker is unavailable"))?;
            let mut env = vm
                .attach_current_thread()
                .map_err(|_| error("Android source broker is unavailable"))?;
            let token = env
                .new_string(&cleanup_token)
                .map_err(|_| error("Android source broker is unavailable"))?;
            let class: &jni::objects::JClass = class.as_obj().into();
            let result = env.call_static_method(
                class,
                "releaseDescriptor",
                "(Ljava/lang/String;)Z",
                &[jni::objects::JValue::Object(&token)],
            );
            if result.as_ref().is_err() {
                let _ = env.exception_clear();
            }
            if result.ok().and_then(|value| value.z().ok()) == Some(true) {
                Ok(())
            } else {
                Err(NativeJobError::new(
                    "cleanup-failed",
                    "Android source release did not complete",
                ))
            }
        });
        register(
            &token,
            Platform::Android,
            duplicate_descriptor(fd).ok()?,
            Some(release),
        )
        .ok()?;
        Some(())
    })()
    .is_some();
    u8::from(accepted)
}
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_io_github_rsyumi_risunest_PortableSourceNative_duplicateForCopy(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
    token: jni::objects::JString,
) -> jni::sys::jint {
    let result = (|| {
        use std::os::fd::{AsRawFd, IntoRawFd};
        let token: String = env.get_string(&token).ok()?.into();
        let mut selected = sources().lock().ok()?;
        let owned = selected.get_mut(&token)?;
        if owned.platform != Platform::Android
            || owned.claimed.is_some()
            || owned.releasing
            || !matches!(
                owned.format,
                Some(
                    super::BackupSourceFormat::BlockRisuSave
                        | super::BackupSourceFormat::LocalBackup
                )
            )
        {
            return None;
        }
        let file = owned.file.as_mut()?;
        if !(owned.identity)(file).ok()? {
            return None;
        }
        file.seek(SeekFrom::Start(0)).ok()?;
        let duplicate = duplicate_descriptor(file.as_raw_fd()).ok()?;
        owned.releasing = true;
        Some(duplicate.into_raw_fd())
    })();
    result.unwrap_or(-1)
}
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_io_github_rsyumi_risunest_PortableSourceNative_verifyCopy(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
    token: jni::objects::JString,
) -> jni::sys::jboolean {
    let verified = (|| {
        let token: String = env.get_string(&token).ok()?.into();
        let selected = sources().lock().ok()?;
        let owned = selected.get(&token)?;
        if owned.platform != Platform::Android || owned.claimed.is_some() || !owned.releasing {
            return None;
        }
        (owned.identity)(owned.file.as_ref()?).ok()
    })()
    .unwrap_or(false);
    u8::from(verified)
}
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_io_github_rsyumi_risunest_PortableSourceNative_discard(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
    token: jni::objects::JString,
) -> jni::sys::jboolean {
    let removed = env
        .get_string(&token)
        .ok()
        .map(String::from)
        .and_then(|token| release(&token, Platform::Android, None).ok())
        .unwrap_or(false);
    u8::from(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lost_claim_response_releases_only_the_exact_owned_job_before_reporting_failure() {
        let (_directory, source) = selected();
        probe_format(&source).unwrap();
        let (token, _) = parts(&source).unwrap();
        let job = uuid::Uuid::new_v4().to_string();
        let expected = job.clone();
        let cleaned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = cleaned.clone();
        {
            let mut selected = sources().lock().unwrap();
            let owned = selected.get_mut(token).unwrap();
            owned.claim = Some(Arc::new(|_| Err(error("Synthetic lost claim response"))));
            owned.release = Some(Arc::new(move |job| {
                assert_eq!(job, Some(expected.as_str()));
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }));
        }
        assert!(claim(&source, &job).is_err());
        assert!(cleaned.load(std::sync::atomic::Ordering::SeqCst));
        assert!(probe(&source).is_err());
        assert!(discard(&source).unwrap());
    }
    #[test]
    fn concurrent_selected_cleanup_closes_the_platform_descriptor_once() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };
        let (_directory, source) = selected();
        let (token, platform) = parts(&source).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let continue_rx = Mutex::new(continue_rx);
        sources().lock().unwrap().get_mut(token).unwrap().release = Some(Arc::new(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).unwrap();
            continue_rx.lock().unwrap().recv().unwrap();
            Ok(())
        }));
        std::thread::scope(|scope| {
            let first = scope.spawn(|| release(token, platform, None).unwrap());
            entered_rx.recv().unwrap();
            let second = scope.spawn(|| release(token, platform, None).unwrap());
            continue_tx.send(()).unwrap();
            assert!(first.join().unwrap());
            assert!(second.join().unwrap());
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    fn selected() -> (tempfile::TempDir, JobSource) {
        selected_on(Platform::Android)
    }
    fn selected_on(platform: Platform) -> (tempfile::TempDir, JobSource) {
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("synthetic.risunest");
        let mut archive = zip::ZipWriter::new(File::create(&path).unwrap());
        archive
            .start_file(
                "format.json",
                zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        archive
            .write_all(br#"{"magic":"risunest-portable-backup"}"#)
            .unwrap();
        archive
            .start_file(
                "unopened-payload",
                zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(&vec![7; 65536]).unwrap();
        archive.finish().unwrap();
        let token = uuid::Uuid::new_v4().to_string();
        register(&token, platform, File::open(path).unwrap(), None).unwrap();
        let source = match platform {
            Platform::Android => JobSource::AndroidSeekable { token },
            Platform::Ios => JobSource::IosScoped { token },
        };
        (directory, source)
    }
    #[test]
    fn orphan_acknowledgement_follows_cleanup_and_failed_ack_remains_retryable() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (_directory, source) = selected_on(Platform::Ios);
        let (token, _) = parts(&source).unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        sources().lock().unwrap().get_mut(token).unwrap().release = Some(Arc::new(|_| {
            Err(NativeJobError::new(
                "cleanup-failed",
                "Synthetic scope failure",
            ))
        }));
        assert!(cleanup_orphan(token, |_| panic!("ACK before native cleanup")).is_err());
        assert!(sources().lock().unwrap().contains_key(token));
        let observed = closed.clone();
        sources().lock().unwrap().get_mut(token).unwrap().release = Some(Arc::new(move |_| {
            observed.store(true, Ordering::SeqCst);
            Ok(())
        }));
        assert!(cleanup_orphan(token, |exact| {
            assert_eq!(exact, token);
            assert!(closed.load(Ordering::SeqCst));
            Ok(false)
        })
        .is_err());
        assert!(!sources().lock().unwrap().contains_key(token));
        cleanup_orphan(token, |exact| {
            assert_eq!(exact, token);
            Ok(true)
        })
        .unwrap();
        let absent = uuid::Uuid::new_v4().to_string();
        cleanup_orphan(&absent, |exact| {
            assert_eq!(exact, absent);
            Ok(true)
        })
        .unwrap();
    }
    #[test]
    fn orphan_cleanup_counts_every_interrupted_source_it_releases() {
        let (_first_directory, first) = selected_on(Platform::Ios);
        let (_second_directory, second) = selected_on(Platform::Ios);
        let tokens = [parts(&first).unwrap().0.to_owned(), parts(&second).unwrap().0.to_owned()];
        let mut acknowledged = Vec::new();
        assert_eq!(
            cleanup_orphan_tokens(&tokens, |token| {
                acknowledged.push(token.to_owned());
                Ok(true)
            })
            .unwrap(),
            2
        );
        assert_eq!(acknowledged, tokens);
        assert_eq!(cleanup_orphan_tokens(&[], |_| panic!("ACK without an orphan")).unwrap(), 0);
    }
    #[test]
    fn orphan_cleanup_does_not_acknowledge_foreign_or_claimed_native_owners() {
        let (_directory, source) = selected();
        let (token, _) = parts(&source).unwrap();
        assert!(cleanup_orphan(token, |_| panic!("ACK of foreign owner")).is_err());
        assert!(discard(&source).unwrap());
        let (_directory, source) = selected_on(Platform::Ios);
        probe_format(&source).unwrap();
        let mut opened = claim(&source, &uuid::Uuid::new_v4().to_string()).unwrap();
        let (token, _) = parts(&source).unwrap();
        assert!(cleanup_orphan(token, |_| panic!("ACK of claimed owner")).is_err());
        opened.custody.take().unwrap().finish().unwrap();
    }
    #[test]
    fn exact_descriptor_is_probed_then_claimed_once_and_released_after_job() {
        let (_directory, source) = selected();
        assert!(probe(&source).unwrap().total_bytes > 65536);
        assert!(claim(&source, &uuid::Uuid::new_v4().to_string()).is_err());
        assert_eq!(
            probe_format(&source).unwrap(),
            super::super::BackupSourceFormat::Portable
        );
        let mut opened = claim(&source, &uuid::Uuid::new_v4().to_string()).unwrap();
        assert!(probe(&source).is_err());
        assert!(!discard(&source).unwrap());
        assert!(claim(&source, &uuid::Uuid::new_v4().to_string()).is_err());
        opened.custody.take().unwrap().finish().unwrap();
        assert!(probe(&source).is_err());
        assert!(discard(&source).unwrap());
    }
    #[test]
    fn foreign_token_changed_file_and_detached_source_fail_closed() {
        let (directory, source) = selected();
        let JobSource::AndroidSeekable { token } = &source else {
            unreachable!()
        };
        assert!(probe(&JobSource::IosScoped {
            token: token.clone()
        })
        .is_err());
        std::fs::write(directory.path().join("synthetic.risunest"), b"changed").unwrap();
        assert!(probe(&source).is_err());
        assert!(claim(&source, &uuid::Uuid::new_v4().to_string()).is_err());
        assert!(discard(&source).unwrap());
        assert!(probe(&source).is_err());
    }
    #[test]
    fn metadata_probe_keeps_original_descriptor_and_never_creates_an_archive_copy() {
        use std::io::{Read, Seek};
        let (directory, source) = selected();
        assert_eq!(
            probe_format(&source).unwrap(),
            super::super::BackupSourceFormat::Portable
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        let mut opened = claim(&source, &uuid::Uuid::new_v4().to_string()).unwrap();
        opened.file.seek(SeekFrom::Start(0)).unwrap();
        let mut header = [0; 4];
        opened.file.read_exact(&mut header).unwrap();
        assert_eq!(&header, b"PK\x03\x04");
        opened.custody.take().unwrap().finish().unwrap();
    }
    #[test]
    fn invalid_fresh_signature_is_not_authorized_for_portable_or_compatibility_copy() {
        let (directory, source) = selected();
        std::fs::write(
            directory.path().join("synthetic.risunest"),
            b"PK\x03\x04invalid",
        )
        .unwrap();
        assert!(probe_format(&source).is_err());
        let (token, _) = parts(&source).unwrap();
        assert_eq!(sources().lock().unwrap().get(token).unwrap().format, None);
        assert!(claim(&source, &uuid::Uuid::new_v4().to_string()).is_err());
        assert!(discard(&source).unwrap());
    }
    #[test]
    fn claimed_scope_release_completes_before_finish_and_wrong_owner_cannot_release() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (_directory, source) = selected();
        probe_format(&source).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let (token, platform) = parts(&source).unwrap();
        sources().lock().unwrap().get_mut(token).unwrap().release = Some(Arc::new(move |job| {
            assert!(job.is_some());
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        let job = uuid::Uuid::new_v4().to_string();
        let mut opened = claim(&source, &job).unwrap();
        assert!(!release(token, platform, Some(&uuid::Uuid::new_v4().to_string())).unwrap());
        assert!(!discard(&source).unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        opened.custody.take().unwrap().finish().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(discard(&source).unwrap());
        assert!(!discard(&JobSource::IosScoped {
            token: token.into()
        })
        .unwrap());
    }
    #[test]
    fn failed_scope_cleanup_retains_exact_job_authority_and_is_not_success() {
        let (_directory, source) = selected();
        probe_format(&source).unwrap();
        let (token, platform) = parts(&source).unwrap();
        sources().lock().unwrap().get_mut(token).unwrap().release = Some(Arc::new(|_| {
            Err(NativeJobError::new(
                "cleanup-failed",
                "Synthetic cleanup failure",
            ))
        }));
        let job = uuid::Uuid::new_v4().to_string();
        let mut opened = claim(&source, &job).unwrap();
        assert_eq!(
            opened.custody.take().unwrap().finish().unwrap_err().code,
            "cleanup-failed"
        );
        assert!(!discard(&source).unwrap());
        assert!(probe(&source).is_err());
        assert_eq!(
            sources()
                .lock()
                .unwrap()
                .get(token)
                .unwrap()
                .claimed
                .as_deref(),
            Some(job.as_str())
        );
        sources().lock().unwrap().get_mut(token).unwrap().release = None;
        assert!(release(token, platform, Some(&job)).unwrap());
    }
}
