import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";

// Build the SPA to ../dist, which the native host serves via --static-dir.
export default defineConfig({
  plugins: [react()],
  resolve: { alias: { "@": path.resolve(__dirname, "src") } },
  build: { outDir: "../dist", emptyOutDir: true },
});
