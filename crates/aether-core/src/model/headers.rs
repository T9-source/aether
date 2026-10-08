//! Ordered, duplicate-capable header storage — HTTP/1.1 wire truth (ADR-003).
//!
//! Lookup is case-insensitive (HTTP semantics), but original casing and order
//! are preserved for replay and research (header-order matters for request
//! smuggling work). Repeated headers (e.g. `Set-Cookie`) are first-class.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Headers {
    entries: Vec<Header>,
}

impl Headers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (replace the first case-insensitive match **in place**, keeping
    /// its position) or push when absent.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|h| h.name.eq_ignore_ascii_case(&name))
        {
            slot.value = value.into();
        } else {
            self.entries.push(Header {
                name,
                value: value.into(),
            });
        }
    }

    /// Append unconditionally — the correct call for repeatable headers
    /// (`Set-Cookie`, `Via`, …).
    pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.entries.push(Header {
            name: name.into(),
            value: value.into(),
        });
    }

    /// First case-insensitive match.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }

    /// All case-insensitive matches, in order.
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
            .collect()
    }

    /// Remove every case-insensitive match. Returns true if anything was removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|h| !h.name.eq_ignore_ascii_case(name));
        self.entries.len() != before
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Header> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_order_and_duplicates() {
        let mut h = Headers::new();
        h.append("Set-Cookie", "a=1");
        h.append("X-Before", "keep");
        h.append("Set-Cookie", "b=2");
        assert_eq!(h.get_all("set-cookie"), vec!["a=1", "b=2"]);
        let order: Vec<&str> = h.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(order, vec!["Set-Cookie", "X-Before", "Set-Cookie"]);
    }

    #[test]
    fn insert_replaces_in_place_case_insensitively() {
        let mut h = Headers::new();
        h.append("content-type", "text/plain");
        h.append("X-Keep", "1");
        h.insert("CONTENT-TYPE", "application/json");
        assert_eq!(h.get("content-TYPE"), Some("application/json"));
        let order: Vec<&str> = h.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(order, vec!["content-type", "X-Keep"]);
    }

    #[test]
    fn remove_all_matches() {
        let mut h = Headers::new();
        h.append("A", "1");
        h.append("a", "2");
        h.append("B", "3");
        assert!(h.remove("A"));
        assert!(!h.contains("a"));
        assert!(h.contains("B"));
    }
}
