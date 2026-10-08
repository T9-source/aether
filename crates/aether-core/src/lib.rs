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
//! This crate may only depend on runtime/serialization plumbing (`serde`,
//! `tokio`, `async-trait`, `thiserror`, `base64`, `ts-rs`).
//! It must **never** know about `hyper`, TLS, SQLite, WebSocket, Tauri or React.
//! `tokio` here is the async runtime and timeout budget only — zero protocol
//! knowledge lives in this crate.

pub mod context;
pub mod error;
pub mod events;
pub mod hooks;
pub mod model;
pub mod pipeline;

pub use context::{Capabilities, RequestContext, Scope, TrafficSource};
pub use error::{AetherError, AetherResult};
pub use events::{AetherEvent, EventBus, EventSubscription, HookStage};
pub use hooks::{Hook, HookAction, HookRegistry, HookResult};
pub use model::{
    Body, Flow, FlowId, FlowMeta, Header, Headers, HttpVersion, Method, Request, Response,
    StatusCode, TransportProto, Url,
};
pub use pipeline::{Pipeline, PipelineVerdict};
