// Single source of truth: these types are GENERATED from Rust (ts-rs, ADR-002)
// — do not hand-edit the *.ts files in this directory. This module adds one
// documented adaptation: the control plane serializes u64 as JSON numbers
// (ids never approach 2^53 in practice), so `bigint` annotations are
// normalized to `number` for the whole UI. Re-type only here.
import type { FlowStats as RawFlowStats } from "./FlowStats";

export type Normalize<T> = T extends bigint
  ? number
  : T extends (infer U)[]
    ? Normalize<U>[]
    : T extends object
      ? { [K in keyof T]: Normalize<T[K]> }
      : T;

import type { AetherEvent as RawAetherEvent } from "./AetherEvent";
import type { Body as RawBody } from "./Body";
import type { Flow as RawFlow } from "./Flow";
import type { FlowMeta as RawFlowMeta } from "./FlowMeta";
import type { FlowPage as RawFlowPage } from "./FlowPage";
import type { FlowSummary as RawFlowSummary } from "./FlowSummary";
import type { Request as RawRequest } from "./Request";
import type { Response as RawResponse } from "./Response";
import type { RuleSpec as RawRuleSpec } from "./RuleSpec";
import type { RuleAction as RawRuleAction } from "./RuleAction";
import type { TransportProto as RawTransportProto } from "./TransportProto";
import type { TlsMeta as RawTlsMeta } from "./TlsMeta";
import type { Url as RawUrl } from "./Url";
import type { Headers as RawHeaders } from "./Headers";
import type { Header as RawHeader } from "./Header";
import type { Method as RawMethod } from "./Method";
import type { HttpVersion as RawHttpVersion } from "./HttpVersion";
import type { StatusCode as RawStatusCode } from "./StatusCode";
import type { InterceptDirection as RawInterceptDirection } from "./InterceptDirection";
import type { InterceptDecision as RawInterceptDecision } from "./InterceptDecision";
import type { RuleStage as RawRuleStage } from "./RuleStage";

export type AetherEvent = Normalize<RawAetherEvent>;
export type Body = Normalize<RawBody>;
export type Flow = Normalize<RawFlow>;
export type FlowMeta = Normalize<RawFlowMeta>;
export type FlowPage = Normalize<RawFlowPage>;
export type FlowSummary = Normalize<RawFlowSummary>;
export type Request = Normalize<RawRequest>;
export type Response = Normalize<RawResponse>;
export type RuleSpec = Normalize<RawRuleSpec>;
export type RuleAction = Normalize<RawRuleAction>;
export type TransportProto = Normalize<RawTransportProto>;
export type TlsMeta = Normalize<RawTlsMeta>;
export type Url = Normalize<RawUrl>;
export type Headers = Normalize<RawHeaders>;
export type Header = Normalize<RawHeader>;
export type Method = Normalize<RawMethod>;
export type HttpVersion = Normalize<RawHttpVersion>;
export type StatusCode = Normalize<RawStatusCode>;
export type InterceptDirection = Normalize<RawInterceptDirection>;
export type InterceptDecision = Normalize<RawInterceptDecision>;
export type RuleStage = Normalize<RawRuleStage>;
export type FlowStats = Normalize<RawFlowStats>;

// ── Daemon-side DTOs (mirror crates/aether-daemon/src/control.rs — the
// daemon crate does not export ts-rs bindings; keep in sync with protocol v1) ──

export type HeldPayloadDto =
  | { request: Request }
  | { response: Response };

export type DecisionDto =
  | { action: "drop" }
  | { action: "forward"; payload: HeldPayloadDto };

export interface HeldRow {
  flow_id: number;
  direction: InterceptDirection;
  method: string | null;
  host: string | null;
  path: string | null;
  status: number | null;
}

export interface StatsInfo {
  flows: FlowStats;
  held: number;
  rules: number;
  uptime_ms: number;
  version: string;
  proxy_addr: string;
}

export interface Hello {
  version: string;
  server_time_unix_ms: number;
}
