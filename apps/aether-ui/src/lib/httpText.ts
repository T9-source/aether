// Raw-HTTP rendering + cURL export — read-only projections of the domain
// model (the same shapes the wire codec owns in Rust, ADR-003).
import type { Body, Headers, Request, Response } from "../types";

export function bodyBytes(b: Body): Uint8Array {
  switch (b.kind) {
    case "empty":
      return new Uint8Array(0);
    case "text":
      return new TextEncoder().encode(b.text);
    case "binary":
      return base64ToBytes(b.bytes);
  }
}

export function bodyAsText(b: Body): string {
  switch (b.kind) {
    case "empty":
      return "";
    case "text":
      return b.text;
    case "binary": {
      // best-effort UTF-8 view; the hex viewer is the honest fallback
      return new TextDecoder("utf-8", { fatal: false }).decode(base64ToBytes(b.bytes));
    }
  }
}

export function bodyIsTextual(b: Body): boolean {
  return b.kind === "text" || b.kind === "empty";
}

export function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function bytesToBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

function headersBlock(h: Headers): string {
  return h.entries.map((e) => `${e.name}: ${e.value}`).join("\r\n");
}

/** Origin-form request target: path + optional `?query`. */
function requestTarget(r: Request): string {
  const q = r.url.query ? `?${r.url.query}` : "";
  return `${r.url.path}${q}`;
}

export function requestToRaw(r: Request): string {
  const head = [
    `${methodToString(r.method)} ${requestTarget(r)} HTTP/${versionToString(r.version)}`,
    headersBlock(r.headers),
  ].join("\r\n");
  const body = r.body.kind === "empty" ? "" : `\r\n${bodyAsText(r.body)}`;
  return `${head}${body}`;
}

export function responseToRaw(r: Response): string {
  const head = [
    `HTTP/${versionToString(r.version)} ${r.status.code}`,
    headersBlock(r.headers),
  ].join("\r\n");
  const body = r.body.kind === "empty" ? "" : `\r\n${bodyAsText(r.body)}`;
  return `${head}${body}`;
}

export function methodToString(m: Method2): string {
  return typeof m === "string" ? m.toUpperCase() : m.custom;
}

// local alias to avoid an extra import cycle in editors
type Method2 = Request["method"];

export function versionToString(v: string): string {
  switch (v) {
    case "http10":
      return "1.0";
    case "http11":
      return "1.1";
    case "http2":
      return "2";
    case "http3":
      return "3";
    default:
      return "1.1";
  }
}

/** Copy-as-cURL for the request as captured (single-quoted, POSIX). */
export function requestToCurl(r: Request): string {
  const scheme = r.url.scheme || "http";
  const host = r.url.host;
  const port = r.url.port;
  const hostPort = port !== null && !isDefaultPort(scheme, port) ? `${host}:${port}` : host;
  const url = `${scheme}://${hostPort}${requestTarget(r)}`;
  const parts = ["curl", "-i", "-X", methodToString(r.method), `'${url}'`];
  for (const h of r.headers.entries) {
    parts.push("-H", `'${h.name}: ${h.value.replace(/'/g, `'\\''`)}'`);
  }
  if (r.body.kind === "text" && r.body.text.length > 0) {
    parts.push("--data-raw", `'${r.body.text.replace(/'/g, `'\\''`)}'`);
  } else if (r.body.kind === "binary" && r.body.bytes.length > 0) {
    parts.push("--data-binary", `'${r.body.bytes}'`); // base64 passthrough note in UI
  }
  return parts.join(" ");
}

function isDefaultPort(scheme: string, port: number): boolean {
  return (
    (scheme === "http" && port === 80) || (scheme === "https" && port === 443)
  );
}

/** Hex + ASCII dump (16 bytes per row) — the security-tool classic. */
export function toHexDump(bytes: Uint8Array, maxRows = 512): string {
  const rows: string[] = [];
  const hex = (n: number) => n.toString(16).padStart(2, "0");
  const ascii = (n: number) => (n >= 0x20 && n < 0x7f ? String.fromCharCode(n) : ".");
  for (let off = 0; off < bytes.length; off += 16) {
    if (rows.length >= maxRows) {
      rows.push("… (truncated — download for full payload)");
      break;
    }
    const slice = bytes.subarray(off, off + 16);
    const hexPart = Array.from(slice, hex).join(" ").padEnd(47, " ");
    const asciiPart = Array.from(slice, ascii).join("");
    rows.push(`${off.toString(16).padStart(8, "0")}  ${hexPart}  |${asciiPart}|`);
  }
  return rows.join("\n");
}
