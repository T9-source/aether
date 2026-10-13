//! Aether — WebSocket relay codec (V0.2, ADR-013).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! RFC 6455 framing for the inspection relay. The relay is *not* a proxy in
//! the h1 sense: frames cross the connection **byte-for-byte untouched**
//! (masks included); this module only *reads* the stream to assemble
//! messages into the bounded `WsSession` record and to emit events.
//!
//! Wire-truth notes:
//! - client→server frames are masked (RFC 6455 §5.3); the relay never
//!   re-frames, it XORs a *copy* of the payload for the preview only,
//! - RSV bits are legal here: the handshake was forwarded end-to-end, so an
//!   extension like permessage-deflate may be active — previews of compressed
//!   payloads are recorded as opaque bytes (unless the daemon stripped the
//!   extension from the handshake, which is the default),
//! - one hard limit exists (`MAX_FRAME_BYTES`) to make memory exhaustion by a
//!   hostile peer impossible; beyond it the relay closes the connection —
//!   that is a relay safety policy, not a protocol judgment.

use aether_core::model::{WsDirection, WsKind, WsMessage, WsSession, unix_millis_now};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Relay safety cap: the largest frame payload the relay will buffer at all.
pub const MAX_FRAME_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub fin: bool,
    /// Raw RSV1-3 bits (extensions — forwarded untouched, never judged).
    pub rsv: u8,
    pub opcode: u8,
    pub masked: bool,
    pub mask_key: [u8; 4],
    pub payload_len: u64,
    /// Bytes consumed by the header itself (2 + ext-len + mask).
    pub header_len: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RawFrame {
    pub header: FrameHeader,
    /// Exact wire bytes: header + payload, ready to be re-emitted verbatim.
    pub bytes: Vec<u8>,
}

impl RawFrame {
    pub fn payload(&self) -> &[u8] {
        &self.bytes[self.header.header_len..]
    }

    /// Unmasked copy of the payload (for previews only — never for forwarding).
    pub fn unmasked_payload(&self) -> Vec<u8> {
        if !self.header.masked {
            return self.payload().to_vec();
        }
        xor_mask(self.payload(), &self.header.mask_key)
    }
}

pub fn xor_mask(payload: &[u8], key: &[u8; 4]) -> Vec<u8> {
    payload
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % 4])
        .collect()
}

/// Frame-parsing error. `Eof` is a clean peer close, anything else is a
/// broken connection (the relay tears the socket down either way).
#[derive(Debug)]
pub enum FrameError {
    Eof,
    Io(std::io::Error),
    Protocol(&'static str),
    TooLarge(u64),
}

impl From<std::io::Error> for FrameError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            FrameError::Eof
        } else {
            FrameError::Io(e)
        }
    }
}

/// Reads exactly one frame (header + payload) from the reader.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<RawFrame, FrameError> {
    let mut fixed = [0u8; 2];
    let n = reader.read(&mut fixed).await?;
    if n == 0 {
        return Err(FrameError::Eof);
    }
    if n < 2 {
        // a torn header — read the second byte or fail
        reader
            .read_exact(&mut fixed[1..])
            .await
            .map_err(|_| FrameError::Eof)?;
    }
    let fin = fixed[0] & 0x80 != 0;
    let rsv = (fixed[0] & 0x70) >> 4;
    let opcode = fixed[0] & 0x0F;
    let masked = fixed[1] & 0x80 != 0;
    let len7 = (fixed[1] & 0x7F) as u64;

    let mut bytes = vec![fixed[0], fixed[1]];
    let payload_len = match len7 {
        126 => {
            let mut ext = [0u8; 2];
            reader.read_exact(&mut ext).await?;
            bytes.extend_from_slice(&ext);
            u16::from_be_bytes(ext) as u64
        }
        127 => {
            let mut ext = [0u8; 8];
            reader.read_exact(&mut ext).await?;
            bytes.extend_from_slice(&ext);
            u64::from_be_bytes(ext)
        }
        n => n,
    };
    let mask_key = if masked {
        let mut key = [0u8; 4];
        reader.read_exact(&mut key).await?;
        bytes.extend_from_slice(&key);
        key
    } else {
        [0u8; 4]
    };

    if payload_len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(payload_len));
    }

    let header_len = bytes.len();
    let mut payload = vec![0u8; payload_len as usize];
    reader.read_exact(&mut payload).await?;
    bytes.extend_from_slice(&payload);

    Ok(RawFrame {
        bytes,
        header: FrameHeader {
            fin,
            rsv,
            opcode,
            masked,
            mask_key,
            payload_len,
            header_len,
        },
    })
}

/// Builds a frame (test + tooling helper — the relay itself never re-frames).
pub fn build_frame(fin: bool, opcode: u8, mask: Option<[u8; 4]>, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push((if fin { 0x80 } else { 0 }) | (opcode & 0x0F));
    let mask_bit = if mask.is_some() { 0x80u8 } else { 0 };
    match payload.len() {
        n if n < 126 => out.push(mask_bit | n as u8),
        n if n <= u16::MAX as usize => {
            out.push(mask_bit | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(mask_bit | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    if let Some(key) = mask {
        out.extend_from_slice(&key);
        out.extend(xor_mask(payload, &key));
    } else {
        out.extend_from_slice(payload);
    }
    out
}

/// Assembles fragmented messages (opcode ≠ 0 starts, 0 continues, FIN ends).
/// Control frames (ping/pong/close) are complete by definition and pass
/// straight through. Oversized messages keep their size counted but their
/// payload is dropped at the cap — the relay still forwards every byte.
pub struct MessageAssembler {
    kind: Option<WsKind>,
    payload: Vec<u8>,
    cap: usize,
}

/// Hard cap for one assembled message kept in the *record* (forwarding is
/// unaffected — it never buffers more than one frame).
pub const MAX_RECORDED_MESSAGE_BYTES: usize = 256 * 1024;

impl MessageAssembler {
    pub fn new() -> Self {
        Self {
            kind: None,
            payload: Vec::new(),
            cap: MAX_RECORDED_MESSAGE_BYTES,
        }
    }

    /// Feeds one frame; returns `Some((kind, payload))` when a message is complete.
    pub fn feed(&mut self, frame: &RawFrame) -> Option<(WsKind, Vec<u8>)> {
        let opcode = frame.header.opcode;
        let payload = frame.unmasked_payload();
        if opcode >= 0x8 {
            // control frame — never fragmented per RFC 6455 §5.4
            return Some((WsKind::from_opcode(opcode), payload));
        }
        match (opcode, self.kind) {
            (0x1 | 0x2, None) => {
                self.kind = Some(WsKind::from_opcode(opcode));
                self.payload = if payload.len() > self.cap {
                    payload[..self.cap].to_vec()
                } else {
                    payload
                };
            }
            (0x1 | 0x2, Some(_)) => {
                // a new message while one is open: protocol violation — be
                // forgiving toward the *record* and start fresh
                self.kind = Some(WsKind::from_opcode(opcode));
                self.payload = if payload.len() > self.cap {
                    payload[..self.cap].to_vec()
                } else {
                    payload
                };
            }
            (0x0, None) => {
                // stray continuation — start an opaque message
                self.kind = Some(WsKind::Other);
                self.payload = if payload.len() > self.cap {
                    payload[..self.cap].to_vec()
                } else {
                    payload
                };
            }
            (0x0, Some(_)) if self.payload.len() < self.cap => {
                let room = self.cap - self.payload.len();
                self.payload
                    .extend_from_slice(&payload[..payload.len().min(room)]);
            }
            (0x0, Some(_)) => {}
            _ => {}
        }
        if !frame.header.fin {
            return None;
        }
        self.kind
            .take()
            .map(|k| (k, std::mem::take(&mut self.payload)))
    }
}

impl Default for MessageAssembler {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared session recorder — the two relay legs publish into one `WsSession`.
#[derive(Clone)]
pub struct WsRecorder {
    session: Arc<Mutex<WsSession>>,
    flow_id: u64,
    bus: aether_core::EventBus,
}

impl WsRecorder {
    pub fn new(flow_id: u64, bus: aether_core::EventBus) -> Self {
        Self {
            session: Arc::new(Mutex::new(WsSession::opened_now())),
            flow_id,
            bus,
        }
    }
    pub fn record(&self, direction: WsDirection, kind: WsKind, payload: &[u8]) {
        use base64::Engine as _;
        let size = payload.len() as u64;
        let (text, base64) = match std::str::from_utf8(payload) {
            Ok(t) => (Some(t.to_string()), None),
            Err(_) => (
                None,
                Some(base64::engine::general_purpose::STANDARD.encode(payload)),
            ),
        };
        let msg = WsMessage {
            direction,
            kind,
            size,
            text,
            base64,
            truncated: false,
            at_unix_ms: unix_millis_now(),
        };
        self.bus.publish(aether_core::AetherEvent::WsMessage {
            flow_id: self.flow_id,
            direction,
            kind,
            size,
        });
        self.session.lock().expect("ws session lock").record(msg);
    }

    pub fn finish(&self) -> WsSession {
        let mut guard = self.session.lock().expect("ws session lock");
        guard.closed_now();
        guard.clone()
    }
}

// ---------------------------------------------------------------------------
// the relay leg — one direction of the duplex copy
// ---------------------------------------------------------------------------

/// Copies one direction of a WebSocket connection: reads frames from
/// `input`, re-emits the **exact wire bytes** to `output`, and records the
/// assembled messages. Returns when the peer closes cleanly (EOF) or on the
/// first hard error — the caller owns tearing down the sibling leg.
pub async fn relay_leg<R, W>(
    mut input: R,
    mut output: W,
    direction: WsDirection,
    recorder: &WsRecorder,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let mut assembler = MessageAssembler::new();
    loop {
        let frame = match read_frame(&mut input).await {
            Ok(f) => f,
            Err(FrameError::Eof) => return Ok(()),
            Err(FrameError::Io(e)) => return Err(e),
            Err(FrameError::Protocol(m)) => {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, m));
            }
            Err(FrameError::TooLarge(n)) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("ws frame of {n} bytes exceeds relay cap {MAX_FRAME_BYTES}"),
                ));
            }
        };
        // forward untouched FIRST — inspection must never delay or alter traffic
        // flush() is load-bearing here: write_all only guarantees the bytes
        // reached the writer's *internal* buffer (e.g. rustls' encrypted
        // queue). Under TCP backpressure (a peer that is busy sending while
        // we are writing) the tail would otherwise sit there unflushed while
        // this loop blocks on the next read — the stream freezes until the
        // next client-bound frame, which may never come.
        if output.write_all(&frame.bytes).await.is_err() || output.flush().await.is_err() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "ws relay sink closed",
            ));
        }
        if let Some((kind, payload)) = assembler.feed(&frame) {
            recorder.record(direction, kind, &payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_core::EventBus;
    use tokio::io::duplex;

    fn frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        build_frame(fin, opcode, None, payload)
    }

    #[tokio::test]
    async fn parses_small_masked_frame() {
        let raw = build_frame(true, 0x1, Some([0x01, 0x02, 0x03, 0x04]), b"Hi");
        let mut r = raw.as_slice();
        let f = read_frame(&mut r).await.unwrap();
        assert!(f.header.fin);
        assert_eq!(f.header.opcode, 0x1);
        assert!(f.header.masked);
        assert_eq!(f.payload().len(), 2);
        assert_eq!(f.unmasked_payload(), b"Hi");
        assert_eq!(f.bytes, raw, "wire bytes preserved");
    }

    #[tokio::test]
    async fn parses_16bit_and_64bit_lengths() {
        let payload = vec![0xABu8; 300];
        let raw = frame(true, 0x2, &payload);
        let f = read_frame(&mut raw.as_slice()).await.unwrap();
        assert_eq!(f.header.payload_len, 300);
        assert_eq!(f.unmasked_payload(), payload);

        let big = vec![0xCDu8; 70_000];
        let raw = frame(true, 0x2, &big);
        let f = read_frame(&mut raw.as_slice()).await.unwrap();
        assert_eq!(f.header.payload_len, 70_000);
        assert_eq!(f.unmasked_payload(), big);
    }

    #[tokio::test]
    async fn eof_is_clean() {
        let mut r: &[u8] = &[];
        assert!(matches!(read_frame(&mut r).await, Err(FrameError::Eof)));
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected() {
        let mut raw = frame(true, 0x2, &[]);
        raw[1] = 127; // 64-bit length marker
        raw.extend_from_slice(&((MAX_FRAME_BYTES + 1u64).to_be_bytes()));
        let err = read_frame(&mut raw.as_slice()).await.unwrap_err();
        assert!(matches!(err, FrameError::TooLarge(_)));
    }

    #[test]
    fn assembler_reassembles_fragments() {
        let mut asm = MessageAssembler::new();
        assert!(asm.feed(&mk(false, 0x1, b"Hel")).is_none());
        assert!(asm.feed(&mk(false, 0x0, b"lo ")).is_none());
        let (kind, payload) = asm.feed(&mk(true, 0x0, b"world")).unwrap();
        assert_eq!(kind, WsKind::Text);
        assert_eq!(payload, b"Hello world");
        // control frames pass through immediately
        let (kind, p) = asm.feed(&mk(true, 0x9, b"p")).unwrap();
        assert_eq!(kind, WsKind::Ping);
        assert_eq!(p, b"p");
    }

    #[test]
    fn assembler_caps_recorded_payload_but_counts_size() {
        let mut asm = MessageAssembler::new();
        asm.cap = 8;
        let (kind, payload) = asm.feed(&mk(true, 0x2, &[0u8; 100])).unwrap();
        assert_eq!(kind, WsKind::Binary);
        assert_eq!(payload.len(), 8, "record capped");
    }

    /// Builds an unmasked RawFrame directly (sync tests — no parser needed).
    fn mk(fin: bool, opcode: u8, payload: &[u8]) -> RawFrame {
        let mut bytes = Vec::with_capacity(payload.len() + 2);
        bytes.push((if fin { 0x80 } else { 0 }) | opcode);
        bytes.push(payload.len() as u8);
        bytes.extend_from_slice(payload);
        RawFrame {
            header: FrameHeader {
                fin,
                rsv: 0,
                opcode,
                masked: false,
                mask_key: [0; 4],
                payload_len: payload.len() as u64,
                header_len: 2,
            },
            bytes,
        }
    }

    #[tokio::test]
    async fn recorder_counts_and_caps() {
        let bus = EventBus::new(4096);
        let rec = WsRecorder::new(9, bus.clone());
        let mut events = bus.subscribe();
        for i in 0..600u32 {
            let text = format!("m{i}");
            rec.record(WsDirection::ToServer, WsKind::Text, text.as_bytes());
        }
        let s = rec.finish();
        assert_eq!(s.total_messages, 600);
        assert_eq!(
            s.messages.len(),
            aether_core::model::ws::MAX_RECORDED_MESSAGES
        );
        assert!(s.truncated);
        assert!(s.closed_at_unix_ms.is_some());
        let mut seen = 0;
        while let Some(e) = events.try_next() {
            if matches!(e, aether_core::AetherEvent::WsMessage { .. }) {
                seen += 1;
            }
        }
        assert_eq!(seen, 600, "events flow even past the recording cap");
    }

    #[tokio::test]
    async fn base64_roundtrip_matches_known_vectors() {
        use base64::Engine as _;
        let enc = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        assert_eq!(enc(b""), "");
        assert_eq!(enc(b"f"), "Zg==");
        assert_eq!(enc(b"fo"), "Zm8=");
        assert_eq!(enc(b"foo"), "Zm9v");
        assert_eq!(enc(b"foob"), "Zm9vYg==");
        assert_eq!(enc(b"fooba"), "Zm9vYmE=");
        assert_eq!(enc(b"foobar"), "Zm9vYmFy");
    }

    #[tokio::test]
    async fn relay_leg_flushes_after_every_forwarded_frame() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        /// Writer with internal buffering, mirroring tokio-rustls: poll_write
        /// accepts every byte (Ready) without promising it reached the wire.
        struct FlushSpy {
            writes: usize,
            last_op_was_flush: bool,
        }

        impl tokio::io::AsyncWrite for FlushSpy {
            fn poll_write(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                self.writes += 1;
                self.last_op_was_flush = false;
                Poll::Ready(Ok(buf.len()))
            }

            fn poll_flush(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                self.last_op_was_flush = true;
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        // Regression: without an explicit flush() after write_all, a relay
        // writer with an internal queue (tokio-rustls' encrypted buffer)
        // keeps the tail of a backpressured write off the wire while the
        // loop blocks on the next read — the stream freezes until the next
        // client-bound frame, which may never come. This is exactly how the
        // megabyte WS echo in v02_e2e used to hang ~50% of runs (flush was
        // missing; fixed in relay_leg).
        let bus = EventBus::new(8);
        let rec = WsRecorder::new(1, bus);
        let mut spy = FlushSpy {
            writes: 0,
            last_op_was_flush: false,
        };
        let wire = frame(true, 0x1, b"hello");
        let mut input = wire.as_slice();
        relay_leg(&mut input, &mut spy, WsDirection::ToServer, &rec)
            .await
            .unwrap(); // EOF after the frame -> clean return
        assert_eq!(spy.writes, 1);
        assert!(
            spy.last_op_was_flush,
            "relay_leg must flush() after every forwarded frame — writers with \
             internal buffers keep unsent bytes otherwise (backpressure freeze)"
        );
    }

    #[tokio::test]
    async fn relay_leg_forwards_bytes_and_records_messages() {
        use tokio::io::AsyncWriteExt;
        let (mut peer_tx, relay_in) = duplex(4096);
        let (sink_tx, sink_rx) = duplex(4096);

        let bus = EventBus::new(64);
        let rec = WsRecorder::new(3, bus);
        let rec2 = rec.clone();
        let relay = tokio::spawn(async move {
            relay_leg(relay_in, sink_tx, WsDirection::ToServer, &rec2).await
        });

        let wire = [
            frame(true, 0x1, b"hello").as_slice(),
            build_frame(true, 0x2, Some([9, 9, 9, 9]), &[0xFF, 0xFE, 0x80]).as_slice(),
            frame(false, 0x1, b"frag").as_slice(),
            frame(true, 0x0, b"ment").as_slice(),
            frame(true, 0x9, b"ping").as_slice(),
        ]
        .concat();
        peer_tx.write_all(&wire).await.unwrap();
        drop(peer_tx); // EOF -> relay returns

        let mut forwarded = Vec::new();
        sink_rx
            .take(wire.len() as u64)
            .read_to_end(&mut forwarded)
            .await
            .unwrap();
        assert_eq!(forwarded, wire, "byte-for-byte relay");
        relay.await.unwrap().unwrap();

        let session = rec.finish();
        assert_eq!(session.total_messages, 4);
        let kinds: Vec<WsKind> = session.messages.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            vec![WsKind::Text, WsKind::Binary, WsKind::Text, WsKind::Ping]
        );
        assert_eq!(session.messages[2].text.as_deref(), Some("fragment"));
        assert_eq!(session.messages[0].text.as_deref(), Some("hello"));
        assert_eq!(session.messages[1].base64.as_deref(), Some("//6A")); // original [FF,FE,80]
    }
}
