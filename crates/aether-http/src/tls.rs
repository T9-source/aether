//! Aether — network engine.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! TLS configuration (ADR-004): rustls with the `ring` provider everywhere.
//! - MITM server leg: per-connection config with a **fixed** leaf for the
//!   CONNECT authority — SNI-independent, so clients that skip SNI still work.
//! - Upstream client leg: webpki roots; optional `verify=false` mode
//!   (`NoVerify`) for labs and tests — loudly documented, never a default.

use aether_core::error::AetherResult;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme};
use std::sync::{Arc, OnceLock};

/// Shared crypto provider (ring). rustls builders need an `Arc` of it.
pub fn crypto_provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(ring::default_provider()))
        .clone()
}

/// Resolver pinned to one pre-built leaf+chain — used by the MITM acceptor
/// where the CONNECT authority is already known (no SNI guessing).
#[derive(Debug)]
pub struct FixedResolver(Arc<CertifiedKey>);

impl ResolvesServerCert for FixedResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// Server config for one MITM'd CONNECT tunnel (cheap to build per tunnel:
/// no key generation happens here — the leaf is pre-built and cached).
/// ALPN offers both h2 and h1 (V0.2, ADR-013): the client picks, the
/// dispatch after the handshake routes to the matching leg codec.
pub fn mitm_server_config(leaf: Arc<CertifiedKey>) -> AetherResult<ServerConfig> {
    let mut cfg = ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| aether_core::AetherError::tls(format!("server builder: {e}")))?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(FixedResolver(leaf)));
    cfg.alpn_protocols = alpn_to_vec(ALPN_H2_H1);
    Ok(cfg)
}

/// ALPN lists (V0.2): the client leg offers h2 first, h1 second.
pub const ALPN_H2_H1: &[&[u8]] = &[b"h2", b"http/1.1"];
pub const ALPN_H1: &[&[u8]] = &[b"http/1.1"];

fn alpn_to_vec(alpn: &[&[u8]]) -> Vec<Vec<u8>> {
    alpn.iter().map(|p| p.to_vec()).collect()
}

/// Upstream client config with an explicit ALPN list. h1-only is used for
/// WebSocket upgrades (h2 has no upgrade mechanism on this path); h2+h1 is
/// used for normal exchanges when `upstream_alpn_h2` is enabled (ADR-013).
pub fn upstream_client_config_with_alpn(
    verify: bool,
    alpn: &[&[u8]],
) -> AetherResult<ClientConfig> {
    let versions = ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| aether_core::AetherError::tls(format!("client builder: {e}")))?;
    let mut cfg = if verify {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        versions.with_root_certificates(roots).with_no_client_auth()
    } else {
        tracing::warn!(
            "upstream TLS verification is DISABLED (lab mode) — do not use in production"
        );
        versions
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth()
    };
    cfg.alpn_protocols = alpn_to_vec(alpn);
    Ok(cfg)
}

/// Upstream client config (h1 ALPN only — the pre-V0.2 behavior, kept for
/// WebSocket upgrades and for `upstream_alpn_h2 = false`).
pub fn upstream_client_config(verify: bool) -> AetherResult<ClientConfig> {
    upstream_client_config_with_alpn(verify, ALPN_H1)
}

/// Accept-everything verifier for `verify=false` and integration tests.
/// This is intentional insecurity, scoped to explicit opt-in (ADR-011).
#[derive(Debug)]
pub struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        let provider = crypto_provider();
        provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Best-effort TLS facts recorded on the flow metadata.
#[derive(Debug, Clone)]
pub struct TlsFacts {
    pub version: Option<String>,
    pub cipher: Option<String>,
    pub alpn: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_config_builds_in_both_modes() {
        assert!(upstream_client_config(true).is_ok());
        assert!(upstream_client_config(false).is_ok());
    }

    #[test]
    fn fixed_resolver_returns_the_same_key() {
        // provider + builder smoke test; the resolver is exercised in e2e
        assert!(!crypto_provider().cipher_suites.is_empty());
    }
}
