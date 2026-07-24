import { defineConfig } from "vite";

// Build straight into the track-assets component's `static/` dir, so its
// build.rs embeds this bundle into the wasm — no separate copy step.
export default defineConfig({
  base: "/",
  build: {
    outDir: "../../../components/track-assets/static",
    emptyOutDir: true,
  },
  server: {
    // dev: proxy the API to a running `just host-track`.
    proxy: {
      "/api": "http://localhost:3025",
      "/auth": "http://localhost:3025",
    },
  },
});
