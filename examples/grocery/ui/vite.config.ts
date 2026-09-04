import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { resolve } from "path";

// Build straight into the grocery-assets component's static/ directory
// so build.rs embeds the bundled assets into the wasm component.
export default defineConfig({
  plugins: [react()],
  base: "/",
  build: {
    outDir: resolve(__dirname, "../../../components/grocery-assets/static"),
    emptyOutDir: true,
  },
  server: {
    port: 3056,
    proxy: {
      "/api": "http://localhost:3055",
      "/auth": "http://localhost:3055",
    },
  },
});
