//! Scoped bearer tokens (docs/ingest.md § Auth).
//!
//! M0: tokens come from the `CAIRN_TOKENS` Worker secret as sha256 hashes:
//!   CAIRN_TOKENS="phone:capture:<sha256>;ui:read,write:<sha256>;me:admin:<sha256>"
//! M1 moves them to the `api_token` table (`cairn token create`).

use crate::error::ApiError;
use axum::http::HeaderMap;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Capture,
    Read,
    Write,
    Admin,
}

impl Scope {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "capture" => Self::Capture,
            "read" => Self::Read,
            "write" => Self::Write,
            "admin" => Self::Admin,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TokenEntry {
    pub name: String,
    pub scopes: Vec<Scope>,
    pub sha256: String,
}

#[derive(Debug, Clone, Default)]
pub struct TokenSet(Vec<TokenEntry>);

impl TokenSet {
    pub fn from_env_str(raw: &str) -> Self {
        let entries = raw
            .split(';')
            .filter_map(|e| {
                let mut parts = e.trim().splitn(3, ':');
                let name = parts.next()?.to_string();
                let scopes = parts.next()?.split(',').filter_map(Scope::parse).collect();
                let sha256 = parts.next()?.to_lowercase();
                Some(TokenEntry { name, scopes, sha256 })
            })
            .collect();
        Self(entries)
    }

    /// Returns the token name if the bearer token is valid and has `scope`. Admin implies all;
    /// write implies read and capture ("everything except admin", docs/ingest.md).
    pub fn check(&self, headers: &HeaderMap, scope: Scope) -> Result<String, ApiError> {
        let raw = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(ApiError::unauthorized)?;
        let digest = hex::encode(Sha256::digest(raw.as_bytes()));
        let entry = self.0.iter().find(|e| e.sha256 == digest).ok_or_else(ApiError::unauthorized)?;
        let ok = entry.scopes.contains(&scope)
            || entry.scopes.contains(&Scope::Admin)
            || (matches!(scope, Scope::Read | Scope::Capture) && entry.scopes.contains(&Scope::Write));
        if ok { Ok(entry.name.clone()) } else { Err(ApiError::forbidden(scope.as_str())) }
    }
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(tok: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::AUTHORIZATION, format!("Bearer {tok}").parse().unwrap());
        h
    }

    #[test]
    fn capture_token_cannot_read() {
        let set = TokenSet::from_env_str(&format!("phone:capture:{}", sha256_hex("s3cret")));
        assert_eq!(set.check(&headers("s3cret"), Scope::Capture).unwrap(), "phone");
        assert!(set.check(&headers("s3cret"), Scope::Read).is_err());
        assert!(set.check(&headers("wrong"), Scope::Capture).is_err());
    }

    #[test]
    fn write_token_can_read_and_capture_but_not_admin() {
        let set = TokenSet::from_env_str(&format!("ui:write:{}", sha256_hex("w")));
        for s in [Scope::Read, Scope::Capture, Scope::Write] {
            assert!(set.check(&headers("w"), s).is_ok(), "{s:?}");
        }
        assert!(set.check(&headers("w"), Scope::Admin).is_err());
    }
}
