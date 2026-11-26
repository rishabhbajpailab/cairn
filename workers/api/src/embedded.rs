//! Plugins and schema compiled into the Worker (no filesystem at runtime).

use cairn_plugins::PluginSource;
use include_dir::{Dir, include_dir};

static PLUGINS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../plugins");
static SCHEMA: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../schema");
pub const CORE_CONFIG: &str = include_str!("../../../config/cairn.toml");

pub struct Embedded;

impl PluginSource for Embedded {
    fn dirs(&self, sub: &str) -> Vec<String> {
        let mut d: Vec<String> = PLUGINS
            .get_dir(sub)
            .map(|dir| {
                dir.dirs().filter_map(|d| d.path().file_name().map(|n| n.to_string_lossy().into_owned())).collect()
            })
            .unwrap_or_default();
        d.sort();
        d
    }

    fn read(&self, sub: &str, dir: &str, file: &str) -> Option<String> {
        PLUGINS.get_file(format!("{sub}/{dir}/{file}")).and_then(|f| f.contents_utf8()).map(str::to_owned)
    }
}

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
