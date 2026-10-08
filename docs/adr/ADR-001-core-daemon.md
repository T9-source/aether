# ADR-001: Core Daemon vs In-Process

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Project founder + architecture review

## Context

The original vision placed the core inside the desktop app
(`React → Tauri → IPC → Core`). That contradicts the stated principle
"the GUI is just a client" and blocks three first-class requirements:
a CLI (`aether proxy`, `aether replay`), headless CI/CD usage, and — later —
team/remote scenarios. Precedent: Caido ships as an instance (daemon) plus
thin web clients; mitmproxy separates engine from UI. Competing options:

1. Core compiled into the Tauri app (in-process commands).
2. Core as a Tauri sidecar only.
3. Core as a standalone, UI-independent daemon.

## Decision

**Aether Core is a standalone, UI-independent daemon process.** It exposes the
control plane (ADR-002) and owns project state. All clients — Tauri UI, CLI,
CI jobs — connect over the same network boundary, even when on localhost.

Rules:
- Core must never depend on UI crates (workspace-level lint / crate dependency rule).
- Core runs fully headless: `aether serve --project api.aether`.
- The Tauri app *manages* the daemon lifecycle for convenience (spawn, health-check,
  shut down) but the daemon must be fully usable without the app.
- Single-writer principle: the daemon is the only process that touches the project file.
- One daemon serves multiple clients simultaneously (UI + CLI at the same time).

## Consequences

Positive:
- CLI and CI/CD are free from day 1 — they are just control-plane clients.
- Integration tests drive the system exactly like the real UI does.
- Remote/team daemon becomes a deployment option, not a rewrite.

Negative (accepted costs):
- Every call crosses a serialization boundary — no free in-process shortcuts.
- Slightly more operational complexity: port allocation, auth token, health checks.

## Alternatives Considered

- **In-process Tauri commands** — rejected: breaks CLI/CI promise; the "platform"
  positioning dies at day 1.
- **Sidecar-only** — rejected: same in-process coupling with extra packaging pain.
