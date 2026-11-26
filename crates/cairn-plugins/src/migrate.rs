//! Declarative payload migrations (`migrations.json`). The core is Rust and embeds no JS
//! runtime, so migrations are data: rename / default / drop / map / move.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Migration {
    pub from: u32,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// [old, new]
    Rename(String, String),
    /// [field, value]: set only if absent
    Default(String, Value),
    Drop(String),
    /// [field, {old: new}] for enum-like string values
    Map(String, Map<String, Value>),
    /// [field, into]: nest `field` under object `into`
    Move(String, String),
}

/// Apply all migrations with `from >= current` in order. Returns the new version.
pub fn migrate(payload: &mut Map<String, Value>, current: u32, migrations: &[Migration]) -> u32 {
    let mut version = current;
    let mut ms: Vec<&Migration> = migrations.iter().filter(|m| m.from >= current).collect();
    ms.sort_by_key(|m| m.from);
    for m in ms {
        if m.from != version {
            break; // gap: stop rather than guess
        }
        for step in &m.steps {
            apply_step(payload, step);
        }
        version += 1;
    }
    version
}

fn apply_step(p: &mut Map<String, Value>, step: &Step) {
    match step {
        Step::Rename(old, new) => {
            if let Some(v) = p.remove(old) {
                p.insert(new.clone(), v);
            }
        }
        Step::Default(field, v) => {
            p.entry(field.clone()).or_insert_with(|| v.clone());
        }
        Step::Drop(field) => {
            p.remove(field);
        }
        Step::Map(field, table) => {
            if let Some(Value::String(s)) = p.get(field)
                && let Some(new) = table.get(s)
            {
                p.insert(field.clone(), new.clone());
            }
        }
        Step::Move(field, into) => {
            if let Some(v) = p.remove(field) {
                let target = p.entry(into.clone()).or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(o) = target {
                    o.insert(field.clone(), v);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn migrates_in_order_and_stops_at_gaps() {
        let ms: Vec<Migration> = serde_json::from_value(json!([
            { "from": 1, "steps": [{ "rename": ["text", "body"] }, { "default": ["priority", 0] }] },
            { "from": 2, "steps": [{ "map": ["priority", { "0": "low" }] }] },
            { "from": 4, "steps": [{ "drop": "body" }] }
        ]))
        .unwrap();
        let mut p = json!({ "text": "hi" }).as_object().unwrap().clone();
        let v = migrate(&mut p, 1, &ms);
        assert_eq!(v, 3);
        assert_eq!(p["body"], "hi");
        assert_eq!(p["priority"], 0); // number, not string "0": map only touches strings
    }
}
