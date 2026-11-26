//! Plugin host. Loads `plugins/types/*/plugin.json` and `plugins/ai/*/plugin.json`,
//! compiles JSON Schemas, and provides the operations the core needs without ever
//! interpreting payloads itself:
//!
//! - [`Registry::validate`]: payload against the type's JSON Schema
//! - [`Registry::extract_idx`]: copy plugin-declared index fields out of the payload
//! - [`Registry::derive_title`]: title from `title_from`
//! - [`Registry::match_capture`]: declarative capture matchers (prefixes), deterministic order
//! - [`migrate`]: declarative payload migrations (see docs/plugins.md)
//!
//! Schemas can be checked two ways. A [`PluginSource`] may hand over a [`Compiled`] validator
//! generated at build time (the Worker does this through `cairn-plugins-embedded`), so loading
//! the registry compiles nothing. Otherwise the schema is compiled at load time with
//! `jsonschema`, which is fine natively but costs tens of milliseconds of CPU in a fresh
//! Worker isolate (see docs/deploy.md § CPU limits).

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

/// A validator generated at build time (`jsonschema::validator` macro), as plain function
/// pointers so the registry doesn't depend on the generated types.
#[derive(Clone, Copy)]
pub struct Compiled {
    pub is_valid: fn(&Value) -> bool,
    /// Every error message; empty when valid.
    pub errors: fn(&Value) -> Vec<String>,
}

enum Check {
    Compiled(Compiled),
    Runtime(Box<jsonschema::Validator>),
}

impl Check {
    fn errors(&self, instance: &Value) -> Vec<String> {
        match self {
            Check::Compiled(c) if (c.is_valid)(instance) => vec![],
            Check::Compiled(c) => (c.errors)(instance),
            Check::Runtime(v) => v.iter_errors(instance).map(|e| e.to_string()).collect(),
        }
    }

    fn is_runtime(&self) -> bool {
        matches!(self, Check::Runtime(_))
    }
}

pub struct LoadedType {
    pub manifest: TypeManifest,
    pub dir: PathBuf,
    check: Check,
}

pub struct LoadedAi {
    pub manifest: AiManifest,
    pub dir: PathBuf,
    /// Prompt template text (`prompt.md`).
    pub prompt: String,
    output_check: Check,
}

impl LoadedAi {
    /// Validate model output against `output.schema.json`.
    pub fn validate_output(&self, output: &Value) -> Result<(), Vec<String>> {
        let errors = self.output_check.errors(output);
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
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
    /// A build-time validator for the schema at `sub/dir/file`, if the source has one.
    /// When present, the schema file is not read or compiled at load time.
    fn compiled(&self, _sub: &str, _dir: &str, _file: &str) -> Option<Compiled> {
        None
    }
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

fn compile(name: &str, schema: &Value) -> Result<Check, PluginError> {
    jsonschema::validator_for(schema)
        .map(|v| Check::Runtime(Box::new(v)))
        .map_err(|e| PluginError::Schema { name: name.into(), message: e.to_string() })
}

/// The build-time validator for `sub/dir/file` if the source has one, else read and compile it.
fn check_for(src: &dyn PluginSource, name: &str, sub: &str, dir: &str, file: &str) -> Result<Check, PluginError> {
    if let Some(c) = src.compiled(sub, dir, file) {
        return Ok(Check::Compiled(c));
    }
    let raw = src.read(sub, dir, file).ok_or_else(|| PluginError::Io {
        path: format!("{sub}/{dir}/{file}").into(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing file"),
    })?;
    compile(name, &parse_json(format!("{sub}/{dir}/{file}"), &raw)?)
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
                    let check = check_for(src, &manifest.name, sub, &d, &manifest.schema)?;
                    reg.insert_checked(manifest, check, PathBuf::from(sub).join(&d))?;
                } else {
                    let manifest: AiManifest = serde_json::from_value(value)
                        .map_err(|source| PluginError::Manifest { path: mpath.clone().into(), source })?;
                    let output_check = check_for(src, &manifest.name, sub, &d, &manifest.output)?;
                    let prompt = src.read(sub, &d, &manifest.prompt).ok_or_else(|| missing(&manifest.prompt))?;
                    if reg.ai.contains_key(&manifest.name) {
                        return Err(PluginError::Duplicate(manifest.name));
                    }
                    tracing::info!(plugin = %manifest.name, "loaded ai capability");
                    reg.ai.insert(
                        manifest.name.clone(),
                        LoadedAi { manifest, dir: PathBuf::from(sub).join(&d), prompt, output_check },
                    );
                }
            }
        }
        Ok(reg)
    }

    /// Register a type from an in-memory manifest and schema (used by tests and `load`).
    pub fn insert_type(&mut self, manifest: TypeManifest, schema: &Value, dir: PathBuf) -> Result<(), PluginError> {
        let check = compile(&manifest.name, schema)?;
        self.insert_checked(manifest, check, dir)
    }

    fn insert_checked(&mut self, manifest: TypeManifest, check: Check, dir: PathBuf) -> Result<(), PluginError> {
        if self.types.contains_key(&manifest.name) {
            return Err(PluginError::Duplicate(manifest.name));
        }
        tracing::info!(plugin = %manifest.name, version = manifest.version, "loaded type");
        self.types.insert(manifest.name.clone(), LoadedType { manifest, dir, check });
        Ok(())
    }

    /// Plugins whose schemas were compiled at load time rather than at build time, as
    /// `types/<name>` or `ai/<name>`. The Worker expects this to be empty.
    pub fn runtime_compiled(&self) -> Vec<String> {
        let types = self.types.iter().filter(|(_, t)| t.check.is_runtime()).map(|(n, _)| format!("types/{n}"));
        let ai = self.ai.iter().filter(|(_, a)| a.output_check.is_runtime()).map(|(n, _)| format!("ai/{n}"));
        types.chain(ai).collect()
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
        let errors = t.check.errors(&instance);
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }

    /// Copy the fields a plugin lists in `index` out of the payload. `datetime` fields are
    /// normalised with [`reltime::canonical_json`](cairn_domain::reltime::canonical_json) so
    /// they compare and sort correctly as strings.
    pub fn extract_idx(&self, type_name: &str, payload: &Map<String, Value>) -> Map<String, Value> {
        let Some(t) = self.types.get(type_name) else { return Map::new() };
        t.manifest
            .index
            .iter()
            .filter_map(|(k, kind)| {
                let v = payload.get(k)?;
                let v = if *kind == IndexKind::Datetime { cairn_domain::reltime::canonical_json(v) } else { v.clone() };
                Some((k.clone(), v))
            })
            .collect()
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
    fn idx_datetimes_are_canonical() {
        let r = todo_registry();
        let p = json!({"text": "x", "due": "2026-03-01T09:30:00+01:00"}).as_object().unwrap().clone();
        assert_eq!(r.extract_idx("todo", &p)["due"], "2026-03-01T08:30:00.000Z");
        let p = json!({"text": "x", "due": "next week"}).as_object().unwrap().clone();
        assert_eq!(r.extract_idx("todo", &p)["due"], "next week", "non-datetimes are left for validation");
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
