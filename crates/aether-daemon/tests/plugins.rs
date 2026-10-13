//! V0.3 plugin wiring, end to end (ADR-019/020).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Real daemon, real proxy, real wasm plugin:
//! - an enabled hello plugin stamps `x-aether-hello` on every proxied
//!   request — observed here through the wire, not through mocks;
//! - an enabled plugin that cannot load refuses daemon startup (fail-closed);
//! - the `[[plugins]]` config section parses with the same fail-loud
//!   policies as every other section.

use aether_core::error::AetherError;
use aether_daemon::config::AetherConfig;
use aether_daemon::{DaemonOptions, run as daemon_run};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

fn plugin_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/plugins")
        .join(name)
}

fn require_artifact(name: &str) {
    let wasm = plugin_path(name).join("plugin.wasm");
    assert!(
        wasm.exists(),
        "missing {wasm:?} — run scripts/build-spike-plugins.sh first"
    );
}

fn base_config() -> AetherConfig {
    AetherConfig {
        proxy: aether_daemon::config::ProxySection {
            listen: "127.0.0.1:0".into(),
        },
        control: aether_daemon::config::ControlSection {
            listen: "127.0.0.1:0".into(),
            token: None,
        },
        storage: aether_daemon::config::StorageSection {
            backend: "memory".into(),
            path: None,
        },
        intercept: aether_daemon::config::InterceptSection {
            request: false,
            response: false,
        },
        passthrough: vec![],
        host_overrides: Default::default(),
        upstream_tls_verify: false,
        upstream_alpn_h2: true,
        capture_max_body: 8 * 1024 * 1024,
        plugins: vec![],
    }
}

/// Origin that echoes the plugin's stamp back as `x-echo-hello` — so the
/// assertion sees the header the ORIGIN received, proving the edit happened
/// on the request leg before upstream dial.
async fn spawn_echo_origin() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&buf[..n]);
                let hello = text
                    .lines()
                    .skip(1)
                    .take_while(|l| !l.is_empty())
                    .find(|l| l.to_ascii_lowercase().starts_with("x-aether-hello:"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|v| v.trim().to_string())
                    .unwrap_or_else(|| "absent".into());
                let body = format!("echo:{hello}");
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn daemon_with_hello_plugin_stamps_proxied_traffic() {
    require_artifact("hello");
    let tmp = tempfile::tempdir().unwrap();
    let (tx, rx) = watch::channel(false);
    let origin = spawn_echo_origin().await;

    let mut cfg = base_config();
    cfg.plugins = vec![aether_plugins::PluginEntry {
        path: plugin_path("hello"),
        enabled: true,
    }];
    cfg.host_overrides
        .insert("plugin-test.example".into(), format!("socket://{origin}"));

    let info = daemon_run(DaemonOptions {
        config: cfg,
        memory: true,
        data_dir: Some(tmp.path().to_path_buf()),
        shutdown: rx,
    })
    .await
    .expect("daemon starts with a loadable plugin");
    assert_eq!(info.plugins_loaded, 1);

    // Plain-HTTP explicit-proxy GET in absolute form.
    let mut stream = tokio::net::TcpStream::connect(info.proxy_addr)
        .await
        .unwrap();
    let request = "GET http://plugin-test.example/ HTTP/1.1\r\nhost: plugin-test.example\r\nconnection: close\r\n\r\n";
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
        .await
        .expect("response within budget")
        .unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.contains("echo:from-wasm"),
        "plugin stamp must reach the origin end-to-end; got: {text}"
    );

    let _ = tx.send(true);
}

#[tokio::test]
async fn daemon_refuses_startup_when_enabled_plugin_cannot_load() {
    let tmp = tempfile::tempdir().unwrap();
    let (tx, rx) = watch::channel(false);

    let mut cfg = base_config();
    cfg.plugins = vec![aether_plugins::PluginEntry {
        path: plugin_path("does-not-exist"),
        enabled: true,
    }];

    let err = daemon_run(DaemonOptions {
        config: cfg,
        memory: true,
        data_dir: Some(tmp.path().to_path_buf()),
        shutdown: rx,
    })
    .await
    .expect_err("fail-closed: enabled broken plugin must abort startup");
    match err {
        AetherError::Config(msg) => assert!(msg.contains("refusing to start"), "unexpected: {msg}"),
        other => panic!("expected Config error, got {other:?}"),
    }
    let _ = tx.send(true);
}

#[tokio::test]
async fn config_parses_plugins_section_with_fail_loud_policies() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("aether.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[proxy]\nlisten = \"127.0.0.1:0\"\n\n[[plugins]]\npath = \"plugins/hello\"\nenabled = false\n\n[[plugins]]\npath = \"{}\"\n",
            plugin_path("hello").display()
        ),
    )
    .unwrap();

    let cfg = AetherConfig::load(&cfg_path).unwrap();
    assert_eq!(cfg.plugins.len(), 2);
    // relative path resolved against the config file's directory
    assert_eq!(
        cfg.plugins[0].path,
        tmp.path().join("plugins/hello"),
        "relative plugin paths anchor to the config file, never the CWD"
    );
    assert!(!cfg.plugins[0].enabled);
    // enabled defaults to true when omitted
    assert!(cfg.plugins[1].enabled);

    // unknown key inside [[plugins]] rejected (same policy as sections)
    let bad = tmp.path().join("bad.toml");
    std::fs::write(
        &bad,
        "[proxy]\nlisten = \"127.0.0.1:0\"\n\n[[plugins]]\npath = \"x\"\nroot = true\n",
    )
    .unwrap();
    let err = AetherConfig::load(&bad).unwrap_err();
    assert!(
        err.to_string().contains("unknown field"),
        "unexpected: {err}"
    );
}
