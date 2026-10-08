# Aether Desktop (Tauri 2 shell)

The desktop window around the Aether console (`apps/aether-ui`). It adds **no
state and no privileges** — the daemon owns everything (ADR-001) and the UI
talks to it over WebSocket exactly like the browser build. The daemon's
control-plane origin check accepts `tauri://localhost` /
`https://tauri.localhost` for this shell (ADR-012).

This crate is **intentionally outside the Cargo workspace**: compiling Tauri
requires desktop webview system libraries that headless machines don't have.

## Building (on a desktop machine)

Prerequisites:
- Rust 1.85+ and Node 20+
- Linux: `libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`
- macOS: Xcode command line tools
- Windows: WebView2 (preinstalled on Win10/11)
- `cargo install tauri-cli --version '^2'`

Steps:

```bash
# 1. build the console UI once (the shell embeds its dist/)
cd apps/aether-ui
npm install && npm run build

# 2. build the desktop app (dev or bundle)
cd ../aether-desktop/src-tauri
cargo tauri dev        # live development against the local UI
cargo tauri build      # installers/bundles into target/release/bundle/
```

## Running against a daemon

1. Start the daemon: `aether serve` (note the control-plane address + token).
2. In the Aether window, enter `ws://127.0.0.1:9099` and the token
   (printed by the daemon, stored at `~/.aether/token`).

The webview origin is `tauri://localhost` (macOS/Linux) or
`https://tauri.localhost` (Windows) — both are in the daemon's allowlist.
CSP is set in `tauri.conf.json`: WebSocket connections are limited to
loopback hosts.
