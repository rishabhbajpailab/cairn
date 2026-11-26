//! Schema and config compiled into the Worker (no filesystem at runtime). Plugins are
//! embedded by `cairn-plugins-embedded`, which also precompiles their JSON Schemas.

use include_dir::{Dir, include_dir};

pub use cairn_plugins_embedded::Embedded;

static SCHEMA: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../schema");
pub const CORE_CONFIG: &str = include_str!("../../../config/cairn.toml");

/// `schema/*.surql` in filename order.
pub fn schema_files() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = SCHEMA
        .files()
        .filter(|f| f.path().extension().is_some_and(|e| e == "surql"))
        .filter_map(|f| Some((f.path().file_name()?.to_string_lossy().into_owned(), f.contents_utf8()?.to_owned())))
        .collect();
    files.sort();
    files
}
