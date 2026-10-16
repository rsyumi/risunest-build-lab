use risunest_sync_wire::{
    delta::{self, Op, Recipe},
    hash,
};

#[test]
fn repeated_prefixes_in_earlier_bases_do_not_hide_the_exact_later_base() {
    let first = vec![b'x'; 4096];
    let mut second = first.clone();
    second.extend((0..65536).map(|i| ((i * 73 + i / 251) % 256) as u8));
    let mut target = second.clone();
    target.extend_from_slice(b"appended");
    let recipe = delta::create(&[&first, &second], &target).unwrap();
    assert!(recipe.encode().unwrap().len() < 512);
    assert_eq!(recipe.apply(&[&first, &second]).unwrap(), target);
}

#[test]
fn exact_multi_base_insert_delete_reorder_and_opaque_content() {
    let a = (0..10000)
        .map(|i| ((i * 73 + i / 251) % 256) as u8)
        .collect::<Vec<_>>();
    let b = (0..10000)
        .map(|i| ((i * 71 + i / 127) % 256) as u8)
        .collect::<Vec<_>>();
    let target = [&b[64..5000], b"new\0\xff", &a[128..9000], &b[5000..]].concat();
    let recipe = delta::create(&[&a, &b], &target).unwrap();
    let encoded = recipe.encode().unwrap();
    assert!(encoded.len() < 1024);
    assert_eq!(
        Recipe::decode(&encoded).unwrap().apply(&[&a, &b]).unwrap(),
        target
    );
    assert!(recipe.apply(&[&b, &a]).is_err());
    for n in 0..encoded.len() {
        assert!(Recipe::decode(&encoded[..n]).is_err());
    }
    let mut extra = encoded.clone();
    extra.push(0);
    assert!(Recipe::decode(&extra).is_err());
    let mut corrupt = recipe.clone();
    corrupt.target_hash = hash(b"bad");
    assert!(corrupt.apply(&[&a, &b]).is_err());
    corrupt = recipe.clone();
    corrupt.ops[0] = Op::Copy {
        base: 0,
        offset: u64::MAX,
        length: 1,
    };
    assert!(corrupt.validate().is_err());
}
#[test]
fn ten_megabyte_plugin_single_edit_is_exact_and_small_both_directions() {
    let old = vec![b'a'; 10 * 1024 * 1024];
    let mut new = old.clone();
    new.splice(
        5 * 1024 * 1024..5 * 1024 * 1024,
        b"synthetic-new-content".iter().copied(),
    );
    for (base, target) in [(&old, &new), (&new, &old)] {
        let recipe = delta::create(&[base], target).unwrap();
        assert!(recipe.encode().unwrap().len() < 1024);
        assert_eq!(recipe.apply(&[base]).unwrap(), *target);
    }
}
#[test]
fn sparse_matches_stop_searching_a_sixteen_megabyte_target() {
    let base = vec![b'a'; delta::MAX_TARGET_BYTES];
    let mut target = vec![b'b'; delta::MAX_TARGET_BYTES];
    target[..64].copy_from_slice(&base[..64]);
    assert_eq!(
        delta::create(&[&base], &target).unwrap_err().0,
        "delta-search-limit"
    );
}

#[test]
fn useful_matches_after_a_bounded_literal_prefix_still_make_an_exact_recipe() {
    let base = vec![b'a'; 1024 * 1024];
    let target = [vec![b'b'; 128 * 1024], base.clone()].concat();
    let recipe = delta::create(&[&base], &target).unwrap();
    assert!(recipe.encoded_len().unwrap() < target.len() / 4);
    assert_eq!(recipe.apply(&[&base]).unwrap(), target);
}

#[test]
fn a_large_append_keeps_the_matching_base_when_search_is_exhausted() {
    let base = vec![b'a'; 2 * 1024 * 1024];
    let target = [base.clone(), vec![b'b'; 300 * 1024]].concat();
    let recipe = delta::create(&[&base], &target).unwrap();
    assert!(recipe.encoded_len().unwrap() < target.len() / 4);
    assert_eq!(recipe.apply(&[&base]).unwrap(), target);
}

#[test]
fn empty_and_no_base_exact_full_recipe_and_untrusted_limits() {
    let recipe = delta::create(&[], b"").unwrap();
    assert_eq!(
        Recipe::decode(&recipe.encode().unwrap())
            .unwrap()
            .apply(&[])
            .unwrap(),
        b""
    );
    assert_eq!(
        delta::create(&[], b"arbitrary bytes")
            .unwrap()
            .apply(&[])
            .unwrap(),
        b"arbitrary bytes"
    );
    let mut recipe = delta::create(&[b"x"], b"x").unwrap();
    recipe.bases.push(recipe.bases[0].clone());
    assert!(recipe.validate().is_err());
    let mut recipe = delta::create(&[], b"x").unwrap();
    recipe.target_size = u64::MAX;
    assert!(recipe.validate().is_err());
}
