use risunest_sync_wire::{delta::{Base, Op, Recipe, MAX_OPS, MAX_PATCH_BYTES}, hash, transfer::{self, Frame}};

fn recipe(size: usize) -> Recipe {
    Recipe { bases: vec![], target_hash: hash(&[]), target_size: size as u64,
        ops: if size == 0 { vec![] } else { vec![Op::Insert(vec![7; size])] } }
}
fn same_recipe(value: &Recipe) {
    assert_eq!(value.encoded_len(), value.encode().map(|bytes| bytes.len()));
}
fn same_frames(frames: &[Frame]) {
    assert_eq!(transfer::encoded_len(frames), transfer::encode(frames).map(|bytes| bytes.len()));
}
#[test]
fn lengths_include_all_headers_and_exact_batch_limits() {
    assert_eq!(transfer::encoded_len(&[]).unwrap(), 8);
    same_frames(&[]);
    let frames = [Frame::Full(vec![]), Frame::Delta(recipe(0)),
        Frame::FullRequired { hash: hash(&[]), size: u64::MAX }];
    assert_eq!(transfer::encoded_len(&frames).unwrap(), 8 + 45 + 54 + 45);
    same_frames(&frames);
    let mut frames = vec![Frame::Full(vec![7; transfer::MAX_BATCH_BYTES - 53])];
    assert_eq!(transfer::encoded_len(&frames).unwrap(), transfer::MAX_BATCH_BYTES);
    same_frames(&frames);
    if let Frame::Full(bytes) = &mut frames[0] { bytes.push(7); }
    assert_eq!(transfer::encoded_len(&frames).unwrap_err().0, "batch-too-large");
    same_frames(&frames);
    let mut frames = (0..transfer::MAX_BATCH_OBJECTS).map(|_| Frame::Full(vec![])).collect::<Vec<_>>();
    same_frames(&frames);
    frames.push(Frame::Full(vec![]));
    assert_eq!(transfer::encoded_len(&frames).unwrap_err().0, "too-many-frames");
    same_frames(&frames);
    same_frames(&[Frame::FullRequired { hash: "invalid".into(), size: 0 }]);
}
#[test]
fn recipe_lengths_preserve_validation_and_patch_boundaries() {
    same_recipe(&recipe(0));
    assert_eq!(recipe(0).encoded_len().unwrap(), 49);
    let boundary = recipe(MAX_PATCH_BYTES - 54);
    assert_eq!(boundary.encoded_len().unwrap(), MAX_PATCH_BYTES);
    same_recipe(&boundary);
    same_frames(&[Frame::Delta(boundary)]);
    let framed_boundary = [Frame::Delta(recipe(transfer::MAX_BATCH_BYTES - 67))];
    assert_eq!(transfer::encoded_len(&framed_boundary).unwrap(), transfer::MAX_BATCH_BYTES);
    same_frames(&framed_boundary);
    same_recipe(&recipe(MAX_PATCH_BYTES - 53));
    assert_eq!(recipe(MAX_PATCH_BYTES - 53).encoded_len().unwrap_err().0, "delta-limit");
    let mut copies = Recipe { bases: vec![Base { hash: hash(b"x"), size: 1 }],
        target_hash: hash(&[]), target_size: MAX_OPS as u64,
        ops: vec![Op::Copy { base: 0, offset: 0, length: 1 }; MAX_OPS] };
    assert_eq!(copies.encoded_len().unwrap(), 89 + MAX_OPS * 14);
    same_recipe(&copies);
    copies.ops.push(Op::Copy { base: 0, offset: 0, length: 1 });
    copies.target_size += 1;
    same_recipe(&copies);
    let mut bad = recipe(1);
    bad.target_size = 2;
    same_recipe(&bad);
    same_frames(&[Frame::Delta(bad)]);
    let mut bad = recipe(1);
    bad.target_hash = "bad".into();
    same_recipe(&bad);
    let mut bad = recipe(1);
    bad.ops = vec![Op::Copy { base: 0, offset: u64::MAX, length: 1 }];
    same_recipe(&bad);
    let mut bad = recipe(0);
    bad.ops.push(Op::Insert(vec![]));
    same_recipe(&bad);
    let base = Base { hash: hash(b"x"), size: 1 };
    let mut bad = recipe(0);
    bad.bases = vec![base.clone(); 2];
    same_recipe(&bad);
    let exact_bases = Recipe { bases: (0..4).map(|n| Base { hash: hash(&[n]), size: 1 }).collect(),
        target_hash: hash(&[]), target_size: 0, ops: vec![] };
    assert_eq!(exact_bases.encoded_len().unwrap(), 49 + 4 * 40);
    same_recipe(&exact_bases);
    bad.bases = vec![base; 5];
    same_recipe(&bad);
}
#[test]
fn mixed_recipe_and_frame_encoding_stays_exact() {
    let base = b"abcd";
    let recipe = Recipe { bases: vec![Base { hash: hash(base), size: 4 }],
        target_hash: hash(b"bctail"), target_size: 6,
        ops: vec![Op::Copy { base: 0, offset: 1, length: 2 }, Op::Insert(b"tail".to_vec())] };
    assert_eq!(recipe.encoded_len().unwrap(), 89 + 14 + 9);
    same_recipe(&recipe);
    let frames = [Frame::Full(base.to_vec()), Frame::Delta(recipe.clone())];
    same_frames(&frames);
    let encoded = transfer::encode(&frames).unwrap();
    let decoded = transfer::decode(&encoded).unwrap();
    assert_eq!(transfer::encode(&decoded).unwrap(), encoded);
    assert_eq!(recipe.apply(&[base]).unwrap(), b"bctail");
    for full_len in [recipe.encoded_len().unwrap() + 64, recipe.encoded_len().unwrap() + 65] {
        assert_eq!(recipe.encoded_len().unwrap() + 64 < full_len,
            recipe.encode().unwrap().len() + 64 < full_len);
    }
}
