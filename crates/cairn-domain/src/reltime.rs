//! Relative time expressions used in queries, task inputs and GC policies:
//! `-24h`, `+2d`, `30d` (unsigned = duration), `90m`, `2w`. Resolved against an explicit
//! `now` so dry-runs and tests are deterministic.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum RelTimeError {
    #[error("empty relative time")]
    Empty,
    #[error("invalid relative time `{0}` (expected e.g. -24h, +2d, 30d)")]
    Invalid(String),
}

/// Parse an unsigned or signed span into a `Duration` (sign included).
pub fn parse_span(s: &str) -> Result<Duration, RelTimeError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(RelTimeError::Empty);
    }
    let (sign, rest) = match s.as_bytes()[0] {
        b'-' => (-1, &s[1..]),
        b'+' => (1, &s[1..]),
        _ => (1, s),
    };
    let invalid = || RelTimeError::Invalid(s.to_string());
    let unit = rest.chars().last().ok_or_else(invalid)?;
    let n: i64 = rest[..rest.len() - unit.len_utf8()].parse().map_err(|_| invalid())?;
    let d = match unit {
        'm' => Duration::minutes(n),
        'h' => Duration::hours(n),
        'd' => Duration::days(n),
        'w' => Duration::weeks(n),
        _ => return Err(invalid()),
    };
    Ok(d * sign)
}

/// Resolve `-24h` → now - 24h, `+2d` → now + 2d, or an RFC 3339 timestamp.
pub fn resolve(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, RelTimeError> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    Ok(now + parse_span(s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans() {
        assert_eq!(parse_span("-24h"), Ok(Duration::hours(-24)));
        assert_eq!(parse_span("+2d"), Ok(Duration::days(2)));
        assert_eq!(parse_span("30d"), Ok(Duration::days(30)));
        assert!(parse_span("soon").is_err());
        assert!(parse_span("").is_err());
    }

    #[test]
    fn resolves_against_now() {
        let now = DateTime::parse_from_rfc3339("2026-11-23T09:00:00Z").unwrap().with_timezone(&Utc);
        let t = resolve("-1d", now).unwrap();
        assert_eq!(t.to_rfc3339(), "2026-11-22T09:00:00+00:00");
    }
}

/// Resolve signed relative-time strings inside query values (`"+2d"` → RFC 3339). Other values
/// pass through unchanged. Used by every Store implementation so where-clauses agree.
pub fn resolve_json(v: &serde_json::Value, now: DateTime<Utc>) -> serde_json::Value {
    if let serde_json::Value::String(s) = v {
        if (s.starts_with('+') || s.starts_with('-'))
            && let Ok(d) = parse_span(s)
        {
            return serde_json::Value::String(canonical(now + d));
        }
        return canonical_json(v);
    }
    v.clone()
}

/// The one datetime format stored in `idx` and used in comparisons: UTC, millisecond
/// precision, `Z` suffix, fixed width (`2026-01-01T09:30:00.000Z`). Datetimes are stored as
/// strings (the SurrealDB wire format keeps strings as strings), so a fixed width is what makes
/// lexicographic order match time order, in SurrealDB and in `MemStore` alike.
pub fn canonical(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// [`canonical`] for an RFC 3339 string value; anything else is returned unchanged.
pub fn canonical_json(v: &serde_json::Value) -> serde_json::Value {
    match v.as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()) {
        Some(t) => serde_json::Value::String(canonical(t.with_timezone(&Utc))),
        None => v.clone(),
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;

    #[test]
    fn resolves_only_signed_spans() {
        let now = Utc::now();
        assert!(resolve_json(&serde_json::json!("+2d"), now).as_str().unwrap().contains('T'));
        assert_eq!(resolve_json(&serde_json::json!("2d"), now), serde_json::json!("2d"));
        assert_eq!(resolve_json(&serde_json::json!(3), now), serde_json::json!(3));
        assert_eq!(resolve_json(&serde_json::json!("note:abc"), now), serde_json::json!("note:abc"));
    }

    #[test]
    fn canonical_datetimes_sort_lexicographically() {
        let a = canonical_json(&serde_json::json!("2026-01-01T10:00:00+02:00"));
        let b = canonical_json(&serde_json::json!("2026-01-01T08:00:00.5Z"));
        let c = canonical_json(&serde_json::json!("2026-01-01T08:00:01Z"));
        assert_eq!(a, serde_json::json!("2026-01-01T08:00:00.000Z"));
        assert_eq!(b, serde_json::json!("2026-01-01T08:00:00.500Z"));
        assert!(a.as_str() < b.as_str() && b.as_str() < c.as_str());
        let now = DateTime::parse_from_rfc3339("2026-01-01T08:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(resolve_json(&serde_json::json!("+1h"), now), serde_json::json!("2026-01-01T09:00:00.000Z"));
    }
}
