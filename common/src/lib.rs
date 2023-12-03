use serde::Deserializer;
use std::fmt::{Debug, Display};
use std::str::FromStr;
use ulid::Ulid;

pub mod frame;

#[cfg(feature = "full")]
pub mod codec;
#[cfg(feature = "full")]
pub mod framed;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocId(pub u128);

impl serde::Serialize for DocId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for DocId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        DocId::from_str(&s).map_err(serde::de::Error::custom)
    }
}

impl FromStr for DocId {
    type Err = InvalidDocId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ulid::from_str(&s)
            .map(|ulid| Self(ulid.0))
            .map_err(|_| InvalidDocId)
    }
}

impl Debug for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DocId").field(&self.to_string()).finish()
    }
}

impl Display for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ulid = Ulid(self.0);
        Display::fmt(&ulid, f)
    }
}

#[derive(Debug)]
pub struct InvalidDocId;

impl Display for InvalidDocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid doc id")
    }
}

impl std::error::Error for InvalidDocId {}
