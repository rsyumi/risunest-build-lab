use super::{
    default_animation_decode_bytes, decode_physical_key, encode_inlay_image, respond,
    InlayEncodeFormat,
    InlayEncodeOptions,
};
use image::{
    codecs::gif::{GifEncoder, Repeat},
    codecs::webp::WebPDecoder,
    imageops::FilterType,
    AnimationDecoder, Delay, DynamicImage, Frame, ImageDecoder, ImageFormat, ImageReader, Rgba, RgbaImage,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, io::Cursor, path::Path};
use tauri::http::{header, Method, Request, StatusCode};
use tempfile::TempDir;

fn hex(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn request(method: Method, physical_key: &str) -> Request<Vec<u8>> {
    Request::builder()
        .method(method)
        .uri(format!("http://risuasset.localhost/{}", hex(physical_key)))
        .body(Vec::new())
        .unwrap()
}

fn cas_request(method: Method, physical_key: &str, mime: &str, size: u64) -> Request<Vec<u8>> {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("mime", mime)
        .append_pair("size", &size.to_string())
        .finish();
    Request::builder()
        .method(method)
        .uri(format!(
            "http://risuasset.localhost/{}?{}",
            hex(physical_key),
            query
        ))
        .body(Vec::new())
        .unwrap()
}

fn write_cas_object(root: &Path, bytes: &[u8]) -> (String, String) {
    let hash = sha256_hex(bytes);
    let physical_key = format!("assets/objects/{}/{}", &hash[..2], &hash[2..]);
    let payload = root.join(physical_key.replace('/', std::path::MAIN_SEPARATOR_STR));
    fs::create_dir_all(payload.parent().unwrap()).unwrap();
    fs::write(payload, bytes).unwrap();
    (hash, physical_key)
}


#[test]
fn maps_only_exact_lowercase_cas_object_paths() {
    let hash = "ab".repeat(32);
    let physical_key = format!("assets/objects/ab/{}", &hash[2..]);
    assert_eq!(
        decode_physical_key(&format!(
            "http://risuasset.localhost/{}?mime=image%2Fpng&size=1",
            hex(&physical_key)
        )),
        Some(physical_key)
    );

    for invalid in [
        format!("assets/objects/a/{}", &hash[2..]),
        format!("assets/objects/AB/{}", &hash[2..]),
        format!("assets/objects/ab/{}", &hash[3..]),
        format!("assets/objects/ab/{}/extra", &hash[2..]),
    ] {
        assert_eq!(
            decode_physical_key(&format!("http://risuasset.localhost/{}", hex(&invalid))),
            None,
            "accepted {invalid}"
        );
    }
}

#[test]
fn serves_cas_get_head_and_range_with_alias_descriptor_and_strong_hash_etag() {
    let temp = TempDir::new().unwrap();
    let bytes = b"0123456789";
    let (hash, physical_key) = write_cas_object(temp.path(), bytes);

    let get = respond(
        temp.path(),
        cas_request(
            Method::GET,
            &physical_key,
            "application/x-exact",
            bytes.len() as u64,
        ),
    );
    assert_eq!(get.status(), StatusCode::OK);
    assert_eq!(get.body(), bytes);
    assert_eq!(get.headers()[header::CONTENT_TYPE], "application/x-exact");
    assert_eq!(
        get.headers()[header::ETAG].to_str().unwrap(),
        format!("\"{hash}\"")
    );

    let head = respond(
        temp.path(),
        cas_request(
            Method::HEAD,
            &physical_key,
            "application/x-exact",
            bytes.len() as u64,
        ),
    );
    assert_eq!(head.status(), StatusCode::OK);
    assert!(head.body().is_empty());
    assert_eq!(head.headers()[header::CONTENT_LENGTH], "10");
    assert_eq!(
        head.headers()[header::ETAG].to_str().unwrap(),
        format!("\"{hash}\"")
    );

    let mut ranged = cas_request(
        Method::GET,
        &physical_key,
        "application/x-exact",
        bytes.len() as u64,
    );
    ranged
        .headers_mut()
        .insert(header::RANGE, "bytes=3-6".parse().unwrap());
    let ranged = respond(temp.path(), ranged);
    assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(ranged.body(), b"3456");
    assert_eq!(ranged.headers()[header::CONTENT_RANGE], "bytes 3-6/10");

    let mut conditional = cas_request(
        Method::GET,
        &physical_key,
        "application/x-exact",
        bytes.len() as u64,
    );
    conditional.headers_mut().insert(
        header::IF_NONE_MATCH,
        format!("\"{hash}\"").parse().unwrap(),
    );
    assert_eq!(
        respond(temp.path(), conditional).status(),
        StatusCode::NOT_MODIFIED
    );
}

#[test]
fn rejects_missing_or_corrupt_cas_descriptors_and_preserves_zero_byte_behavior() {
    let temp = TempDir::new().unwrap();
    let (_, physical_key) = write_cas_object(temp.path(), b"abc");

    let missing_hash = "cd".repeat(32);
    let missing_key = format!("assets/objects/cd/{}", &missing_hash[2..]);
    assert_eq!(
        respond(
            temp.path(),
            cas_request(Method::GET, &missing_key, "application/octet-stream", 0),
        )
        .status(),
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        respond(temp.path(), request(Method::GET, &physical_key)).status(),
        StatusCode::NOT_FOUND
    );

    let duplicate_mime = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "http://risuasset.localhost/{}?mime=text%2Fplain&mime=text%2Fhtml&size=3",
            hex(&physical_key)
        ))
        .body(Vec::new())
        .unwrap();
    assert_eq!(
        respond(temp.path(), duplicate_mime).status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        respond(
            temp.path(),
            cas_request(Method::GET, &physical_key, "application/octet-stream", 4),
        )
        .status(),
        StatusCode::NOT_FOUND
    );

    let (_, empty_key) = write_cas_object(temp.path(), b"");
    let empty = respond(
        temp.path(),
        cas_request(Method::GET, &empty_key, "application/octet-stream", 0),
    );
    assert_eq!(empty.status(), StatusCode::OK);
    assert!(empty.body().is_empty());
    assert_eq!(empty.headers()[header::CONTENT_LENGTH], "0");
}





fn encoded_fixture(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(width, height, |x, y| {
        Rgba([(x * 17) as u8, (y * 29) as u8, 91, 255])
    }));
    let mut output = Cursor::new(Vec::new());
    image.write_to(&mut output, format).unwrap();
    output.into_inner()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut value = u32::MAX;
    for byte in bytes {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            value = (value >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(value & 1)));
        }
    }
    !value
}

/// Noise, so that neither the source nor the re-encoded animation compresses
/// away to nothing and hides what the encoder actually did.
fn animation_frame(width: u32, height: u32, seed: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        let mixed = x
            .wrapping_mul(2_654_435_761)
            .wrapping_add(y.wrapping_mul(40_503))
            .wrapping_add(seed.wrapping_mul(2_246_822_519));
        let mixed = mixed ^ (mixed >> 15);
        Rgba([
            (mixed >> 3) as u8,
            (mixed >> 11) as u8,
            (mixed >> 19) as u8,
            255,
        ])
    })
}

fn animated_gif_fixture(delays_ms: &[u32], width: u32, height: u32) -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut encoder = GifEncoder::new(Cursor::new(&mut output));
        encoder.set_repeat(Repeat::Infinite).unwrap();
        for (index, delay) in delays_ms.iter().enumerate() {
            encoder
                .encode_frame(Frame::from_parts(
                    animation_frame(width, height, index as u32),
                    0,
                    0,
                    Delay::from_numer_denom_ms(*delay, 1),
                ))
                .unwrap();
        }
    }
    output
}

fn animated_webp_fixture(delays_ms: &[u32], width: u32, height: u32) -> Vec<u8> {
    let frames: Vec<RgbaImage> = (0..delays_ms.len())
        .map(|index| animation_frame(width, height, index as u32))
        .collect();
    let mut config = webp::WebPConfig::new().unwrap();
    config.lossless = 1;
    let mut encoder = webp::AnimEncoder::new(width, height, &config);
    encoder.set_loop_count(0);
    let mut timestamp = 0i32;
    for (frame, delay) in frames.iter().zip(delays_ms) {
        encoder.add_frame(webp::AnimFrame::from_rgba(
            frame.as_raw(),
            width,
            height,
            timestamp,
        ));
        timestamp += *delay as i32;
    }
    let mut encoded = encoder.try_encode().unwrap().to_vec();
    set_webp_frame_delays(&mut encoded, delays_ms);
    encoded
}

fn png_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(payload.len() + 12);
    chunk.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(payload);
    let crc = crc32(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    chunk
}

fn png_chunks(png: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut chunks = Vec::new();
    let mut offset = 8usize;
    while offset + 12 <= png.len() {
        let length = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
        let kind = String::from_utf8_lossy(&png[offset + 4..offset + 8]).into_owned();
        chunks.push((kind, png[offset + 8..offset + 8 + length].to_vec()));
        offset += 12 + length;
    }
    chunks
}

fn apng_frame_control(sequence: u32, width: u32, height: u32, delay_ms: u16) -> Vec<u8> {
    let mut payload = Vec::with_capacity(26);
    payload.extend_from_slice(&sequence.to_be_bytes());
    payload.extend_from_slice(&width.to_be_bytes());
    payload.extend_from_slice(&height.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&delay_ms.to_be_bytes());
    payload.extend_from_slice(&1000u16.to_be_bytes());
    payload.push(0);
    payload.push(0);
    payload
}

/// Every frame repeats the default image, which is enough to check frame timing.
fn animated_apng_fixture(delays_ms: &[u16], width: u32, height: u32) -> Vec<u8> {
    let frame_png = |seed: u32| {
        let mut buffer = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(animation_frame(width, height, seed))
            .write_to(&mut buffer, ImageFormat::Png)
            .unwrap();
        buffer.into_inner()
    };
    let frame_data = |seed: u32| -> Vec<u8> {
        png_chunks(&frame_png(seed))
            .iter()
            .filter(|(kind, _)| kind == "IDAT")
            .flat_map(|(_, payload)| payload.clone())
            .collect()
    };
    let png = frame_png(0);
    let chunks = png_chunks(&png);
    let image_data = frame_data(0);
    let mut output = png[..8].to_vec();
    for (kind, payload) in &chunks {
        if kind == "IDAT" || kind == "IEND" {
            continue;
        }
        output.extend_from_slice(&png_chunk(kind.as_bytes().try_into().unwrap(), payload));
        if kind == "IHDR" {
            let mut control = Vec::new();
            control.extend_from_slice(&(delays_ms.len() as u32).to_be_bytes());
            control.extend_from_slice(&0u32.to_be_bytes());
            output.extend_from_slice(&png_chunk(b"acTL", &control));
        }
    }
    let mut sequence = 0u32;
    output.extend_from_slice(&png_chunk(
        b"fcTL",
        &apng_frame_control(sequence, width, height, delays_ms[0]),
    ));
    sequence += 1;
    output.extend_from_slice(&png_chunk(b"IDAT", &image_data));
    for (index, delay) in delays_ms[1..].iter().enumerate() {
        output.extend_from_slice(&png_chunk(
            b"fcTL",
            &apng_frame_control(sequence, width, height, *delay),
        ));
        sequence += 1;
        let mut payload = sequence.to_be_bytes().to_vec();
        sequence += 1;
        payload.extend_from_slice(&frame_data(index as u32 + 1));
        output.extend_from_slice(&png_chunk(b"fdAT", &payload));
    }
    output.extend_from_slice(&png_chunk(b"IEND", &[]));
    output
}

fn set_webp_frame_delays(data: &mut [u8], delays: &[u32]) {
    let mut written = 0usize;
    let mut offset = 12usize;
    while offset + 8 <= data.len() {
        let size = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let payload = offset + 8;
        if &data[offset..offset + 4] == b"ANMF" {
            let duration = delays[written].to_le_bytes();
            data[payload + 12..payload + 15].copy_from_slice(&duration[..3]);
            written += 1;
        }
        offset = payload + size + (size & 1);
    }
    assert_eq!(written, delays.len(), "unexpected animation frame count");
}

fn webp_frame_delays(data: &[u8]) -> Vec<u32> {
    WebPDecoder::new(Cursor::new(data))
        .unwrap()
        .into_frames()
        .collect_frames()
        .unwrap()
        .iter()
        .map(|frame| {
            let (numerator, denominator) = frame.delay().numer_denom_ms();
            numerator / denominator
        })
        .collect()
}

fn animation_options(max_dimension: u32, animation_max_fps: u32) -> InlayEncodeOptions {
    InlayEncodeOptions {
        format: InlayEncodeFormat::Webp,
        quality: 85,
        max_dimension,
        skip_reencode: true,
        animation_max_fps,
        animation_decode_bytes: default_animation_decode_bytes(),
    }
}

#[test]
fn stores_an_animated_gif_as_an_animated_webp_with_the_delays_browsers_play() {
    let source = animated_gif_fixture(&[10, 30, 50], 64, 64);

    let result =
        encode_inlay_image("gif", &source, "loop.gif", Some(animation_options(0, 0))).unwrap();

    assert_eq!(result.metadata.mime, "image/webp");
    assert_eq!(result.metadata.ext, "webp");
    assert_eq!(
        (result.metadata.width, result.metadata.height),
        (Some(64), Some(64))
    );
    // A GIF frame at or under 10 ms plays as 100 ms in browsers; longer delays stay.
    assert_eq!(webp_frame_delays(&result.data), vec![100, 30, 50]);
}

#[test]
fn keeps_animated_webp_delays_and_folds_a_zero_delay_frame_into_the_one_before_it() {
    let mut source = animated_webp_fixture(&[30, 30, 40], 64, 64);
    set_webp_frame_delays(&mut source, &[30, 0, 40]);
    assert_eq!(webp_frame_delays(&source), vec![30, 0, 40]);

    let result =
        encode_inlay_image("webp", &source, "loop.webp", Some(animation_options(0, 0))).unwrap();

    // Milliseconds are meant literally in WebP, so only the unplayable frame goes.
    assert_eq!(webp_frame_delays(&result.data), vec![30, 40]);
}

#[test]
fn replaces_a_zero_delay_apng_frame_with_a_tenth_of_a_second() {
    let source = animated_apng_fixture(&[0, 40], 64, 64);

    let result =
        encode_inlay_image("apng", &source, "loop.png", Some(animation_options(0, 0))).unwrap();

    assert_eq!(result.metadata.ext, "webp");
    assert_eq!(webp_frame_delays(&result.data), vec![100, 40]);
}

#[test]
fn caps_the_frame_rate_without_changing_how_long_the_animation_runs() {
    let source = animated_gif_fixture(&[30, 30, 30, 30], 64, 64);

    let capped =
        encode_inlay_image("fps", &source, "loop.gif", Some(animation_options(0, 15))).unwrap();

    let delays = webp_frame_delays(&capped.data);
    assert_eq!(delays, vec![90, 30]);
    assert_eq!(delays.iter().sum::<u32>(), 120);
}

#[test]
fn scales_every_animation_frame_to_the_maximum_resolution() {
    let source = animated_gif_fixture(&[30, 30, 30], 64, 32);

    let result = encode_inlay_image(
        "scaled",
        &source,
        "loop.gif",
        Some(animation_options(16, 0)),
    )
    .unwrap();

    assert_eq!(
        (result.metadata.width, result.metadata.height),
        (Some(16), Some(8))
    );
    let frames = WebPDecoder::new(Cursor::new(&result.data))
        .unwrap()
        .into_frames()
        .collect_frames()
        .unwrap();
    assert_eq!(frames.len(), 3);
    for frame in &frames {
        assert_eq!(
            (frame.buffer().width(), frame.buffer().height()),
            (16u32, 8u32)
        );
    }
}

#[test]
fn preserves_an_animation_with_more_frames_than_the_limit_allows() {
    let delays = vec![20u32; 601];
    let source = animated_gif_fixture(&delays, 8, 8);

    let result =
        encode_inlay_image("many", &source, "long.gif", Some(animation_options(0, 0))).unwrap();

    assert_eq!(result.data, source);
    assert_eq!(result.metadata.mime, "image/gif");
    assert_eq!(result.metadata.ext, "gif");
    assert_eq!(
        (result.metadata.width, result.metadata.height),
        (None, None)
    );
}

#[test]
fn preserves_an_animation_the_settings_ask_to_keep_as_it_is() {
    let source = animated_gif_fixture(&[30, 30, 30], 64, 64);

    let result = encode_inlay_image(
        "original",
        &source,
        "loop.gif",
        Some(InlayEncodeOptions {
            format: InlayEncodeFormat::Original,
            quality: 85,
            max_dimension: 0,
            skip_reencode: true,
            animation_max_fps: 0,
            animation_decode_bytes: default_animation_decode_bytes(),
        }),
    )
    .unwrap();

    assert_eq!(result.data, source);
    assert_eq!(result.metadata.ext, "gif");
}

#[test]
fn preserves_an_animation_that_re_encoding_would_not_shrink() {
    let source = animated_gif_fixture(&[30, 30], 2, 1);

    let result =
        encode_inlay_image("tiny", &source, "tiny.gif", Some(animation_options(0, 0))).unwrap();

    assert_eq!(result.data, source);
    assert_eq!(result.metadata.mime, "image/gif");
}

#[test]
fn preserves_formats_and_broken_bytes_this_device_cannot_re_encode() {
    let avif = b"\0\0\0\x18ftypavif\0\0\0\0avifmif1";
    let broken = b"not an image";

    let stored_avif =
        encode_inlay_image("avif", avif, "source.avif", Some(animation_options(0, 0))).unwrap();
    let stored_broken = encode_inlay_image(
        "broken",
        broken,
        "broken.png",
        Some(animation_options(0, 0)),
    )
    .unwrap();

    assert_eq!(stored_avif.data, avif);
    assert_eq!(stored_avif.metadata.mime, "image/avif");
    assert_eq!(stored_avif.metadata.ext, "avif");
    assert_eq!(stored_broken.data, broken);
    // Nothing in the bytes says what this is, so the file name decides.
    assert_eq!(stored_broken.metadata.mime, "image/png");
    assert_eq!(stored_broken.metadata.ext, "png");
    assert_eq!(
        (stored_broken.metadata.width, stored_broken.metadata.height),
        (None, None)
    );
}



#[test]
fn configurable_inlay_encoding_preserves_original_and_skipped_webp_bytes() {
    let png = encoded_fixture(ImageFormat::Png, 8, 4);
    let webp = encoded_fixture(ImageFormat::WebP, 8, 4);
    for (source, format, skip, expected_mime, expected_ext, expected_bytes) in [
        (
            &png,
            InlayEncodeFormat::Original,
            false,
            "image/png",
            "png",
            true,
        ),
        (
            &webp,
            InlayEncodeFormat::Original,
            false,
            "image/webp",
            "webp",
            true,
        ),
        (
            &webp,
            InlayEncodeFormat::Webp,
            true,
            "image/webp",
            "webp",
            true,
        ),
    ] {
        let result = encode_inlay_image(
            "matrix",
            source,
            "source",
            Some(InlayEncodeOptions {
                format,
                quality: 12,
                max_dimension: 0,
                skip_reencode: skip,
                animation_max_fps: 0,
                animation_decode_bytes: default_animation_decode_bytes(),
            }),
        )
        .unwrap();
        assert_eq!(result.metadata.mime, expected_mime);
        assert_eq!(result.metadata.ext, expected_ext);
        assert_eq!(result.data.as_slice() == source.as_slice(), expected_bytes);
    }
    let png_result = encode_inlay_image(
        "png",
        &png,
        "source",
        Some(InlayEncodeOptions {
            format: InlayEncodeFormat::Png,
            quality: 12,
            max_dimension: 0,
            skip_reencode: false,
            animation_max_fps: 0,
            animation_decode_bytes: default_animation_decode_bytes(),
        }),
    )
    .unwrap();
    assert_eq!(&png_result.data[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(png_result.metadata.mime, "image/png");
}

#[test]
fn configurable_inlay_encoding_resizes_before_webp_encoding() {
    let source = encoded_fixture(ImageFormat::Png, 20, 10);
    let result = encode_inlay_image(
        "resize",
        &source,
        "source",
        Some(InlayEncodeOptions {
            format: InlayEncodeFormat::Webp,
            quality: 70,
            max_dimension: 5,
            skip_reencode: false,
            animation_max_fps: 0,
            animation_decode_bytes: default_animation_decode_bytes(),
        }),
    )
    .unwrap();
    assert_eq!(
        (result.metadata.width, result.metadata.height),
        (Some(5), Some(3))
    );
    assert_eq!(result.metadata.mime, "image/webp");
}

#[test]
fn configured_webp_quality_is_forwarded_instead_of_using_the_default() {
    let source = encoded_fixture(ImageFormat::Png, 32, 24);
    let rgba = image::load_from_memory_with_format(&source, ImageFormat::Png)
        .unwrap()
        .to_rgba8();
    let configured = encode_inlay_image(
        "quality-70",
        &source,
        "source.png",
        Some(InlayEncodeOptions {
            format: InlayEncodeFormat::Webp,
            quality: 70,
            max_dimension: 0,
            skip_reencode: false,
            animation_max_fps: 0,
            animation_decode_bytes: default_animation_decode_bytes(),
        }),
    )
    .unwrap();
    let defaulted = encode_inlay_image("quality-85", &source, "source.png", None).unwrap();

    for (encoded, quality) in [(&configured.data, 70.0), (&defaulted.data, 85.0)] {
        let expected = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height())
            .encode(quality)
            .to_vec();
        assert_eq!(encoded.as_slice(), expected.as_slice());
        let decoded = image::load_from_memory_with_format(encoded, ImageFormat::WebP).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (32, 24));
    }
}

#[test]
fn configured_png_resizes_losslessly_with_truthful_dimensions() {
    let source = encoded_fixture(ImageFormat::Jpeg, 20, 10);
    let result = encode_inlay_image(
        "png-resize",
        &source,
        "source.jpg",
        Some(InlayEncodeOptions {
            format: InlayEncodeFormat::Png,
            quality: 1,
            max_dimension: 5,
            skip_reencode: true,
            animation_max_fps: 0,
            animation_decode_bytes: default_animation_decode_bytes(),
        }),
    )
    .unwrap();

    assert_eq!(&result.data[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        (result.metadata.width, result.metadata.height),
        (Some(5), Some(3))
    );
    let decoded = image::load_from_memory_with_format(&result.data, ImageFormat::Png)
        .unwrap()
        .to_rgba8();
    assert_eq!((decoded.width(), decoded.height()), (5, 3));
    let expected = image::load_from_memory_with_format(&source, ImageFormat::Jpeg)
        .unwrap()
        .resize(5, 3, FilterType::Lanczos3)
        .to_rgba8();
    assert_eq!(decoded, expected);
}

#[test]
fn native_options_clamp_max_dimension_before_original_mode_ignores_it() {
    for (input, expected) in [
        (json!(-1), 0),
        (json!(12.6), 13),
        (json!(4_294_967_296_u64), u32::MAX),
        (json!(u64::MAX), u32::MAX),
    ] {
        let options: InlayEncodeOptions = serde_json::from_value(json!({
            "format": "original",
            "quality": 85,
            "maxDimension": input,
            "skipReencode": false,
            "animationMaxFps": 0,
        }))
        .unwrap();
        assert_eq!(options.max_dimension, expected);

        let source = encoded_fixture(ImageFormat::Png, 8, 4);
        let result = encode_inlay_image("original", &source, "source.png", Some(options)).unwrap();
        assert_eq!(result.data, source);
    }
}

#[test]
fn native_options_clamp_webp_quality_to_the_encoder_range() {
    for (input, expected) in [(0, 1), (1, 1), (70, 70), (100, 100), (101, 100), (255, 100)] {
        let options: InlayEncodeOptions = serde_json::from_value(json!({
            "format": "webp",
            "quality": input,
            "maxDimension": 0,
            "skipReencode": false,
            "animationMaxFps": 0,
        }))
        .unwrap();

        assert_eq!(options.quality, expected);
    }
}

#[test]
fn native_options_clamp_the_animation_frame_rate() {
    for (input, expected) in [(0u32, 0u32), (12, 12), (240, 240), (1000, 240)] {
        let options: InlayEncodeOptions = serde_json::from_value(json!({
            "format": "webp",
            "quality": 85,
            "maxDimension": 0,
            "skipReencode": false,
            "animationMaxFps": input,
        }))
        .unwrap();

        assert_eq!(options.animation_max_fps, expected);
    }
}

#[test]
fn configured_png_converts_jpeg_and_webp_sources() {
    for (name, source) in [
        ("jpeg", encoded_fixture(ImageFormat::Jpeg, 9, 4)),
        ("webp", encoded_fixture(ImageFormat::WebP, 5, 8)),
    ] {
        let result = encode_inlay_image(
            name,
            &source,
            "source",
            Some(InlayEncodeOptions {
                format: InlayEncodeFormat::Png,
                quality: 1,
                max_dimension: 0,
                skip_reencode: false,
                animation_max_fps: 0,
                animation_decode_bytes: default_animation_decode_bytes(),
            }),
        )
        .unwrap();
        assert_eq!(&result.data[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(result.metadata.mime, "image/png");
        assert_eq!(result.metadata.ext, "png");
    }
}

#[test]
fn skipped_webp_reencodes_when_max_dimension_requires_resize() {
    let source = encoded_fixture(ImageFormat::WebP, 20, 10);
    let result = encode_inlay_image(
        "resized-skip",
        &source,
        "source.webp",
        Some(InlayEncodeOptions {
            format: InlayEncodeFormat::Webp,
            quality: 70,
            max_dimension: 5,
            skip_reencode: true,
            animation_max_fps: 0,
            animation_decode_bytes: default_animation_decode_bytes(),
        }),
    )
    .unwrap();
    assert_ne!(result.data, source);
    assert_eq!(
        (result.metadata.width, result.metadata.height),
        (Some(5), Some(3))
    );
    assert_eq!(result.metadata.mime, "image/webp");
}

#[test]
fn original_mode_keeps_header_dimensions_without_decoding_png_pixels() {
    let valid = encoded_fixture(ImageFormat::Png, 8, 3);
    let mut source = valid[..8].to_vec();
    for (kind, payload) in png_chunks(&valid) {
        let kind: &[u8; 4] = kind.as_bytes().try_into().unwrap();
        source.extend(png_chunk(kind, if kind == b"IDAT" { b"invalid pixels" } else { &payload }));
    }
    let decoder = ImageReader::new(Cursor::new(&source)).with_guessed_format().unwrap().into_decoder().unwrap();
    assert_eq!(decoder.dimensions(), (8, 3));
    assert!(DynamicImage::from_decoder(decoder).is_err());
    let result = encode_inlay_image("original", &source, "image.png", Some(InlayEncodeOptions {
        format: InlayEncodeFormat::Original, ..InlayEncodeOptions::default()
    })).unwrap();
    assert_eq!(result.data, source);
    assert_eq!((result.metadata.width, result.metadata.height), (Some(8), Some(3)));
}

#[test]
fn metadata_only_original_dimensions_follow_every_exif_orientation() {
    for orientation in 1..=8 {
        let source = with_exif_orientation(encoded_fixture(ImageFormat::Jpeg, 8, 3), orientation);
        let result = encode_inlay_image("original", &source, "image.jpg", Some(InlayEncodeOptions {
            format: InlayEncodeFormat::Original, ..InlayEncodeOptions::default()
        })).unwrap();
        let expected = if orientation >= 5 { (Some(3), Some(8)) } else { (Some(8), Some(3)) };
        assert_eq!((result.metadata.width, result.metadata.height), expected);
        assert_eq!(result.data, source);
    }
}

#[test]
fn original_inlay_ignores_max_dimension_and_preserves_oriented_jpeg_bytes() {
    let source = with_exif_orientation(encoded_fixture(ImageFormat::Jpeg, 8, 3), 6);
    let result = encode_inlay_image(
        "original-jpeg",
        &source,
        "source.jpg",
        Some(InlayEncodeOptions {
            format: InlayEncodeFormat::Original,
            quality: 1,
            max_dimension: 1,
            skip_reencode: false,
            animation_max_fps: 0,
            animation_decode_bytes: default_animation_decode_bytes(),
        }),
    )
    .unwrap();
    assert_eq!(result.data, source);
    assert_eq!(
        (result.metadata.width, result.metadata.height),
        (Some(3), Some(8))
    );
    assert_eq!(result.metadata.mime, "image/jpeg");
    assert_eq!(result.metadata.ext, "jpg");
}


fn with_exif_orientation(jpeg: Vec<u8>, orientation: u8) -> Vec<u8> {
    assert!(jpeg.starts_with(&[0xff, 0xd8]));
    let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
    exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
    let length = (exif.len() + 2) as u16;
    let mut oriented = Vec::with_capacity(jpeg.len() + exif.len() + 4);
    oriented.extend_from_slice(&jpeg[..2]);
    oriented.extend_from_slice(&[0xff, 0xe1]);
    oriented.extend_from_slice(&length.to_be_bytes());
    oriented.extend_from_slice(&exif);
    oriented.extend_from_slice(&jpeg[2..]);
    oriented
}







#[test]
fn animation_preflight_preserves_exact_source_under_small_budget() {
    for (source, name) in [
        (animated_gif_fixture(&[30, 40], 64, 64), "loop.gif"),
        (animated_apng_fixture(&[30, 40], 64, 64), "loop.png"),
        (animated_webp_fixture(&[30, 40], 64, 64), "loop.webp"),
    ] {
        let mut options = animation_options(1, 1);
        options.animation_decode_bytes = 64 * 64 * 4 * 4 - 1;
        let result = encode_inlay_image("budget", &source, name, Some(options)).unwrap();
        assert_eq!(result.data, source);
        assert_eq!(result.metadata.preservation_reason.as_deref(), Some("animation-cost"));
        let format = image::guess_format(&source).unwrap();
        let kind = super::inlay_animation(format, &source).unwrap();
        assert!(super::animation_policy::permits_decode(&source, kind, 64 * 64 * 4 * 4));
        assert!(!super::animation_policy::permits_decode(&source[..source.len() - 1], kind, u64::MAX));
    }
}

#[test]
fn huge_gif_canvas_is_preserved_without_decoding_pixels() {
    let mut source = animated_gif_fixture(&[30, 40], 2, 2);
    source[6..10].copy_from_slice(&[255, 255, 255, 255]);
    let result = encode_inlay_image("huge", &source, "huge.gif", None).unwrap();
    assert_eq!(result.data, source);
    assert_eq!(result.metadata.preservation_reason.as_deref(), Some("animation-cost"));
}
