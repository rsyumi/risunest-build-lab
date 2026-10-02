pub(crate) use risunest_sync_wire::{hash, validate_hash, Result, WireError};
#[path = "../../../../crates/sync-wire/src/delta.rs"]
pub(crate) mod delta;
#[path = "../../../../crates/sync-wire/src/stream_delta.rs"]
pub(crate) mod stream_delta;
#[path = "../../../../crates/sync-wire/src/transfer.rs"]
pub(crate) mod transfer;

pub(crate) fn from_wire(recipe: risunest_sync_wire::delta::Recipe) -> delta::Recipe {
    delta::Recipe {
        bases: recipe
            .bases
            .into_iter()
            .map(|base| delta::Base {
                hash: base.hash,
                size: base.size,
            })
            .collect(),
        target_hash: recipe.target_hash,
        target_size: recipe.target_size,
        ops: recipe
            .ops
            .into_iter()
            .map(|op| match op {
                risunest_sync_wire::delta::Op::Insert(bytes) => delta::Op::Insert(bytes),
                risunest_sync_wire::delta::Op::Copy {
                    base,
                    offset,
                    length,
                } => delta::Op::Copy {
                    base,
                    offset,
                    length,
                },
            })
            .collect(),
    }
}
pub(crate) fn to_wire(recipe: delta::Recipe) -> risunest_sync_wire::delta::Recipe {
    risunest_sync_wire::delta::Recipe {
        bases: recipe
            .bases
            .into_iter()
            .map(|base| risunest_sync_wire::delta::Base {
                hash: base.hash,
                size: base.size,
            })
            .collect(),
        target_hash: recipe.target_hash,
        target_size: recipe.target_size,
        ops: recipe
            .ops
            .into_iter()
            .map(|op| match op {
                delta::Op::Insert(bytes) => risunest_sync_wire::delta::Op::Insert(bytes),
                delta::Op::Copy {
                    base,
                    offset,
                    length,
                } => risunest_sync_wire::delta::Op::Copy {
                    base,
                    offset,
                    length,
                },
            })
            .collect(),
    }
}
