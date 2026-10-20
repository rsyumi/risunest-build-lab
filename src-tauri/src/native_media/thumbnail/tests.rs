use super::*;
use crate::asset_repository::PayloadCas;
use image::{ImageEncoder, ImageFormat, Rgba, RgbaImage};
use tauri::http::{header, Method};
use tempfile::TempDir;

fn image_bytes(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let image = RgbaImage::from_fn(width, height, |x, y| Rgba([seed, (x % 256) as u8, (y % 256) as u8, 255]));
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(image).write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
    bytes
}

fn stored(root: &Path, source: &[u8]) -> String {
    PayloadCas::new(root).unwrap().prepare_bytes(source).unwrap().content_hash
}

fn read(file: (File, u64)) -> Vec<u8> {
    let (mut file, expected) = file;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes.len() as u64, expected);
    bytes
}

fn request(hash: &str, size: u64, method: Method, validator: Option<&str>) -> Request<()> {
    let mut request = Request::builder().method(method).uri(format!(
        "http://risuasset.localhost/{}?mime=image%2Fpng&size={size}",
        hex::encode(crate::asset_repository::object_physical_key(hash)),
    ));
    if let Some(validator) = validator { request = request.header(header::IF_NONE_MATCH, validator); }
    request.body(()).unwrap()
}

#[test]
fn large_originals_have_bounded_previews_without_changing_source_bytes_or_identity() {
    let root = TempDir::new().unwrap();
    let source = image_bytes(2048, 1024, 200);
    let hash = stored(root.path(), &source);
    let cache = ThumbnailCache::new(root.path().join("cache"));
    let preview = read(cache.get(root.path(), &hash, source.len() as u64).unwrap());
    assert_eq!(image::guess_format(&preview).unwrap(), ImageFormat::WebP);
    let image = image::load_from_memory(&preview).unwrap();
    assert_eq!((image.width(), image.height()), (512, 256));
    assert_eq!(PayloadCas::new(root.path()).unwrap().read_object(&hash).unwrap().unwrap(), source);
    assert_eq!(PayloadCas::new(root.path()).unwrap().object_lengths().unwrap().len(), 1);
    assert_eq!(read(cache.get(root.path(), &hash, source.len() as u64).unwrap()), preview);
    assert_eq!(fs::read_dir(&cache.directory).unwrap().count(), 1);
}

#[test]
fn phone_photos_and_wide_images_fit_the_unchanged_working_budget() {
    for (width, height, expected) in [(4032, 3024, (512, 384)), (16384, 16, (512, 1))] {
        let source = image_bytes(width, height, 7);
        let preview = encode(&source).unwrap();
        let decoded = image::load_from_memory(&preview).unwrap();
        assert_eq!((decoded.width(), decoded.height()), expected);
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
        .encode_image(&image::RgbImage::from_pixel(4032, 3024, image::Rgb([100, 120, 140]))).unwrap();
    jpeg.resize(jpeg.len() + 4 * 1024 * 1024, 0);
    let preview = image::load_from_memory(&encode(&jpeg).unwrap()).unwrap();
    assert_eq!((preview.width(), preview.height()), (512, 384));
}

#[test]
fn progressive_and_multiscan_jpeg_coefficients_are_reserved_before_image_decoding() {
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg).encode_image(&image::RgbImage::new(32, 24)).unwrap();
    let frame = jpeg.windows(2).position(|bytes| bytes == [0xff, 0xc0]).unwrap();
    jpeg[frame + 5..frame + 7].copy_from_slice(&4096_u16.to_be_bytes());
    jpeg[frame + 7..frame + 9].copy_from_slice(&4096_u16.to_be_bytes());
    let scan = jpeg.windows(2).position(|bytes| bytes == [0xff, 0xda]).unwrap();
    let scan_end = scan + 2 + usize::from(u16::from_be_bytes([jpeg[scan + 2], jpeg[scan + 3]]));
    let mut progressive = jpeg.clone();
    progressive[frame + 1] = 0xc2;
    progressive[scan_end - 2] = 0;
    assert!(matches!(encode(&progressive), Err(ThumbnailError::Fallback)));
    let first_component = jpeg[scan + 5];
    let first_table = jpeg[scan + 6];
    jpeg.splice(scan + 2..scan_end, [0, 8, 1, first_component, first_table, 0, 63, 0]);
    assert!(matches!(encode(&jpeg), Err(ThumbnailError::Fallback)));
    jpeg[frame + 5..frame + 7].copy_from_slice(&3500_u16.to_be_bytes());
    jpeg[frame + 7..frame + 9].copy_from_slice(&3500_u16.to_be_bytes());
    let frame_length = usize::from(u16::from_be_bytes([jpeg[frame + 2], jpeg[frame + 3]]));
    jpeg[frame + 2..frame + 4].copy_from_slice(&((frame_length + 3) as u16).to_be_bytes());
    jpeg[frame + 9] = 4;
    jpeg.splice(frame + 2 + frame_length..frame + 2 + frame_length, [
        4, 0x11, 0,
        0xff, 0xee, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, 1,
    ]);
    assert!(matches!(encode(&jpeg), Err(ThumbnailError::Fallback)));
}

#[test]
fn preview_keeps_exif_orientation_after_bounded_sampling() {
    let pixels = image::RgbImage::from_pixel(1024, 512, image::Rgb([200, 80, 10]));
    let mut source = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut source);
    encoder.set_exif_metadata(vec![
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0,
        0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
    ]).unwrap();
    encoder.write_image(pixels.as_raw(), 1024, 512, image::ExtendedColorType::Rgb8).unwrap();
    let preview = image::load_from_memory(&encode(&source).unwrap()).unwrap();
    assert_eq!((preview.width(), preview.height()), (256, 512));
}

#[test]
fn high_bit_depth_surfaces_are_charged_at_their_decoded_size() {
    let image = image::ImageBuffer::from_pixel(64, 32, image::Rgba([0_u16, 100, 200, u16::MAX]));
    let mut source = Vec::new();
    DynamicImage::ImageRgba16(image).write_to(&mut Cursor::new(&mut source), ImageFormat::Png).unwrap();
    let budget = 64 * 32 * 36 + 2 * source.len() as u64;
    assert!(matches!(encode_with_budget(&source, budget), Err(ThumbnailError::Fallback)));
    assert!(encode_with_budget(&source, budget * 2).is_ok());
}

#[test]
fn verified_fallbacks_avoid_body_reads_but_recheck_availability_and_stay_bounded() {
    use crate::asset_repository::body_io::{reset_body_io, take_body_io};
    let root = TempDir::new().unwrap();
    let mut cache = ThumbnailCache::new(root.path().join("cache"));
    cache.decode_bytes = 256;
    cache.max_entries = 1;
    let source = image_bytes(32, 16, 1);
    let hash = stored(root.path(), &source);
    let request_original = || request(&hash, source.len() as u64, Method::GET, None);
    assert!(cache.response(root.path(), request_original()).is_none());
    reset_body_io();
    assert!(cache.response(root.path(), request_original()).is_none());
    let work = take_body_io();
    assert!(work.domains.is_empty(), "verified fallback must not reread the original: {work:?}");
    let other = image_bytes(32, 16, 2);
    let other_hash = stored(root.path(), &other);
    assert!(cache.response(root.path(), request(&other_hash, other.len() as u64, Method::GET, None)).is_none());
    assert_eq!(cache.index.lock().unwrap().fallbacks.len(), 1);
    let path = PayloadCas::new(root.path()).unwrap().object_path(&other_hash).unwrap().unwrap();
    fs::remove_file(path).unwrap();
    assert_eq!(cache.response(root.path(), request(&other_hash, other.len() as u64, Method::GET, None)).unwrap().status(), StatusCode::NOT_FOUND);
}

#[test]
fn cached_responses_revalidate_source_availability_without_reading_original_bodies() {
    use crate::asset_repository::body_io::{reset_body_io, take_body_io};
    use crate::server_sync::residency::{Residency, test_remote};
    let root = TempDir::new().unwrap();
    let source = image_bytes(32, 16, 1);
    let size = source.len() as u64;
    let hash = stored(root.path(), &source);
    let path = PayloadCas::new(root.path()).unwrap().object_path(&hash).unwrap().unwrap();
    let cache = ThumbnailCache::new(root.path().join("cache"));
    let cold = cache.response(root.path(), request(&hash, size, Method::GET, None)).unwrap();
    assert_eq!(cold.status(), StatusCode::OK);
    assert_eq!(cold.headers()[header::CACHE_CONTROL], "no-cache");
    let validator = cold.headers()[header::ETAG].to_str().unwrap().to_owned();
    drop(cold);
    reset_body_io();
    for method in [Method::GET, Method::HEAD] {
        let warm = cache.response(root.path(), request(&hash, size, method.clone(), None)).unwrap();
        assert_eq!(warm.status(), StatusCode::OK);
        assert_eq!(warm.headers()[header::CACHE_CONTROL], "no-cache");
        let conditional = cache.response(root.path(), request(&hash, size, method, Some(&validator))).unwrap();
        assert_eq!(conditional.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(conditional.headers()[header::CACHE_CONTROL], "no-cache");
    }
    let work = take_body_io();
    assert!(work.domains.is_empty(), "warm responses must not open or read original bodies: {work:?}");
    assert!(work.complete());
    assert!(!Residency::exists(root.path()));
    assert_eq!(cache.response(root.path(), request(&hash, size + 1, Method::GET, Some(&validator))).unwrap().status(), StatusCode::NOT_FOUND);

    test_remote::hold(root.path(), &[(&hash, size)]);
    fs::write(&path, &source[..source.len() - 1]).unwrap();
    for method in [Method::GET, Method::HEAD] {
        assert_eq!(cache.response(root.path(), request(&hash, size, method, Some(&validator))).unwrap().status(), StatusCode::NOT_FOUND);
    }
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(cache.response(root.path(), request(&hash, size, Method::GET, Some(&validator))).unwrap().status(), StatusCode::NOT_FOUND);
    fs::remove_dir(&path).unwrap();
    let residency = Residency::open(root.path()).unwrap();
    let object = residency.object(&hash, None).unwrap().unwrap();
    assert!(residency.begin_release(&object).unwrap());
    residency.finish_release(&object).unwrap();
    for method in [Method::GET, Method::HEAD] {
        for validator in [None, Some(validator.as_str())] {
            assert_eq!(cache.response(root.path(), request(&hash, size, method.clone(), validator)).unwrap().status(), StatusCode::NOT_FOUND);
        }
    }
    assert!(cache.path(&hash).exists());
}

#[test]
fn color_profiles_and_alpha_survive_derived_encoding() {
    for alpha in [0, 255] {
        let mut source = Vec::new();
        let profile = vec![17; 133];
        let mut encoder = image::codecs::png::PngEncoder::new(&mut source);
        encoder.set_icc_profile(profile.clone()).unwrap();
        encoder.write_image(&[255, 32, 0, alpha].repeat(1024), 32, 32, image::ExtendedColorType::Rgba8).unwrap();
        let encoded = encode(&source).unwrap();
        let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(&encoded)).unwrap();
        assert_eq!(decoder.icc_profile().unwrap().unwrap(), profile);
        assert_eq!(decoder.dimensions(), (32, 32));
        assert_eq!(image::load_from_memory(&encoded).unwrap().into_rgba8().get_pixel(0, 0)[3], alpha);
        assert_eq!(u32::from_le_bytes(encoded[4..8].try_into().unwrap()) as usize + 8, encoded.len());
    }
}

#[test]
fn animated_images_become_one_still_preview() {
    use image::Frame;
    let mut gif = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
        encoder.encode_frames([Frame::new(RgbaImage::from_pixel(640, 320, Rgba([255, 0, 0, 255]))),
            Frame::new(RgbaImage::from_pixel(640, 320, Rgba([0, 0, 255, 255])))]).unwrap();
    }
    let encoded = encode(&gif).unwrap();
    let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(&encoded)).unwrap();
    assert_eq!(decoder.dimensions(), (512, 256));
    assert!(!decoder.has_animation());
    let pixel = image::load_from_memory(&encoded).unwrap().into_rgba8().get_pixel(0, 0).0;
    assert!(pixel[0] > 240 && pixel[2] < 15, "{pixel:?}");
}

#[test]
fn decode_failure_and_memory_budget_never_return_original_bytes_or_publish_a_cache_file() {
    let source = image_bytes(32, 16, 1);
    assert!(encode_with_budget(&source, 1024).is_err());
    assert!(encode(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
    let root = TempDir::new().unwrap();
    let corrupt = b"not a raster image";
    let hash = stored(root.path(), corrupt);
    let cache = ThumbnailCache::new(root.path().join("cache"));
    assert!(cache.get(root.path(), &hash, corrupt.len() as u64).is_err());
    assert_eq!(fs::read_dir(&cache.directory).unwrap().count(), 0);
    assert_eq!(PayloadCas::new(root.path()).unwrap().read_object(&hash).unwrap().unwrap(), corrupt);
}

#[test]
fn corrupt_original_and_wrong_declared_size_are_refused() {
    let root = TempDir::new().unwrap();
    let source = image_bytes(16, 16, 1);
    let hash = stored(root.path(), &source);
    let mut cache = ThumbnailCache::new(root.path().join("cache"));
    assert!(cache.get(root.path(), &hash, source.len() as u64 + 1).is_err());
    let path = PayloadCas::new(root.path()).unwrap().object_path(&hash).unwrap().unwrap();
    let replaced = (2..=255).map(|seed| image_bytes(16, 16, seed))
        .find(|bytes| bytes.len() == source.len()).unwrap();
    fs::write(&path, replaced).unwrap();
    assert!(cache.get(root.path(), &hash, source.len() as u64).is_err());
    assert_eq!(fs::read_dir(&cache.directory).unwrap().count(), 0);
    fs::write(path, &source).unwrap();
    cache.max_bytes = 0;
    assert!(cache.get(root.path(), &hash, source.len() as u64).is_err());
    cache.max_bytes = DISK_BYTES;
    let preview = read(cache.get(root.path(), &hash, source.len() as u64).unwrap());
    assert_eq!(image::load_from_memory(&preview).unwrap().width(), 16);
}

#[test]
fn disk_entry_and_byte_budgets_evict_least_recently_used_derivatives_only() {
    for byte_bound in [false, true] {
        let root = TempDir::new().unwrap();
        let sources = (0..3).map(|seed| image_bytes(16, 16, seed)).collect::<Vec<_>>();
        let hashes = sources.iter().map(|source| stored(root.path(), source)).collect::<Vec<_>>();
        let mut cache = ThumbnailCache::new(root.path().join("cache"));
        if byte_bound {
            cache.max_bytes = sources.iter().map(|source| encode(source).unwrap().len() as u64).max().unwrap() * 2;
        } else { cache.max_entries = 2; }
        for (hash, source) in hashes[..2].iter().zip(&sources) {
            drop(cache.get(root.path(), hash, source.len() as u64).unwrap());
        }
        drop(cache.cached(&hashes[0]).unwrap().unwrap());
        drop(cache.get(root.path(), &hashes[2], sources[2].len() as u64).unwrap());
        assert!(cache.path(&hashes[0]).is_file());
        assert!(!cache.path(&hashes[1]).exists());
        assert!(cache.path(&hashes[2]).is_file());
        assert_eq!(PayloadCas::new(root.path()).unwrap().object_lengths().unwrap().len(), 3);
        let index = cache.index.lock().unwrap();
        assert!(index.bytes <= cache.max_bytes && index.entries.len() <= cache.max_entries);
    }
}

#[test]
fn cached_corruption_is_regenerated_and_orphan_staging_is_removed_on_restart() {
    let root = TempDir::new().unwrap();
    let source = image_bytes(32, 16, 1);
    let hash = stored(root.path(), &source);
    let cache = ThumbnailCache::new(root.path().join("cache"));
    let expected = read(cache.get(root.path(), &hash, source.len() as u64).unwrap());
    fs::write(cache.path(&hash), b"broken").unwrap();
    fs::write(cache.directory.join(".gallery-orphan"), b"incomplete").unwrap();
    let transient = cache.directory.join("asset-transient-crashed");
    fs::create_dir(&transient).unwrap();
    fs::write(transient.join("original"), b"unfinished remote input").unwrap();
    let restarted = ThumbnailCache::new(root.path().join("cache"));
    assert_eq!(read(restarted.get(root.path(), &hash, source.len() as u64).unwrap()), expected);
    assert_eq!(fs::read_dir(&cache.directory).unwrap().count(), 1);
}

#[test]
fn same_hash_requests_publish_one_shared_derivative() {
    use crate::server_sync::residency::test_remote;
    for remote in [false, true] {
        let root = TempDir::new().unwrap();
        let source = image_bytes(64, 32, 1);
        let size = source.len() as u64;
        let hash = if remote {
            let _store = crate::persistent_store::PersistentStore::open(root.path()).unwrap();
            let hash = hex::encode(Sha256::digest(&source));
            test_remote::hold(root.path(), &[(&hash, size)]);
            test_remote::serve(root.path(), &hash, source);
            hash
        } else { stored(root.path(), &source) };
        let cache = std::sync::Arc::new(ThumbnailCache::new(root.path().join("cache")));
        cache.cached(&hash).unwrap();
        let generation = remote.then(|| cache.generation.lock().unwrap());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(5));
        let received = std::thread::scope(|scope| {
            let threads = (0..4).map(|_| {
                let cache = cache.clone(); let barrier = barrier.clone(); let hash = hash.clone();
                let root = root.path();
                scope.spawn(move || { barrier.wait(); read(cache.get(root, &hash, size).unwrap()) })
            }).collect::<Vec<_>>();
            barrier.wait();
            if remote {
                // Keep the derivative absent while concurrent callers enter the source path.
                let started = std::time::Instant::now();
                while cache.waiting.load(Ordering::Acquire) < 3 && test_remote::fetched(root.path()) < 4 && started.elapsed() < Duration::from_secs(5) {
                    std::thread::yield_now();
                }
            }
            drop(generation);
            threads.into_iter().map(|thread| thread.join().unwrap()).collect::<Vec<_>>()
        });
        assert!(received.iter().all(|bytes| bytes == &received[0]));
        assert_eq!(fs::read_dir(&cache.directory).unwrap().count(), 1);
        assert_eq!(cache.index.lock().unwrap().bytes, received[0].len() as u64);
        assert!(cache.index.lock().unwrap().active.is_empty(), "finished requests must release active ownership");
        if remote {
            assert_eq!(test_remote::fetched(root.path()), 1, "concurrent previews must share one remote download");
            assert!(PayloadCas::new(root.path()).unwrap().object_lengths().unwrap().is_empty());
        }
    }
}

#[test]
fn cleanup_fences_pending_generation_and_late_publication() {
    let root = TempDir::new().unwrap();
    let source = image_bytes(32, 16, 1);
    let hash = stored(root.path(), &source);
    let cache = ThumbnailCache::new(root.path().join("cache"));
    cache.cached(&hash).unwrap();
    let generation = cache.generation.lock().unwrap();
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| cache.get(root.path(), &hash, source.len() as u64));
        cache.close();
        drop(generation);
        assert!(waiting.join().unwrap().is_err());
    });
    {
        use crate::server_sync::residency::test_remote;
        let remote_root = TempDir::new().unwrap();
        let _store = crate::persistent_store::PersistentStore::open(remote_root.path()).unwrap();
        let hash = hex::encode(Sha256::digest(&source));
        let size = source.len() as u64;
        test_remote::hold(remote_root.path(), &[(&hash, size)]);
        test_remote::serve(remote_root.path(), &hash, source.clone());
        let pending = ThumbnailCache::new(remote_root.path().join("cache"));
        pending.cached(&hash).unwrap();
        let generation = pending.generation.lock().unwrap();
        let (finished, completion) = std::sync::mpsc::channel();
        let ready = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let owner = scope.spawn(|| pending.get(remote_root.path(), &hash, size));
            let started = std::time::Instant::now();
            while pending.decoder_waiting.load(Ordering::Acquire) == 0 && started.elapsed() < Duration::from_secs(5) {
                std::thread::yield_now();
            }
            let owner_waiting = pending.decoder_waiting.load(Ordering::Acquire) == 1;
            let waiter = scope.spawn(|| {
                ready.wait();
                let result = pending.get(remote_root.path(), &hash, size);
                finished.send(result.is_err()).unwrap();
            });
            ready.wait();
            let started = std::time::Instant::now();
            while pending.waiting.load(Ordering::Acquire) == 0 && started.elapsed() < Duration::from_secs(5) {
                std::thread::yield_now();
            }
            let entered_wait = pending.waiting.load(Ordering::Acquire) == 1;
            pending.close();
            let waiter_cancelled = completion.recv_timeout(Duration::from_secs(5));
            // Release the decoder even on a regression, so failed assertions cannot strand a worker.
            drop(generation);
            assert!(owner.join().unwrap().is_err());
            waiter.join().unwrap();
            assert!(owner_waiting, "the owner must wait for the decoder before cleanup");
            assert!(entered_wait, "the second request must enter the same-hash wait before cleanup");
            assert_eq!(waiter_cancelled.ok(), Some(true), "close must release same-hash waiters before the decoder becomes available");
        });
        assert!(!pending.path(&hash).exists());
    }
    fs::remove_dir_all(&cache.directory).unwrap();
    assert!(cache.publish(&hash, &encode(&source).unwrap()).is_err());
    assert!(!cache.directory.exists());
    let warm = ThumbnailCache::new(root.path().join("warm-cache"));
    let response = warm.response(root.path(), request(&hash, source.len() as u64, Method::GET, None)).unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let validator = response.headers()[header::ETAG].to_str().unwrap().to_owned();
    drop(response);
    warm.close();
    for method in [Method::GET, Method::HEAD] {
        assert_eq!(warm.response(root.path(), request(&hash, source.len() as u64, method, Some(&validator))).unwrap().status(), StatusCode::NOT_FOUND);
    }
}

#[test]
fn remote_source_uses_existing_transient_reader_and_keeps_cas_empty() {
    let root = TempDir::new().unwrap();
    let _store = crate::persistent_store::PersistentStore::open(root.path()).unwrap();
    let source = image_bytes(32, 16, 1);
    let hash = hex::encode(Sha256::digest(&source));
    crate::server_sync::residency::test_remote::hold(root.path(), &[(&hash, source.len() as u64)]);
    crate::server_sync::residency::test_remote::serve(root.path(), &hash, source.clone());
    let cache = ThumbnailCache::new(root.path().join("cache"));
    let response = cache.response(root.path(), request(&hash, source.len() as u64, Method::GET, None)).unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let validator = response.headers()[header::ETAG].to_str().unwrap().to_owned();
    let mut preview = Vec::new();
    response.into_body().unwrap().read_to_end(&mut preview).unwrap();
    assert_eq!(image::load_from_memory(&preview).unwrap().width(), 32);
    assert!(PayloadCas::new(root.path()).unwrap().object_lengths().unwrap().is_empty());
    assert_eq!(fs::read_dir(&cache.directory).unwrap().count(), 1);
    let fetched = crate::server_sync::residency::test_remote::fetched(root.path());
    for method in [Method::GET, Method::HEAD] {
        assert_eq!(cache.response(root.path(), request(&hash, source.len() as u64, method.clone(), None)).unwrap().status(), StatusCode::OK);
        assert_eq!(cache.response(root.path(), request(&hash, source.len() as u64, method, Some(&validator))).unwrap().status(), StatusCode::NOT_MODIFIED);
    }
    assert_eq!(crate::server_sync::residency::test_remote::fetched(root.path()), fetched);
    let residency = crate::server_sync::residency::Residency::open(root.path()).unwrap();
    let object = residency.object(&hash, None).unwrap().unwrap();
    assert!(residency.begin_release(&object).unwrap());
    assert_eq!(cache.response(root.path(), request(&hash, source.len() as u64, Method::GET, Some(&validator))).unwrap().status(), StatusCode::NOT_FOUND);
    residency.finish_release(&object).unwrap();
    assert_eq!(cache.response(root.path(), request(&hash, source.len() as u64, Method::HEAD, Some(&validator))).unwrap().status(), StatusCode::NOT_FOUND);
}

#[test]
fn remote_spooling_progresses_while_another_request_owns_the_decoder() {
    use crate::server_sync::residency::test_remote;
    let root = TempDir::new().unwrap();
    let _store = crate::persistent_store::PersistentStore::open(root.path()).unwrap();
    let sources = [1, 2].map(|seed| {
        let source = image_bytes(32, 16, seed);
        let hash = hex::encode(Sha256::digest(&source));
        let size = source.len() as u64;
        test_remote::serve(root.path(), &hash, source);
        (hash, size)
    });
    let proofs = sources.iter().map(|(hash, size)| (hash.as_str(), *size)).collect::<Vec<_>>();
    test_remote::hold(root.path(), &proofs);
    let cache = ThumbnailCache::new(root.path().join("cache"));
    cache.cached(&sources[0].0).unwrap();
    let generation = cache.generation.lock().unwrap();
    std::thread::scope(|scope| {
        let waiting = sources.iter().map(|(hash, size)| {
            let cache = &cache; let root = root.path();
            scope.spawn(move || cache.get(root, hash, *size))
        }).collect::<Vec<_>>();
        let started = std::time::Instant::now();
        while cache.decoder_waiting.load(Ordering::Acquire) < 2 && started.elapsed() < Duration::from_secs(5) {
            std::thread::yield_now();
        }
        let fetched_while_decoder_busy = cache.decoder_waiting.load(Ordering::Acquire) == 2;
        drop(generation);
        for thread in waiting {
            let preview = read(thread.join().unwrap().unwrap());
            assert_eq!(image::load_from_memory(&preview).unwrap().width(), 32);
        }
        assert!(fetched_while_decoder_busy, "different hashes must finish spooling independently of the decoder");
        assert_eq!(test_remote::fetched(root.path()), 2);
    });
}

#[test]
fn external_storage_preview_does_not_hydrate_an_original_into_the_library() {
    use crate::external_storage::{lww_tests::{CycleFixture, small_asset}, lww_residency, previous_storage_tests};
    let mut fixture = CycleFixture::new();
    let source = image_bytes(32, 16, 1);
    let hash = small_asset(&mut fixture.a, "gallery-external", &source);
    tauri::async_runtime::block_on(fixture.publish_a());
    tauri::async_runtime::block_on(fixture.receive_b());
    let _connection = lww_residency::install_test_source_connection(fixture.directory_b.path(),
        Arc::new(previous_storage_tests::receiver_connection(&fixture))).unwrap();
    let cas = PayloadCas::new(fixture.directory_b.path()).unwrap();
    assert!(cas.stat_object(&hash).unwrap().is_none());
    let cache = ThumbnailCache::new(fixture.directory_b.path().join("cache"));
    assert_eq!(cache.response(fixture.directory_b.path(), request(&hash, source.len() as u64, Method::GET, None)).unwrap().status(), StatusCode::NOT_FOUND);
    let preview = read(cache.get(fixture.directory_b.path(), &hash, source.len() as u64).unwrap());
    assert_eq!(image::load_from_memory(&preview).unwrap().width(), 32);
    assert!(cas.stat_object(&hash).unwrap().is_none());
    assert_eq!(fs::read_dir(&cache.directory).unwrap().count(), 1);
    assert_eq!(cache.response(fixture.directory_b.path(), request(&hash, source.len() as u64, Method::GET, None)).unwrap().status(), StatusCode::NOT_FOUND);
}
