//! Response + status code classification.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AetherError, AetherResult};
use crate::model::fnv::Fnv1a;
use crate::model::request::{HttpVersion, hash_body_tail};
use crate::model::{Body, Headers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatusCode {
    pub code: u16,
}

impl StatusCode {
    pub fn new(code: u16) -> AetherResult<Self> {
        if (100..=599).contains(&code) {
            Ok(Self { code })
        } else {
            Err(AetherError::invalid_input(format!(
                "status code out of range: {code}"
            )))
        }
    }

    pub fn is_informational(&self) -> bool {
        (100..200).contains(&self.code)
    }
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.code)
    }
    pub fn is_redirect(&self) -> bool {
        (300..400).contains(&self.code)
    }
    pub fn is_client_error(&self) -> bool {
        (400..500).contains(&self.code)
    }
    pub fn is_server_error(&self) -> bool {
        (500..600).contains(&self.code)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Response {
    pub status: StatusCode,
    pub version: HttpVersion,
    pub headers: Headers,
    pub body: Body,
}

impl Response {
    pub fn new(status: u16) -> AetherResult<Self> {
        Ok(Self {
            status: StatusCode::new(status)?,
            version: HttpVersion::Http11,
            headers: Headers::new(),
            body: Body::Empty,
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }

    pub fn set_header(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.headers.insert(name, value);
    }

    /// Same contract as [`Request::fingerprint`] — pipeline change detection.
    pub fn fingerprint(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write(&self.status.code.to_le_bytes());
        h.write(&[self.version.tag()]);
        for header in self.headers.iter() {
            h.write(header.name.as_bytes());
            h.write(b"\x00");
            h.write(header.value.as_bytes());
            h.write(b"\x01");
        }
        h.write(&self.body.len().to_le_bytes());
        hash_body_tail(&mut h, self.body.as_bytes());
        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classes() {
        let ok = StatusCode::new(200).unwrap();
        assert!(ok.is_success() && !ok.is_redirect());
        assert!(StatusCode::new(404).unwrap().is_client_error());
        assert!(StatusCode::new(503).unwrap().is_server_error());
        assert!(StatusCode::new(999).is_err());
    }

    #[test]
    fn fingerprint_detects_body_edit() {
        let mut r = Response::new(200).unwrap();
        let base = r.fingerprint();
        r.body = Body::text("data");
        assert_ne!(base, r.fingerprint());
    }
}
