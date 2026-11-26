//! Pure domain types for Cairn. No I/O, no async, no database types.
//!
//! See `docs/data-model.md` for the contract these types implement.

pub mod capture;
pub mod edge;
pub mod id;
pub mod item;
pub mod op;
pub mod query;
pub mod reltime;
pub mod view;

pub use capture::{Capture, CaptureSource, SourceKind};
pub use edge::{Edge, EdgeKind};
pub use id::Id;
pub use item::{Item, Lifecycle};
pub use op::{Actor, Op, OpKind};
pub use query::{Cmp, Query, SortDir, SortKey, Where};
pub use view::{LayoutEntry, View, ViewKind};

/// Tag normalization: lowercase, trimmed, internal whitespace → '-'.
pub fn normalize_tag(raw: &str) -> String {
    raw.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_normalize() {
        assert_eq!(normalize_tag("  Life Admin "), "life-admin");
        assert_eq!(normalize_tag("DMV"), "dmv");
    }
}
