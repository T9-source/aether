#!/usr/bin/env node
//! Aether example — a minimal WebSocket echo origin (RFC 6455, server side).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Used by the V0.2 demo (`scripts/` + docs) and handy for manual testing:
//!   node examples/ws-echo-server.js [port]
//! Then point any WebSocket client at ws://127.0.0.1:<port>/ws — every
//! text/binary message is echoed back. Pair it with Aether to watch the
//! relay record the session in the console.
"use strict";
const http = require("http");
const crypto = require("crypto");

const GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const port = Number(process.argv[2] || 7002);

const server = http.createServer((req, res) => {
  res.writeHead(200, { "content-type": "text/plain" });
  res.end("aether ws echo origin — upgrade to /ws\n");
});

server.on("upgrade", (req, socket) => {
  const key = req.headers["sec-websocket-key"];
  if (!key) {
    socket.write("HTTP/1.1 400 Bad Request\r\n\r\n");
    return socket.destroy();
  }
  const accept = crypto.createHash("sha1").update(key + GUID).digest("base64");
  socket.write(
    "HTTP/1.1 101 Switching Protocols\r\n" +
      "Upgrade: websocket\r\n" +
      "Connection: Upgrade\r\n" +
      "Sec-WebSocket-Accept: " + accept + "\r\n\r\n"
  );
  socket.on("error", () => {});
  socket.on("data", (buf) => {
    if (buf.length < 2) return;
    const opcode = buf[0] & 0x0f;
    if (opcode === 0x8) {
      // close: answer close then drop
      socket.write(Buffer.from([0x88, 0x00]));
      socket.end();
      return;
    }
    if (opcode === 0x9) {
      // ping → pong (FIN|0x0A)
      const len = buf[1] & 0x7f;
      socket.write(Buffer.concat([Buffer.from([0x8a, len]), buf.slice(2, 2 + len)]));
      return;
    }
    if (opcode !== 0x1 && opcode !== 0x2) return;
    const len = buf[1] & 0x7f;
    const mask = buf.slice(2, 6);
    const payload = Buffer.from(buf.slice(6, 6 + len));
    for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i % 4];
    // echo unmasked: FIN | opcode, length (small messages only)
    const out = Buffer.concat([Buffer.from([0x80 | opcode, len]), payload]);
    socket.write(out);
  });
});

server.listen(port, "127.0.0.1", () => {
  console.log(`ws echo origin on ws://127.0.0.1:${port}/ws`);
});
