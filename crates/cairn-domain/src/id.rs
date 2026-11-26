use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// ULID identifier used for every record (items, edges, views, ops, captures).
/// Serialized as the 26-char Crockford base32 string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(pub ulid::Ulid);

impl Id {
    pub fn new() -> Self {
        Self(ulid::Ulid::generate())
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Id {
    type Err = ulid::DecodeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ulid::Ulid::from_string(s).map(Self)
    }
}
