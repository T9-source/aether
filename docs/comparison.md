# Aether vs the field — an evidence-first comparison

Aether competes with excellent tools. This page states the differences we
believe matter, with sources, and is equally explicit about where the
incumbents still lead. Every quote was fetched from the linked source on
**2026-10-11**; pages change, so we re-verify before each release and date
the checks. Disagree with something? Open an issue — the page is versioned
like the code.

## The one-sentence version

Aether is a local-first API security proxy built like a database, not like
a desktop app: all flows live in SQLite on your disk, searchable at scale,
with reproducible performance budgets in CI — and an MCP layer that lets
your own AI assistant read (not touch) your traffic, offline, by default.

## Where Aether leads today

### 1. Storage: SQLite on disk, not "all flows in memory"

mitmproxy's own issue tracker has asked for exactly this since 2017:

> "We can solve the long-standing issue we've had where mitmproxy and
> mitmweb memory use grows infinitely … The storage file is a SQLite
> database (or similar) that contains all the auxiliary data."
> — mitmproxy maintainer cortesi, [issue #2175 "Session store"](https://github.com/mitmproxy/mitmproxy/issues/2175), opened Mar 17, 2017 (still open at our last check)

And when users asked for a memory cap, the request was closed as not planned:

> "Mitmproxy memory usage will grow as more flows are added. Large
> downloads can cause issues." — [issue #6398](https://github.com/mitmproxy/mitmproxy/issues/6398)
> closed as not planned: "This is a duplicate of #4129…"
> The maintainer's workaround list ends with: "I think the proper way to
> solve this is #2175." — [issue #4129](https://github.com/mitmproxy/mitmproxy/issues/4129)

Aether ships that design today: every flow is written to SQLite (WAL) with
FTS5 full-text search, and the console lists 100k+ flows from a virtualized
view without holding them in RAM.

### 2. Performance you can reproduce, not vibes we claim

We know of no traffic proxy that publishes reproducible benchmarks. Aether
puts **performance budgets in CI** — a regression fails the build:

| Metric (V0.2 baseline, `aether bench flows --n 100000`) | Result |
|---|---|
| Flow insert throughput | ≈ 12,000 flows/s |
| History page (50 rows) p50 | ≈ 0.5 ms |
| FTS5 search over 100k flows | ≈ 70 ms |

Reproduce: `aether bench flows --n 100000` · steady-state: `aether soak
--secs 60` (the 24h acceptance run is on the roadmap gate) · methodology
and budget thresholds live in `scripts/check-perf-budget.sh` and
`docs/PERF.md`.

### 3. AI that stays on your machine — by default

Burp AI is cloud-only, on its own FAQ:

> "Can I use Burp AI offline? Not currently, but we may explore local model
> options based on demand." — [PortSwigger, Burp AI FAQ](https://portswigger.net/burp/ai)

Aether's answer is architectural, not a pricing tier: `aether mcp` speaks
the Model Context Protocol over stdio, so **your** assistant — local or
BYO-key — can search flows, review intercepts and inspect rules. Read-only
by default; the three mutating tools only appear with an explicit
`--allow-write` flag. No credits, no telemetry, no walled garden.

### 4. Extensibility that cannot backfire

Declarative `RuleSpec` rules (JSON, live over the control plane) ship now;
the V0.3 plugin host is WASM with **capability permissions, deny by
default** — cross-language, hot-reloadable, and sandboxed. For contrast:

> "All extensions to Caido are written in Javascript… unified development
> experience" — [Caido developer docs](https://developer.caido.io/plugins/concepts/runtime)

and Burp extensions run on the JVM with full host access. Both are powerful;
neither is sandboxed. We think a security tool's extension model should be.

## Where the incumbents still lead (honesty section)

- **mitmproxy** has 44k+ stars, ~4M PyPI downloads/month, a mature Python
  addon ecosystem, and experimental HTTP/3 support in some modes. Aether
  has none of that ecosystem weight yet — it is at V0.2.
- **Burp Suite** is the industry default with 300+ BApp extensions and a
  decade of pentest workflow polish. Aether does not try to be Burp; it
  tries to be what Burp's UI cannot be: a fast local engine with a modern
  console.
- **HTTP/3 (QUIC)**: mitmproxy intercepts h3 in transparent/WireGuard/
  reverse modes — its maintainer is explicit that CONNECT-based regular
  proxying cannot carry QUIC ("QUIC is a UDP-based protocol…"). Aether
  deliberately does not market HTTP/3 interception: the honest state of the
  art for *regular-mode* QUIC MITM is "not currently practical", for every
  tool in this category.
- **Pricing maturity**: Aether is free and Apache-2.0. Burp Pro is
  $499/user/year ([PortSwigger](https://portswigger.net/buy/pro), checked
  2026-10-11); Caido and Reqable ship capable free tiers. "Free" is a real
  difference, but ecosystem maturity is a real cost — choose accordingly.

## Scorecard

| Capability | Aether 0.2 | mitmproxy 12 | Burp 2026 | Caido |
|---|---|---|---|---|
| Flows on disk (SQLite), unbounded history | ✅ | ❌ (RAM, [#2175](https://github.com/mitmproxy/mitmproxy/issues/2175) open) | partial (manual disk projects) | ✅ |
| Published reproducible benchmarks | ✅ CI-gated | ❌ | ❌ | ❌ |
| MCP server (AI clients) | ✅ read-only by default | ❌ | ❌ (cloud AI) | ❌ |
| HTTP/1 + HTTP/2 + WebSocket inspection | ✅ | ✅ | ✅ | ✅ |
| HTTP/3 interception | ❌ (documented why) | partial (modes) | ❌ | ❌ |
| Fuzz-tested wire parsers | ✅ CI-gated | ✅ | n/a (closed) | n/a (closed) |
| Open source (Apache-2.0 / MIT) | ✅ | ✅ (MIT) | ❌ | ❌ |
| Zero telemetry | ✅ | ✅ | ❌ | ❌ |

*Every ❌ we claim about a competitor is sourced: mitmproxy storage
[#2175](https://github.com/mitmproxy/mitmproxy/issues/2175) and memory
[#6398](https://github.com/mitmproxy/mitmproxy/issues/6398); Burp AI FAQ
([portswigger.net/burp/ai](https://portswigger.net/burp/ai)) and Pro pricing
([portswigger.net/buy/pro](https://portswigger.net/buy/pro)); Caido JS SDK
([developer.caido.io](https://developer.caido.io/plugins/concepts/runtime));
Reqable's own repo states it is "not an open-source project" and that its
MCP server is "fully open source"
([reqable/reqable-app](https://github.com/reqable/reqable-app)) — credit
where due: Reqable ships a genuinely useful open MCP server.*

## Method

Quotes were fetched from primary sources on 2026-10-11 and re-checked each
release; claims we could not source verbatim are simply not made. Vendor
pages describe vendor products — we link them so you can verify yourself.
Spotted an error? That's a bug report, not a flame war:
[open an issue](https://github.com/smart-jemy/aether/issues).
