//! Minimal, dependency-free URL — owned by the Traffic Model so query-param
//! order and authority semantics stay under our control (security tooling
//! cares about exact wire representation; the `url` crate normalizes more
//! than we are willing to accept).

use serde::{Deserialize, Serialize};
use std::fmt;
use ts_rs::TS;

use crate::error::{AetherError, AetherResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    /// Always starts with `/` after parsing (h2 `:path` equivalent incl. query
    /// is available via [`Url::path_and_query`]).
    pub path: String,
    pub query: Option<String>,
    pub fragment: Option<String>,
}

impl Url {
    pub fn parse(input: &str) -> AetherResult<Self> {
        let input = input.trim();
        let (scheme, rest) = input
            .split_once("://")
            .ok_or_else(|| AetherError::invalid_input(format!("missing scheme in url: {input}")))?;
        if scheme.is_empty()
            || !scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        {
            return Err(AetherError::invalid_input(format!("bad scheme: {scheme}")));
        }

        // authority runs until the first of '/', '?' or '#'
        let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..auth_end];
        let remainder = &rest[auth_end..];
        if authority.is_empty() {
            return Err(AetherError::invalid_input(format!(
                "missing host in url: {input}"
            )));
        }

        // userinfo is out of scope for Phase 0 model; reject to be explicit.
        if authority.contains('@') {
            return Err(AetherError::unsupported(
                "userinfo in URL is not supported by the traffic model yet",
            ));
        }

        let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
            // IPv6 literal
            let (host, port) = rest
                .split_once(']')
                .ok_or_else(|| AetherError::invalid_input("unterminated IPv6 literal"))?;
            let host = format!("[{host}]");
            let port = match port.strip_prefix(':') {
                Some(p) if !p.is_empty() => Some(
                    p.parse::<u16>()
                        .map_err(|_| AetherError::invalid_input(format!("bad port: {p}")))?,
                ),
                Some(_) => return Err(AetherError::invalid_input("empty port")),
                None => None,
            };
            (host, port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) if !p.is_empty() => (
                    h.to_string(),
                    Some(
                        p.parse::<u16>()
                            .map_err(|_| AetherError::invalid_input(format!("bad port: {p}")))?,
                    ),
                ),
                Some((_, "")) => return Err(AetherError::invalid_input("empty port")),
                _ => (authority.to_string(), None),
            }
        };

        let (before_frag, fragment) = match remainder.split_once('#') {
            Some((a, f)) => (a, Some(f.to_string())),
            None => (remainder, None),
        };
        let (path, query) = match before_frag.split_once('?') {
            Some((p, q)) => (p, Some(q.to_string())),
            None => (before_frag, None),
        };
        let path = if path.is_empty() { "/" } else { path };

        Ok(Self {
            scheme: scheme.to_ascii_lowercase(),
            host,
            port,
            path: path.to_string(),
            query,
            fragment,
        })
    }

    /// `host[:port]` with IPv6 brackets handled; default ports are elided.
    pub fn authority(&self) -> String {
        match self.port {
            Some(p) if Some(p) != self.default_port() => format!("{}:{p}", self.host),
            _ => self.host.clone(),
        }
    }

    pub fn default_port(&self) -> Option<u16> {
        match self.scheme.as_str() {
            "http" | "ws" => Some(80),
            "https" | "wss" => Some(443),
            _ => None,
        }
    }

    pub fn effective_port(&self) -> u16 {
        self.port.or(self.default_port()).unwrap_or(80)
    }

    /// h2-style `:path` — path plus query string.
    pub fn path_and_query(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{q}", self.path),
            None => self.path.clone(),
        }
    }

    /// Reassembled absolute URL.
    pub fn as_url(&self) -> String {
        let mut s = format!("{}://{}", self.scheme, self.authority());
        s.push_str(&self.path_and_query());
        if let Some(f) = &self.fragment {
            s.push('#');
            s.push_str(f);
        }
        s
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_url())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_url() {
        let u = Url::parse("https://api.example.com/v1/users/42?id=7#top").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.host, "api.example.com");
        assert_eq!(u.port, None);
        assert_eq!(u.path, "/v1/users/42");
        assert_eq!(u.query.as_deref(), Some("id=7"));
        assert_eq!(u.fragment.as_deref(), Some("top"));
        assert_eq!(u.effective_port(), 443);
        assert_eq!(u.authority(), "api.example.com");
        assert_eq!(u.as_url(), "https://api.example.com/v1/users/42?id=7#top");
    }

    #[test]
    fn explicit_port_is_kept_and_displayed() {
        let u = Url::parse("http://localhost:8080/x").unwrap();
        assert_eq!(u.port, Some(8080));
        assert_eq!(u.authority(), "localhost:8080");
        assert_eq!(u.effective_port(), 8080);
    }

    #[test]
    fn default_port_is_elided_in_authority() {
        let u = Url::parse("https://example.com:443/").unwrap();
        assert_eq!(u.port, Some(443));
        assert_eq!(u.authority(), "example.com");
    }

    #[test]
    fn ipv6_literals_roundtrip() {
        let u = Url::parse("http://[::1]:8080/p").unwrap();
        assert_eq!(u.host, "[::1]");
        assert_eq!(u.authority(), "[::1]:8080");
    }

    #[test]
    fn empty_path_becomes_root() {
        let u = Url::parse("https://example.com").unwrap();
        assert_eq!(u.path, "/");
        assert_eq!(u.path_and_query(), "/");
    }

    #[test]
    fn rejects_garbage() {
        assert!(Url::parse("example.com/x").is_err());
        assert!(Url::parse("https:///x").is_err());
        assert!(Url::parse("https://user@example.com/").is_err());
        assert!(Url::parse("https://host:99999/").is_err());
    }

    #[test]
    fn query_order_is_preserved() {
        let u = Url::parse("https://a.com/?z=1&b=2&a=3").unwrap();
        assert_eq!(u.query.as_deref(), Some("z=1&b=2&a=3"));
    }
}
