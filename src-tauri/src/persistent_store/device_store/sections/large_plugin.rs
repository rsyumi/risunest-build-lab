use super::*;
use std::io::{self, Read, Seek, SeekFrom, Write};

pub(super) fn capture<R: Read>(target: Option<&Connection>, space: &str, mut source: impl FnMut() -> StoreResult<R>) -> StoreResult<SectionValueRow> {
    let mut structure = None;
    if space == "json" {
        match structure::validate_text(&mut source)? {
            Some(prefix) => structure = Some(part_references(&prefix)?),
            None => {
                serde_json::from_reader::<_, serde::de::IgnoredAny>(std::io::BufReader::new(source()?))
                    .map_err(|_| invalid("Section value object is not JSON"))?;
            }
        }
    }
    let object = match target {
        Some(target) => text_value::store_encoded(target, &mut source)?,
        None => text_value::encode(source()?, io::sink(), text_value::MAX_VALUE_BYTES)?,
    };
    let mut parts = vec![];
    let mut buffer = Vec::with_capacity(text_value::PART_BYTES);
    let mut flush = |buffer: &mut Vec<u8>| -> io::Result<()> {
        if buffer.is_empty() { return Ok(()); }
        let reference = object_reference(buffer);
        if let Some(target) = target {
            target.execute("INSERT OR IGNORE INTO section_objects(hash,bytes) VALUES(?1,?2)",
                params![hex::encode(reference.content_sha256), &*buffer]).map_err(io::Error::other)?;
        }
        parts.push(reference); buffer.clear(); Ok(())
    };
    text_value::visit_text(source()?, |text| {
        let mut remaining = text.as_bytes();
        while !remaining.is_empty() {
            let mut length = remaining.len().min(text_value::PART_BYTES - buffer.len());
            while length > 0 && length < remaining.len() && remaining[length] & 0xc0 == 0x80 { length -= 1; }
            if length == 0 { flush(&mut buffer)?; continue; }
            buffer.extend_from_slice(&remaining[..length]);
            remaining = &remaining[length..];
            if buffer.len() == text_value::PART_BYTES { flush(&mut buffer)?; }
        }
        Ok(())
    })?;
    flush(&mut buffer)?;
    Ok(SectionValueRow::PluginObject { space: space.into(), object, parts, structure })
}

impl SectionSpoolBuilder {
    pub(crate) fn push_entry(&mut self, entry: SectionEntry, object: impl FnMut(&ObjectReference) -> StoreResult<Vec<u8>>) -> StoreResult<()> {
        if !self.versioned { return self.push_backup_entry(entry, object); }
        if entry.kind != self.kind || entry.version.is_none() { return Err(invalid("Section entry has the wrong identity")); }
        let encoded = entry.encode().map_err(section_format_error)?;
        let key = entry.key.clone();
        let hash = risunest_external_storage_format::content_identity::hash(&encoded);
        let row = self.decode_entry(entry, true, object)?;
        self.push(row, &key, &hash)
    }

    pub(super) fn decode_entry(&self, entry: SectionEntry, versioned: bool, mut object: impl FnMut(&ObjectReference) -> StoreResult<Vec<u8>>) -> StoreResult<SectionRow> {
        let value = match &entry.value {
            SectionValue::LocalPlugin(LocalPluginValue { space, value: PluginValue::Objects(parts) }) => {
                entry.validate().map_err(section_format_error)?;
                let space = match space { PluginSpace::String => "string", PluginSpace::Json => "json" };
                self.capture_text(space, parts, &mut object)?
            }
            SectionValue::LocalPlugin(LocalPluginValue { value: PluginValue::Structured(value), .. }) => {
                entry.validate().map_err(section_format_error)?;
                self.decode_structured(value, &mut object)?
            }
            _ => return SectionRow::from_entry(entry, versioned, object),
        };
        let (key1, key2, key3) = decode_entry_key(entry.kind, &entry.key)?;
        let (write_clock, writer_id) = match entry.version {
            Some(version) => (version.write_clock, version.writer_id),
            None if !versioned => (Sequence::from(0u64), String::new()),
            None => return Err(invalid("Section entry carries no version")),
        };
        Ok(SectionRow { key1, key2, key3, value, write_clock, writer_id })
    }

    fn capture_text(&self, space: &str, parts: &[ObjectReference], object: &mut impl FnMut(&ObjectReference) -> StoreResult<Vec<u8>>) -> StoreResult<SectionValueRow> {
        let mut raw = tempfile::tempfile()?;
        for reference in parts {
            let bytes = read_object(reference, object)?;
            raw.write_all(&bytes)?;
            self.connection.execute("INSERT OR IGNORE INTO section_objects(hash,bytes) VALUES(?1,?2)",
                params![hex::encode(reference.content_sha256), bytes])?;
        }
        let mut value = capture(Some(&self.connection), space, || {
            let mut file = raw.try_clone()?; file.seek(SeekFrom::Start(0))?; Ok(file)
        })?;
        if let SectionValueRow::PluginObject { parts: stored, .. } = &mut value { *stored = parts.to_vec(); }
        Ok(value)
    }

    /// The text must name exactly the entry's parts, and the stored size must
    /// fit before any part reaches the device.
    fn decode_structured(&self, value: &StructuredPluginValue, object: &mut impl FnMut(&ObjectReference) -> StoreResult<Vec<u8>>) -> StoreResult<SectionValueRow> {
        let text = match &value.text {
            StructureText::Inline(text) => Some(text.clone()),
            StructureText::Object(reference) => Some(object_text(reference, object, false)?),
            StructureText::Objects(_) => None,
        };
        let (row, named, encoded) = match text {
            Some(text) => {
                let named = structure::validate_text(|| Ok(text.as_bytes()))?.as_ref().map(part_references).transpose()?;
                let encoded = text_value::encoded_length(&text);
                (SectionValueRow::Plugin { space: "json".into(), value: text }, named, encoded)
            }
            None => {
                let row = self.capture_text("json", value.text.references(), object)?;
                let SectionValueRow::PluginObject { object: info, structure, .. } = &row else { unreachable!() };
                let (named, encoded) = (structure.clone(), info.encoded_length);
                (row, named, encoded)
            }
        };
        if named.as_ref() != Some(&value.parts) { return Err(invalid("Structured plugin value names other parts")); }
        let size = value.parts.iter().try_fold(encoded, |size, part| size.checked_add(part.byte_length));
        if size.is_none_or(|size| size > text_value::MAX_VALUE_BYTES) { return Err(invalid("plugin device value is too large")); }
        for reference in &value.parts {
            let bytes = read_object(reference, object)?;
            self.connection.execute("INSERT OR IGNORE INTO section_objects(hash,bytes) VALUES(?1,?2)",
                params![hex::encode(reference.content_sha256), bytes])?;
        }
        Ok(row)
    }
}

impl PreparedSectionRows {
    /// Installs the text objects and structure parts the rows name, so the
    /// rows can be written afterwards.
    pub(crate) fn retain_plugin_objects(&self, target: &Transaction<'_>) -> StoreResult<()> {
        self.visit(|row| {
            let parts = match row.value {
                SectionValueRow::PluginObject { object, structure, .. } => {
                    text_value::copy_object(&self.connection, target, &object)?;
                    structure
                }
                SectionValueRow::Plugin { space, value } if space == "json" => structure_references(&value)?,
                _ => None,
            };
            for reference in parts.iter().flatten() {
                let hash = hex::encode(reference.content_sha256);
                let bytes: Vec<u8> = self.connection.query_row("SELECT bytes FROM section_objects WHERE hash=?1", [&hash], |row| row.get(0))?;
                if structure::put_part(target, &bytes)? != hash { return Err(invalid("Section object does not match its reference")); }
            }
            Ok(())
        })
    }
}
