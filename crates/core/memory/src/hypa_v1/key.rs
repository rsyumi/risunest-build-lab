use super::{CacheKey, EmbeddingTarget};
use risunest_ir::{Budget, Text};
use risunest_ir::stage::StageError;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use unicode_normalization::{char::decompose_canonical, UnicodeNormalization};

fn encoded(hash: &mut Sha256, ch: char, budget: &Budget) -> Result<(), StageError> {
    let mut bytes = [0; 4];
    let bytes = ch.encode_utf8(&mut bytes).as_bytes();
    budget.work(bytes.len())?;
    hash.update(bytes);
    Ok(())
}

fn text(hash: &mut Sha256, value: &Text, normalize: bool, budget: &Budget) -> Result<(), StageError> {
    if !normalize {
        for ch in char::decode_utf16(value.units()) { encoded(hash, ch.unwrap_or(char::REPLACEMENT_CHARACTER), budget)?; }
        return Ok(());
    }
    // Preflight the trusted iterator's maximum decomposition and stable-sort storage.
    // Replacement characters are inert starters, preserving isolated-surrogate NFC barriers.
    let mut count = 0usize;
    let mut run = 0usize;
    for ch in char::decode_utf16(value.units()) {
        budget.work(1)?;
        let mut failure = None;
        decompose_canonical(ch.unwrap_or(char::REPLACEMENT_CHARACTER), |part| {
            if failure.is_some() { return; }
            let result = (|| -> Result<(), StageError> {
                budget.work(1)?;
                count = count.checked_add(1).ok_or_else(|| StageError::Limit("work".into()))?;
                if unicode_normalization::char::canonical_combining_class(part) == 0 {
                    // This bounds the iterator's uninterruptible stable sort of a full run.
                    budget.work(run.checked_mul(run).ok_or_else(|| StageError::Limit("work".into()))?)?;
                    run = 0;
                } else { run = run.checked_add(1).ok_or_else(|| StageError::Limit("work".into()))?; }
                Ok(())
            })();
            if let Err(error) = result { failure = Some(error); }
        });
        if let Some(error) = failure { return Err(error); }
    }
    budget.work(run.checked_mul(run).ok_or_else(|| StageError::Limit("work".into()))?)?;
    // TinyVec grows by doubling. Cover both old/new buffers, recomposition, and full stable-sort scratch.
    let capacity = count.max(8).checked_next_power_of_two().ok_or_else(|| StageError::Limit("owned-bytes".into()))?;
    let _scratch = budget.reserve_owned(capacity.checked_mul(64).ok_or_else(|| StageError::Limit("owned-bytes".into()))?)?;
    let failure = RefCell::new(None);
    let source = char::decode_utf16(value.units()).scan((), |_, ch| {
        if let Err(error) = budget.work(1) { *failure.borrow_mut() = Some(error); return None; }
        Some(ch.unwrap_or(char::REPLACEMENT_CHARACTER))
    });
    for ch in source.nfc() {
        if let Some(error) = failure.borrow_mut().take() { return Err(error.into()); }
        encoded(hash, ch, budget)?;
    }
    if let Some(error) = failure.into_inner() { return Err(error.into()); }
    budget.check()?;
    Ok(())
}

pub(super) fn key(content: &Text, target: &EmbeddingTarget, budget: &Budget) -> Result<CacheKey, StageError> {
    budget.work(18)?;
    let mut hash = Sha256::new();
    hash.update(b"hypa-v1-text\0");
    text(&mut hash, content, true, budget)?;
    hash.update([0]);
    text(&mut hash, &target.identity_model, false, budget)?;
    hash.update([0]);
    text(&mut hash, &target.identity_endpoint, false, budget)?;
    hash.update(b"\x001\0");
    Ok(CacheKey(hash.finalize().into()))
}
