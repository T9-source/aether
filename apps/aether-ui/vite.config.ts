import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The UI is a pure static client (ADR-001: the daemon owns all state —
// this app owns none). In dev it talks to a local daemon; in production
// the built dist/ is either served statically (browser client) or wrapped
// by the Tauri desktop shell (apps/aether-desktop).
export default defineConfig({
  plugins: [react()],
  server: { port: 5173, strictPort: true },
  preview: { port: 4173, strictPort: true },
  build: {
    target: "es2022",
    sourcemap: true,
  },
  test: {
    environment: "node",
    include: ["tests/**/*.test.ts"],
  },
});
