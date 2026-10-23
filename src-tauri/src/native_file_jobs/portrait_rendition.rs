use super::{download_handoff, error::{invalid_input, io_error}, open_regular_file_no_follow, owned_handoff_file, NativeJobError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PortraitRenditionDescriptor {
    pub source_hash: String,
    pub path: String,
}

#[derive(Debug)]
pub(crate) struct PortraitRendition {
    pub source_hash: String,
    pub png: Vec<u8>,
}

impl PortraitRenditionDescriptor {
    pub(crate) fn read(self, root: &Path) -> Result<PortraitRendition, NativeJobError> {
        if self.source_hash.len() != 64 || !self.source_hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return Err(invalid_input("portrait rendition source hash is invalid"));
        }
        let path = Path::new(&self.path);
        if !owned_handoff_file(root, path, download_handoff::PREFIX, download_handoff::SUFFIX, "portrait rendition", "invalid-input")? {
            return Err(invalid_input("portrait rendition is unavailable"));
        }
        let opened = open_regular_file_no_follow(path)?;
        const MAX_BYTES: u64 = 50 * 1024 * 1024;
        if opened.total_bytes > MAX_BYTES {
            return Err(NativeJobError::too_large("card", "portrait rendition exceeds the export limit"));
        }
        let mut png = Vec::new();
        opened.file.take(MAX_BYTES + 1).read_to_end(&mut png).map_err(io_error)?;
        if png.len() as u64 != opened.total_bytes || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(invalid_input("portrait rendition is not a complete PNG"));
        }
        Ok(PortraitRendition { source_hash: self.source_hash, png })
    }
}

pub(crate) fn apply(original: Vec<u8>, rendition: Option<&PortraitRendition>) -> Result<Vec<u8>, NativeJobError> {
    let Some(rendition) = rendition else { return Ok(original) };
    if hex::encode(Sha256::digest(&original)) != rendition.source_hash {
        return Err(invalid_input("portrait rendition does not match the pinned portrait"));
    }
    if matches!(image::guess_format(&original), Ok(image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::Gif | image::ImageFormat::WebP)) {
        return Err(invalid_input("portrait does not need a WebView rendition"));
    }
    Ok(rendition.png.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_repository::PayloadCas;
    use crate::persistent_store::{AssetAlias, PersistentStore};
    use crate::native_file_jobs::{character_png_export, character_charx_export, CharacterCharxContainer, JobRegistry, JobKind};
    use serde_json::json;
    use std::fs;

    #[test]
    fn avif_renditions_export_cards_and_preserve_originals_and_existing_destinations() {
        let png = include_bytes!("../../../tests/fixtures/images/still.png");
        for (family, ext) in [("avif", "avif"), ("avif", "png"), ("gif", "gif"), ("webp", "webp")] {
            let original: &[u8] = match family {
                "gif" => include_bytes!("../../../tests/fixtures/images/animated.gif"),
                "webp" => include_bytes!("../../../tests/fixtures/images/animated.webp"),
                _ => include_bytes!("../../../tests/fixtures/images/animated.avif"),
            };
            for appended in [false, true] {
                for failure in ["", "hash", "cancel", "decode"] {
                    if family != "avif" && ["hash", "decode"].contains(&failure) { continue; }
                    let directory = tempfile::tempdir().unwrap();
                    let mut store = PersistentStore::open(directory.path()).unwrap();
                    let cas = PayloadCas::new(directory.path()).unwrap();
                    let payload = cas.prepare_bytes(original).unwrap();
                    let key = format!("assets/portrait.{ext}");
                    let staging = store.replace_begin().unwrap().staging_id;
                    store.replace_put_root(&staging, &json!({})).unwrap();
                    store.replace_put_presets(&staging, &[]).unwrap();
                    store.replace_add_characters(&staging, &[json!({
                        "type": "character", "chaId": "image", "name": "Image", "image": key,
                        "ccAssets": [], "additionalAssets": [], "emotionImages": [],
                        "triggerscript": [], "customscript": [], "chats": []
                    })]).unwrap();
                    store.replace_put_asset_aliases(&staging, &[AssetAlias {
                        kind: "asset".into(), key: key.clone(), object_hash: Some(payload.content_hash.clone()),
                        size: original.len() as i64, mime: format!("image/{ext}"), name: key.clone(), ext: ext.into(),
                        inlay_type: None, width: None, height: None, metadata: json!({}),
                    }]).unwrap();
                    let revision = store.replace_commit(&staging, Some(0)).unwrap().revision;
                    assert!(store.prepare_risu_save_export(revision + 1).is_err());
                    let prepared = store.prepare_risu_save_export(revision).unwrap();
                    let mut card = json!({"spec": "chara_card_v3", "spec_version": "3.0", "data": {
                        "name": "Image", "extensions": {"risuai": {}},
                        "assets": [{"type":"icon", "uri":"ccdefault:", "name":"main", "ext": "png"}]
                    }});
                    let rendition = PortraitRendition {
                        source_hash: if failure == "hash" { "0".repeat(64) } else { payload.content_hash.clone() },
                        png: if failure == "decode" { b"\x89PNG\r\n\x1a\n".to_vec() } else { png.to_vec() },
                    };
                    let owned = directory.path().join("owned");
                    fs::create_dir(&owned).unwrap();
                    let destination = directory.path().join(if appended { "card.jpeg" } else { "card.png" });
                    fs::write(&destination, b"previous destination").unwrap();
                    let job = JobRegistry::default().create(if appended { JobKind::ExportCharacterCharx } else { JobKind::ExportCharacterCard }).unwrap();
                    if failure == "cancel" { job.request_cancel().unwrap(); }
                    let result = if appended {
                        character_charx_export::export_character_charx_container(prepared, "image", card, json!({"trigger": [], "regex": [], "lorebook": []}), CharacterCharxContainer::AppendedCharxJpeg,
                            &owned, &directory.path().join("handoffs"), Some(&destination), &job, (family == "avif").then_some(&rendition))
                    } else {
                        card["data"]["extensions"]["risuai"] = json!({"triggerscript": [], "customScripts": []});
                        character_png_export::export_character_png(prepared, "image", card, &owned, &directory.path().join("handoffs"), Some(&destination), &job, (family == "avif").then_some(&rendition))
                    };
                    if !failure.is_empty() {
                        assert!(result.is_err(), "{ext} {appended} {failure}");
                        assert_eq!(fs::read(&destination).unwrap(), b"previous destination");
                    } else {
                        result.unwrap();
                        let bytes = fs::read(&destination).unwrap();
                        let decoded = image::load_from_memory(&bytes).unwrap();
                        assert_eq!((decoded.width(), decoded.height()), (4, 3));
                        let pixel = decoded.to_rgba8().get_pixel(0, 0).0;
                        assert!(pixel[0] > 200 && pixel[2] < 100, "first frame must be red: {pixel:?}");
                        if appended {
                            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
                            let mut portrait = Vec::new();
                            for index in 0..zip.len() {
                                let mut file = zip.by_index(index).unwrap();
                                if file.name().starts_with("assets/") { file.read_to_end(&mut portrait).unwrap(); }
                            }
                            assert_eq!(portrait, original);
                        } else {
                            let parse = directory.path().join("parse"); fs::create_dir(&parse).unwrap();
                            let parsed = crate::import_export_jobs::png_card::parse_png_card(&mut std::io::Cursor::new(&bytes), &parse, Default::default(), || false).unwrap();
                            assert!(parsed.ccv3.is_some());
                        }
                    }
                    assert_eq!(cas.read_object(&payload.content_hash).unwrap().unwrap(), original);
                    assert_eq!(store.read_asset_alias("asset", &key, None).unwrap().unwrap().value.object_hash, Some(payload.content_hash));
                }
            }
        }
    }
}
