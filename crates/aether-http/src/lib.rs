//! Aether — network engine.
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
//!
//! # aether-http
//!
//! The Phase-1 network engine (ADR-003/004/007/009): HTTP/1.1 explicit proxy
//! with TLS MITM, built on `tokio` + `rustls` + `rcgen` — with a **hand-rolled
//! wire codec** because a security tool must own its bytes (hyper is
//! deliberately absent; see `wire`).
//!
//! Layering (hard rule): this crate *consumes* `aether-core` — the domain
//! model, the pipeline and the event bus — and never leaks sockets into it.
//! `aether-store-sqlite` and `aether-daemon` sit above.

pub mod ca;
pub mod engine;
pub mod h2x;
pub mod intercept;
pub mod server;
pub mod tls;
pub mod upstream;
pub mod wire;
pub mod ws;

pub use ca::CaManager;
pub use engine::{ConnInfo, EngineOutcome, FlowEngine};
pub use intercept::{HeldDecision, HeldInfo, HeldPayload, InterceptState};
pub use server::{ProxyCfg, ProxyServer};
pub use upstream::{UpstreamCfg, UpstreamReply, UpstreamStream};
