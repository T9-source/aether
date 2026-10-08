//! WebSocket session records (V0.2, ADR-013).
//!
//! One `WsSession` belongs to exactly one `Flow` — the flow whose h1
//! exchange performed the `Upgrade: websocket` handshake. Frames on the wire
//! are forwarded **untouched** (relay, not proxying); this module records
//! what the relay *saw*, with bounded previews so a chatty socket can never
//! blow up memory or storage (same philosophy as ADR-011 body capture).
//!
//! Recording is best-effort telemetry, not wire truth: the forwarded bytes
//! are always the original ones, masks included.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::unix_millis_now;

/// Which leg a recorded message traveled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WsDirection {
    /// Client → origin (a frame the browser sent).
    ToServer,
    /// Origin → client (a frame the origin sent).
    ToClient,
}

/// RFC 6455 message kind (assembled from continuation frames).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WsKind {
    Text,
    Binary,
    Ping,
    Pong,
    Close,
    /// Reserved / unknown opcodes are recorded as opaque — never rejected.
    Other,
}

impl WsKind {
    pub fn from_opcode(opcode: u8) -> Self {
        match opcode {
            0x1 => Self::Text,
            0x2 => Self::Binary,
            0x8 => Self::Close,
            0x9 => Self::Ping,
            0xA => Self::Pong,
            _ => Self::Other,
        }
    }
}

/// One assembled WebSocket message with a bounded preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WsMessage {
    pub direction: WsDirection,
    pub kind: WsKind,
    /// Full payload size in bytes (recording may keep only a preview).
    pub size: u64,
    /// Preview text when the payload was valid UTF-8 (text messages).
    #[serde(default)]
    pub text: Option<String>,
    /// Preview as base64 for binary payloads (or non-UTF-8 text).
    #[serde(default)]
    pub base64: Option<String>,
    /// True when the preview was cut short by the capture policy.
    pub truncated: bool,
    pub at_unix_ms: u64,
}

/// Bounded record of one WebSocket connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WsSession {
    /// Recorded messages (bounded by `MAX_RECORDED`).
    pub messages: Vec<WsMessage>,
    /// Total messages observed, including ones past the recording cap.
    pub total_messages: u64,
    /// Total payload bytes observed in both directions.
    pub total_bytes: u64,
    /// True when `total_messages` exceeded the recording cap.
    pub truncated: bool,
    pub opened_at_unix_ms: u64,
    pub closed_at_unix_ms: Option<u64>,
}

/// Recording policy (ADR-013): small enough to store inline in the flow
/// row, large enough to debug a real session.
pub const MAX_RECORDED_MESSAGES: usize = 500;
/// Per-message preview cap in bytes (text or base64 payload).
pub const MAX_PREVIEW_BYTES: usize = 2 * 1024;
/// Hard cap on the total preview payload retained per session.
pub const MAX_SESSION_PREVIEW_BYTES: usize = 1024 * 1024;

impl WsSession {
    pub fn opened_now() -> Self {
        Self {
            messages: Vec::new(),
            total_messages: 0,
            total_bytes: 0,
            truncated: false,
            opened_at_unix_ms: unix_millis_now(),
            closed_at_unix_ms: None,
        }
    }

    /// Records one message under the capture policy: `preview` already
    /// carries the (possibly truncated, possibly base64) representation.
    pub fn record(&mut self, mut msg: WsMessage) {
        self.total_messages = self.total_messages.saturating_add(1);
        self.total_bytes = self.total_bytes.saturating_add(msg.size);
        if self.messages.len() >= MAX_RECORDED_MESSAGES
            || self.total_bytes > MAX_SESSION_PREVIEW_BYTES as u64
        {
            self.truncated = true;
            return;
        }
        if msg.text.as_ref().map(|t| t.len()).unwrap_or(0) > MAX_PREVIEW_BYTES {
            msg.text = msg
                .text
                .take()
                .map(|t| truncate_utf8(&t, MAX_PREVIEW_BYTES).to_string());
            msg.truncated = true;
        }
        if msg.base64.as_ref().map(|b| b.len()).unwrap_or(0) > MAX_PREVIEW_BYTES {
            msg.base64 = msg
                .base64
                .take()
                .map(|b| truncate_utf8(&b, MAX_PREVIEW_BYTES).to_string());
            msg.truncated = true;
        }
        self.messages.push(msg);
    }

    pub fn closed_now(&mut self) {
        self.closed_at_unix_ms = Some(unix_millis_now());
    }
}

/// Char-boundary-safe prefix cut (previews may end mid-multibyte-character).
fn truncate_utf8(s: &str, cap: usize) -> &str {
    if s.len() <= cap {
        return s;
    }
    let mut idx = cap;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(direction: WsDirection, size: u64) -> WsMessage {
        WsMessage {
            direction,
            kind: WsKind::Text,
            size,
            text: Some("x".repeat(size as usize)),
            base64: None,
            truncated: false,
            at_unix_ms: 0,
        }
    }

    #[test]
    fn record_respects_message_cap() {
        let mut s = WsSession::opened_now();
        for i in 0..(MAX_RECORDED_MESSAGES + 25) {
            s.record(msg(WsDirection::ToServer, 1));
            let _ = i;
        }
        assert_eq!(s.messages.len(), MAX_RECORDED_MESSAGES);
        assert_eq!(s.total_messages, MAX_RECORDED_MESSAGES as u64 + 25);
        assert!(s.truncated);
    }

    #[test]
    fn previews_are_truncated_to_cap() {
        let mut s = WsSession::opened_now();
        s.record(msg(WsDirection::ToClient, (MAX_PREVIEW_BYTES * 3) as u64));
        let m = &s.messages[0];
        assert!(m.truncated);
        assert_eq!(m.text.as_ref().unwrap().len(), MAX_PREVIEW_BYTES);
    }

    #[test]
    fn opcode_mapping() {
        assert_eq!(WsKind::from_opcode(0x1), WsKind::Text);
        assert_eq!(WsKind::from_opcode(0x2), WsKind::Binary);
        assert_eq!(WsKind::from_opcode(0x8), WsKind::Close);
        assert_eq!(WsKind::from_opcode(0x9), WsKind::Ping);
        assert_eq!(WsKind::from_opcode(0xA), WsKind::Pong);
        assert_eq!(WsKind::from_opcode(0x3), WsKind::Other);
    }

    #[test]
    fn utf8_truncate_never_panics_on_multibyte() {
        let s = "aé𐍈".repeat(2000);
        let cut = truncate_utf8(&s, 1024);
        assert!(cut.len() <= 1024);
        assert!(cut.is_char_boundary(cut.len()));
        assert_eq!(truncate_utf8("short", 1024), "short");
    }
}
