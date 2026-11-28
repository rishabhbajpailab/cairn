//! The repo's `plugins/` directory compiled into the binary (Workers have no filesystem),
//! with every plugin schema turned into Rust at build time by `jsonschema::validator`.
//!
//! Loading [`Registry`](cairn_plugins::Registry) from [`Embedded`] therefore parses only the
//! small manifests: no schema is compiled in the request path, and none is checked against
//! the JSON Schema meta-schema (the build already did both). This keeps the first request in
//! a fresh Worker isolate cheap in CPU time; see docs/deploy.md § CPU limits.

use cairn_plugins::{Compiled, PluginSource};
use include_dir::{Dir, include_dir};
use serde_json::Value;

static PLUGINS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../plugins");

include!(concat!(env!("OUT_DIR"), "/compiled.rs"));

/// [`PluginSource`] over the embedded `plugins/` directory, with build-time validators.
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

    fn compiled(&self, sub: &str, dir: &str, file: &str) -> Option<Compiled> {
        lookup(&format!("{sub}/{dir}/{file}"))
    }
}

/// Number of schemas compiled at build time.
pub fn compiled_count() -> usize {
    COUNT
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_plugins::{FsSource, Registry};
    use serde_json::json;
    use std::path::Path;

    fn plugins_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
    }

    #[test]
    fn every_embedded_schema_is_precompiled() {
        let r = Registry::load_from(&Embedded).expect("embedded plugins load");
        assert!(r.runtime_compiled().is_empty(), "compiled at runtime: {:?}", r.runtime_compiled());
        assert_eq!(compiled_count(), r.type_names().count() + ["digest", "triage"].len());
    }

    /// Instances that exercise required fields, wrong types and extra fields, for comparing
    /// the generated validators with the runtime ones.
    fn samples() -> Vec<Value> {
        vec![
            json!({}),
            json!(null),
            json!("text"),
            json!([]),
            json!({"text": "x"}),
            json!({"text": 1}),
            json!({"text": "x", "done": "yes"}),
            json!({"text": "x", "done": true, "due": "2026-01-01T00:00:00Z", "priority": 2}),
            json!({"text": "x", "at": "2026-01-01T00:00:00Z"}),
            json!({"name": "b"}),
            json!({"items": []}),
            json!({"items": [{"id": "x"}]}),
            json!({"summary": "s", "items": []}),
        ]
    }

    #[test]
    fn generated_validators_agree_with_runtime_ones() {
        let src = FsSource(plugins_dir());
        for (sub, key) in [("types", "schema"), ("ai", "output")] {
            for d in src.dirs(sub) {
                let manifest: Value =
                    serde_json::from_str(&src.read(sub, &d, "plugin.json").expect("manifest")).expect("manifest json");
                let file = manifest[key].as_str().expect("schema file");
                let schema: Value = serde_json::from_str(&src.read(sub, &d, file).expect("schema")).expect("json");
                let runtime = jsonschema::validator_for(&schema).expect("schema compiles");
                let compiled = Embedded.compiled(sub, &d, file).expect("precompiled");
                for s in samples() {
                    assert_eq!((compiled.is_valid)(&s), runtime.is_valid(&s), "{sub}/{d} disagrees on {s}");
                    assert_eq!((compiled.errors)(&s).is_empty(), runtime.is_valid(&s), "{sub}/{d} errors on {s}");
                }
            }
        }
    }
}
