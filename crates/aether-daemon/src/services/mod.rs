//! The behavior layer (ADR-016) — services own behavior, the daemon owns
//! lifecycle, the control plane owns the wire protocol.
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
//!
//! The daemon (`daemon::run`) is composition only: it builds the services
//! below and never implements behavior itself. The control plane
//! (`control.rs`) is protocol machinery — auth, origin checks, WS framing —
//! that *delegates* every operation to a service. Nothing else in the crate
//! may grow behavior: a new capability means a new service method, which
//! keeps the daemon from ever becoming a God object.

pub mod intercept;
pub mod replay;
pub mod rules;
pub mod traffic;

pub use intercept::InterceptService;
pub use replay::ReplayService;
pub use rules::RulesService;
pub use traffic::TrafficService;

use std::sync::Arc;

use aether_http::ca::CaManager;

/// The CA facts a control-plane client may ask for (currently: the PEM used
/// to trust the MITM root — ADR-004).
pub struct CaService {
    ca: Arc<CaManager>,
}

impl CaService {
    pub fn new(ca: Arc<CaManager>) -> Self {
        Self { ca }
    }

    /// The MITM root certificate as PEM (the operator installs it to make
    /// the browser trust locally-issued leaf certificates).
    pub fn export_pem(&self) -> String {
        self.ca.ca_cert_pem()
    }
}

/// Every behavior the daemon exposes, as composable services (ADR-016).
/// Built exactly once by `daemon::run`; shared with the control plane.
pub struct Services {
    pub traffic: TrafficService,
    pub intercept: InterceptService,
    pub rules: RulesService,
    pub replay: ReplayService,
    pub ca: CaService,
}
