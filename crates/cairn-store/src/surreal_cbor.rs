//! CBOR wire format for SurrealDB `/rpc`.
//!
//! Why not JSON: SurrealDB parses JSON request bodies with its SurrealQL value parser
//! (`legacy_strands`), so any string that *looks* like a record id (`"note:abc"`) or a datetime
//! becomes one, nested values included. That silently retypes user payloads and raw capture
//! bodies (invariant 1) and breaks `TYPE string` fields. CBOR carries exact types: a CBOR text
//! string is always a SurrealQL string.
//!
//! Requests are plain CBOR built from `serde_json::Value`, so no SurrealDB tags are sent.
//! Responses are decoded back into JSON-shaped values, matching what the JSON format returned:
//! - record ids (tag 8) → `"table:key"`
//! - datetimes (tags 0, 12) → RFC 3339 strings
//! - NONE (tag 6) → `null`
//! - decimals (tag 10) → numbers
//! - UUIDs (tags 9, 37) → hyphenated strings
//! - durations (tags 13, 14) → strings
//! - anything else → its content

use chrono::{DateTime, SecondsFormat};
use ciborium::Value as Cbor;
use serde_json::{Map, Number, Value};

const TAG_DATETIME_ISO: u64 = 0;
const TAG_NONE: u64 = 6;
const TAG_TABLE: u64 = 7;
const TAG_RECORDID: u64 = 8;
const TAG_UUID_STRING: u64 = 9;
const TAG_DECIMAL_STRING: u64 = 10;
const TAG_DATETIME: u64 = 12;
const TAG_DURATION_STRING: u64 = 13;
const TAG_DURATION: u64 = 14;
const TAG_UUID: u64 = 37;

pub(crate) fn encode(v: &Value) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    ciborium::into_writer(&to_cbor(v), &mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Value, String> {
    let v: Cbor = ciborium::from_reader(bytes).map_err(|e| e.to_string())?;
    Ok(from_cbor(v))
}

fn to_cbor(v: &Value) -> Cbor {
    match v {
        Value::Null => Cbor::Null,
        Value::Bool(b) => Cbor::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Cbor::Integer(i.into())
            } else if let Some(u) = n.as_u64() {
                Cbor::Integer(u.into())
            } else {
                Cbor::Float(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        Value::String(s) => Cbor::Text(s.clone()),
        Value::Array(a) => Cbor::Array(a.iter().map(to_cbor).collect()),
        Value::Object(o) => Cbor::Map(o.iter().map(|(k, v)| (Cbor::Text(k.clone()), to_cbor(v))).collect()),
    }
}

fn int(v: &Cbor) -> Option<i128> {
    match v {
        Cbor::Integer(i) => Some(i128::from(*i)),
        _ => None,
    }
}

/// `[seconds, nanos]`, where either may be omitted (empty array = 0).
fn secs_nanos(v: &Cbor) -> (i128, i128) {
    match v {
        Cbor::Array(a) => (a.first().and_then(int).unwrap_or(0), a.get(1).and_then(int).unwrap_or(0)),
        _ => (0, 0),
    }
}

fn key_string(v: Cbor) -> String {
    match from_cbor(v) {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

fn number(v: Option<Number>) -> Value {
    v.map(Value::Number).unwrap_or(Value::Null)
}

fn from_cbor(v: Cbor) -> Value {
    match v {
        Cbor::Null => Value::Null,
        Cbor::Bool(b) => Value::Bool(b),
        Cbor::Integer(i) => {
            let i = i128::from(i);
            match i64::try_from(i) {
                Ok(i) => Value::from(i),
                Err(_) => u64::try_from(i).map(Value::from).unwrap_or_else(|_| Value::String(i.to_string())),
            }
        }
        Cbor::Float(f) => number(Number::from_f64(f)),
        Cbor::Text(s) => Value::String(s),
        Cbor::Bytes(b) => Value::Array(b.into_iter().map(Value::from).collect()),
        Cbor::Array(a) => Value::Array(a.into_iter().map(from_cbor).collect()),
        Cbor::Map(m) => Value::Object(m.into_iter().map(|(k, v)| (key_string(k), from_cbor(v))).collect::<Map<_, _>>()),
        Cbor::Tag(tag, inner) => from_tag(tag, *inner),
        _ => Value::Null,
    }
}

fn from_tag(tag: u64, inner: Cbor) -> Value {
    match tag {
        TAG_NONE => Value::Null,
        TAG_RECORDID => match inner {
            Cbor::Array(mut a) if a.len() == 2 => {
                let key = a.pop().map(key_string).unwrap_or_default();
                let table = a.pop().map(key_string).unwrap_or_default();
                Value::String(format!("{table}:{key}"))
            }
            other => from_cbor(other),
        },
        TAG_DATETIME => {
            let (s, n) = secs_nanos(&inner);
            let dt = i64::try_from(s).ok().zip(u32::try_from(n).ok()).and_then(|(s, n)| DateTime::from_timestamp(s, n));
            match dt {
                Some(dt) => Value::String(dt.to_rfc3339_opts(SecondsFormat::AutoSi, true)),
                None => Value::Null,
            }
        }
        TAG_DURATION => {
            let (s, n) = secs_nanos(&inner);
            Value::String(if n == 0 { format!("{s}s") } else { format!("{s}s{n}ns") })
        }
        TAG_DECIMAL_STRING => match from_cbor(inner) {
            Value::String(s) => match s.parse::<i64>() {
                Ok(i) => Value::from(i),
                Err(_) => {
                    s.parse::<f64>().ok().and_then(Number::from_f64).map(Value::Number).unwrap_or(Value::String(s))
                }
            },
            other => other,
        },
        TAG_UUID => match inner {
            Cbor::Bytes(b) if b.len() == 16 => {
                let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
                Value::String(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
            }
            other => from_cbor(other),
        },
        TAG_DATETIME_ISO | TAG_TABLE | TAG_UUID_STRING | TAG_DURATION_STRING => from_cbor(inner),
        _ => from_cbor(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tag(t: u64, v: Cbor) -> Cbor {
        Cbor::Tag(t, Box::new(v))
    }

    #[test]
    fn strings_stay_strings_on_the_way_out() {
        let v = json!({ "a": "note:abc", "b": "2026-01-01T00:00:00Z", "n": [1, -2, 1.5, null, true] });
        assert_eq!(decode(&encode(&v).unwrap()).unwrap(), v);
    }

    #[test]
    fn decodes_surreal_tags() {
        let m = Cbor::Map(vec![
            (
                Cbor::Text("id".into()),
                tag(TAG_RECORDID, Cbor::Array(vec![Cbor::Text("item".into()), Cbor::Text("01J".into())])),
            ),
            (
                Cbor::Text("at".into()),
                tag(
                    TAG_DATETIME,
                    Cbor::Array(vec![Cbor::Integer(1_767_225_600.into()), Cbor::Integer(5_000_000.into())]),
                ),
            ),
            (Cbor::Text("epoch".into()), tag(TAG_DATETIME, Cbor::Array(vec![]))),
            (Cbor::Text("gone".into()), tag(TAG_NONE, Cbor::Null)),
            (Cbor::Text("dec".into()), tag(TAG_DECIMAL_STRING, Cbor::Text("1.25".into()))),
            (Cbor::Text("u".into()), tag(TAG_UUID, Cbor::Bytes((0u8..16).collect()))),
            (Cbor::Text("d".into()), tag(TAG_DURATION, Cbor::Array(vec![Cbor::Integer(90.into())]))),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&m, &mut bytes).unwrap();
        assert_eq!(
            decode(&bytes).unwrap(),
            json!({
                "id": "item:01J",
                "at": "2026-01-01T00:00:00.005Z",
                "epoch": "1970-01-01T00:00:00Z",
                "gone": null,
                "dec": 1.25,
                "u": "00010203-0405-0607-0809-0a0b0c0d0e0f",
                "d": "90s",
            })
        );
    }
}
