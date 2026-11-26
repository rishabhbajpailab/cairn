//! Plugin host. Loads `plugins/types/*/plugin.json` and `plugins/ai/*/plugin.json`,
//! compiles JSON Schemas, and provides the operations the core needs without ever
//! interpreting payloads itself:
//!
//! - [`Registry::validate`]: payload against the type's JSON Schema
//! - [`Registry::extract_idx`]: copy plugin-declared index fields out of the payload
//! - [`Registry::derive_title`]: title from `title_from`
//! - [`Registry::match_capture`]: declarative capture matchers (prefixes), deterministic order
//! - [`migrate`]: declarative payload migrations (see docs/plugins.md)

pub mod manifest;
pub mod migrate;

pub use manifest::{ActionSpec, AiManifest, CaptureMatcher, IndexKind, TypeManifest};

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("io error reading {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("invalid manifest {path}: {source}")]
    Manifest { path: PathBuf, source: serde_json::Error },
    #[error("invalid JSON Schema for type `{name}`: {message}")]
    Schema { name: String, message: String },
    #[error("duplicate plugin name `{0}`")]
    Duplicate(String),
    #[error("unknown type `{0}`")]
    UnknownType(String),
}

pub struct LoadedType {
    pub manifest: TypeManifest,
    pub dir: PathBuf,
    validator: jsonschema::Validator,
}

pub struct LoadedAi {
    pub manifest: AiManifest,
    pub dir: PathBuf,
    /// Prompt template text (`prompt.md`).
    pub prompt: String,
    pub output_validator: jsonschema::Validator,
}

/// All loaded plugins. BTreeMap keeps iteration (and therefore capture matching) deterministic.
#[derive(Default)]
pub struct Registry {
    types: BTreeMap<String, LoadedType>,
    ai: BTreeMap<String, LoadedAi>,
}

/// Where plugin files come from: the filesystem (native tests, tooling) or files embedded
/// into the Worker at build time (`include_dir!`), since Workers have no filesystem.
pub trait PluginSource {
    /// Sorted names of plugin directories under `sub` ("types" or "ai").
    fn dirs(&self, sub: &str) -> Vec<String>;
    /// Contents of `sub/dir/file`, if present.
    fn read(&self, sub: &str, dir: &str, file: &str) -> Option<String>;
}

/// Filesystem source rooted at a `plugins/` directory.
pub struct FsSource(pub PathBuf);

impl PluginSource for FsSource {
    fn dirs(&self, sub: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.0.join(sub)) else { return vec![] };
        let mut d: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        d.sort();
        d
    }

    fn read(&self, sub: &str, dir: &str, file: &str) -> Option<String> {
        std::fs::read_to_string(self.0.join(sub).join(dir).join(file)).ok()
    }
}

fn parse_json(path: String, raw: &str) -> Result<Value, PluginError> {
    serde_json::from_str(raw).map_err(|source| PluginError::Manifest { path: path.into(), source })
}

fn compile(name: &str, schema: &Value) -> Result<jsonschema::Validator, PluginError> {
    jsonschema::validator_for(schema).map_err(|e| PluginError::Schema { name: name.into(), message: e.to_string() })
}

impl Registry {
    /// Load every plugin under `root` (expects `root/types/*` and `root/ai/*`).
    pub fn load(root: &Path) -> Result<Self, PluginError> {
        Self::load_from(&FsSource(root.to_path_buf()))
    }

    /// Load from any [`PluginSource`]. Directory order is sorted, so loading is deterministic.
    pub fn load_from(src: &dyn PluginSource) -> Result<Self, PluginError> {
        let mut reg = Registry::default();
        for (sub, is_type) in [("types", true), ("ai", false)] {
            for d in src.dirs(sub) {
                let Some(raw) = src.read(sub, &d, "plugin.json") else { continue };
                let mpath = format!("{sub}/{d}/plugin.json");
                let value = parse_json(mpath.clone(), &raw)?;
                let missing = |f: &str| PluginError::Io {
                    path: format!("{sub}/{d}/{f}").into(),
                    source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing file"),
                };
                if is_type {
                    let manifest: TypeManifest = serde_json::from_value(value)
                        .map_err(|source| PluginError::Manifest { path: mpath.clone().into(), source })?;
                    let schema_raw = src.read(sub, &d, &manifest.schema).ok_or_else(|| missing(&manifest.schema))?;
                    let schema = parse_json(format!("{sub}/{d}/{}", manifest.schema), &schema_raw)?;
                    reg.insert_type(manifest, &schema, PathBuf::from(sub).join(&d))?;
                } else {
                    let manifest: AiManifest = serde_json::from_value(value)
                        .map_err(|source| PluginError::Manifest { path: mpath.clone().into(), source })?;
                    let out_raw = src.read(sub, &d, &manifest.output).ok_or_else(|| missing(&manifest.output))?;
                    let schema = parse_json(format!("{sub}/{d}/{}", manifest.output), &out_raw)?;
                    let output_validator = compile(&manifest.name, &schema)?;
                    let prompt = src.read(sub, &d, &manifest.prompt).ok_or_else(|| missing(&manifest.prompt))?;
                    if reg.ai.contains_key(&manifest.name) {
                        return Err(PluginError::Duplicate(manifest.name));
                    }
                    tracing::info!(plugin = %manifest.name, "loaded ai capability");
                    reg.ai.insert(
                        manifest.name.clone(),
                        LoadedAi { manifest, dir: PathBuf::from(sub).join(&d), prompt, output_validator },
                    );
                }
            }
        }
        Ok(reg)
    }

    /// Register a type from an in-memory manifest and schema (used by tests and `load`).
    pub fn insert_type(&mut self, manifest: TypeManifest, schema: &Value, dir: PathBuf) -> Result<(), PluginError> {
        if self.types.contains_key(&manifest.name) {
            return Err(PluginError::Duplicate(manifest.name));
        }
        let validator = compile(&manifest.name, schema)?;
        tracing::info!(plugin = %manifest.name, version = manifest.version, "loaded type");
        self.types.insert(manifest.name.clone(), LoadedType { manifest, dir, validator });
        Ok(())
    }

    pub fn get_type(&self, name: &str) -> Option<&LoadedType> {
        self.types.get(name)
    }

    pub fn get_ai(&self, name: &str) -> Option<&LoadedAi> {
        self.ai.get(name)
    }

    pub fn type_names(&self) -> impl Iterator<Item = &str> {
        self.types.keys().map(String::as_str)
    }

    /// Validate a payload. Returns human-readable error messages on failure.
    pub fn validate(&self, type_name: &str, payload: &Map<String, Value>) -> Result<(), Vec<String>> {
        let t = self.types.get(type_name).ok_or_else(|| vec![format!("unknown type `{type_name}`")])?;
        let instance = Value::Object(payload.clone());
        let errors: Vec<String> = t.validator.iter_errors(&instance).map(|e| e.to_string()).collect();
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }

    /// Copy the fields a plugin lists in `index` out of the payload.
    pub fn extract_idx(&self, type_name: &str, payload: &Map<String, Value>) -> Map<String, Value> {
        let Some(t) = self.types.get(type_name) else { return Map::new() };
        t.manifest.index.keys().filter_map(|k| payload.get(k).map(|v| (k.clone(), v.clone()))).collect()
    }

    pub fn derive_title(&self, type_name: &str, payload: &Map<String, Value>) -> Option<String> {
        let t = self.types.get(type_name)?;
        let field = t.manifest.title_from.as_ref()?;
        let s = payload.get(field)?.as_str()?;
        Some(s.lines().next().unwrap_or(s).chars().take(120).collect())
    }

    /// Run declarative capture matchers in deterministic (alphabetical type) order.
    /// Returns `(type, payload)` for the first match.
    pub fn match_capture(&self, text: &str) -> Option<(String, Map<String, Value>)> {
        let trimmed = text.trim_start();
        let lower = trimmed.to_lowercase();
        for (name, t) in &self.types {
            for m in &t.manifest.capture {
                match m {
                    CaptureMatcher::Prefix { prefix, field } => {
                        if lower.starts_with(&prefix.to_lowercase()) {
                            let rest = trimmed.get(prefix.len()..).unwrap_or("").trim().to_string();
                            let mut payload = Map::new();
                            payload.insert(field.clone(), Value::String(rest));
                            return Some((name.clone(), payload));
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn todo_registry() -> Registry {
        let manifest: TypeManifest = serde_json::from_value(json!({
            "kind": "type", "name": "todo", "version": 1, "title": "To-do",
            "schema": "schema.json",
            "index": { "due": "datetime", "done": "bool" },
            "title_from": "text",
            "capture": [{ "prefix": "todo:", "field": "text" }]
        }))
        .unwrap();
        let schema = json!({
            "type": "object", "required": ["text"],
            "properties": { "text": {"type": "string"}, "done": {"type": "boolean"}, "due": {"type": "string"} }
        });
        let mut r = Registry::default();
        r.insert_type(manifest, &schema, PathBuf::new()).unwrap();
        r
    }

    #[test]
    fn validates_payloads() {
        let r = todo_registry();
        let ok = json!({"text": "renew"}).as_object().unwrap().clone();
        let bad = json!({"done": "yes"}).as_object().unwrap().clone();
        assert!(r.validate("todo", &ok).is_ok());
        assert!(r.validate("todo", &bad).is_err());
    }

    #[test]
    fn prefix_matcher_is_case_insensitive() {
        let r = todo_registry();
        let (t, p) = r.match_capture("TODO: renew registration").unwrap();
        assert_eq!(t, "todo");
        assert_eq!(p["text"], "renew registration");
        assert!(r.match_capture("just a thought").is_none());
    }

    #[test]
    fn idx_only_copies_declared_fields() {
        let r = todo_registry();
        let p = json!({"text": "x", "done": true, "secret": 1}).as_object().unwrap().clone();
        let idx = r.extract_idx("todo", &p);
        assert_eq!(idx.len(), 1);
        assert_eq!(idx["done"], true);
    }

    #[test]
    fn loads_repo_plugins() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let r = Registry::load(&root).expect("repo plugins must load");
        for t in ["bucket", "hold-that-thought", "reminder", "task", "task-run", "todo"] {
            assert!(r.get_type(t).is_some(), "missing type plugin {t}");
        }
        assert!(r.get_ai("triage").is_some());
        assert!(r.get_ai("digest").is_some());
    }
}
