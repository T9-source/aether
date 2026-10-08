//! Body — text/binary aware, with a stable JSON representation.
//!
//! Binary payloads serialize as base64 (JSON-friendly for the control plane,
//! ADR-002). Streaming and disk spooling (ADR-003) arrive with the real proxy
//! in Phase 1; the *shape* below is already the final one.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum Body {
    #[default]
    Empty,
    Text {
        text: String,
    },
    Binary {
        #[serde(with = "base64_bytes")]
        #[ts(type = "string")]
        bytes: Vec<u8>,
    },
}

mod base64_bytes {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        base64::engine::general_purpose::STANDARD
            .encode(v)
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(serde::de::Error::custom)
    }
}

impl Body {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn binary(bytes: Vec<u8>) -> Self {
        Self::Binary { bytes }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Text { text } => text.len(),
            Self::Binary { bytes } => bytes.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Empty => &[],
            Self::Text { text } => text.as_bytes(),
            Self::Binary { bytes } => bytes,
        }
    }

    /// Lossy UTF-8 view for display purposes (never mutates the body).
    pub fn display_text(&self) -> String {
        String::from_utf8_lossy(self.as_bytes()).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_roundtrip() {
        let b = Body::text("hello ✅ world");
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(serde_json::from_str::<Body>(&json).unwrap(), b);
        assert!(json.contains("hello"));
    }

    #[test]
    fn binary_survives_json_as_base64() {
        let b = Body::binary(vec![0x00, 0xFF, 0xDE, 0xAD, 0xBE, 0xEF]);
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains("AP/erb7v"), "{json}");
        assert_eq!(serde_json::from_str::<Body>(&json).unwrap(), b);
    }

    #[test]
    fn empty_body() {
        assert!(Body::Empty.is_empty());
        assert_eq!(Body::Empty.as_bytes(), b"");
    }
}
