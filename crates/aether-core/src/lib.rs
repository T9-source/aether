//! # Aether Core
//!
//! The domain core of Aether: **traffic model, pipeline, events**.
//!
//! Architecture invariants (see `docs/adr/`):
//! - **ADR-001**: this crate is UI-independent. The Tauri app, the CLI and CI are
//!   all just clients of the daemon; the daemon is the only state owner.
//! - **ADR-003**: the Traffic Model is protocol-agnostic (h1 + h2 semantics) with
//!   wire truth preserved (ordered, duplicate-capable headers; raw bodies).
//! - **ADR-006**: plugin architecture early, plugin runtime late. Every hook
//!   signature is serialization-shaped (owned, serializable data or capability
//!   handles) so a WASM boundary can be inserted later without semantic changes.
//!
//! ## Dependency discipline (hard invariant)
//!
//! **This crate has zero infrastructure/protocol dependencies (ADR-014).**
//! It must **never** know about TLS, SQLite, WebSocket, Tauri, React — or any
//! HTTP wire codec. What it may use falls into exactly three buckets:
//!
//! 1. *Serialization & type-sharing* — `serde`, `serde_json`, `ts-rs`.
//! 2. *Ergonomics* — `thiserror` (errors), `async-trait` (trait futures).
//! 3. *Computation* — `regex` (serialized rule layer), `base64` (binary
//!    body encoding), and `tokio` **primitives only** (`sync` + `time`:
//!    the broadcast bus and the hook timeout budget — no runtime, no net,
//!    no fs; a test-only runtime lives in `[dev-dependencies]`).
//!
//! If a feature seems to need more than these three buckets, it belongs in
//! another crate — that is the invariant working as intended.

pub mod context;
pub mod error;
pub mod events;
pub mod hooks;
pub mod model;
pub mod pipeline;
pub mod rules;
pub mod storage;

pub use context::{Capabilities, RequestContext, Scope, TrafficSource};
pub use error::{AetherError, AetherResult};
pub use events::{
    AetherEvent, EventBus, EventCategory, EventSubscription, HookStage, InterceptDecision,
    InterceptDirection,
};
pub use hooks::{Hook, HookAction, HookRegistry, HookResult};
pub use model::{
    Body, Flow, FlowId, FlowMeta, Framing, Header, Headers, HttpVersion, Method, Request, Response,
    StatusCode, TransportProto, Url, WsDirection, WsKind, WsMessage, WsSession,
};
pub use pipeline::{Pipeline, PipelineVerdict};
pub use rules::{
    RespondWith, RuleAction, RuleHook, RuleSpec, RuleStage, drain_ctx_tags, take_respond_with,
};
pub use storage::{FlowPage, FlowQuery, FlowStats, FlowSummary, MemoryStorage, Storage};
