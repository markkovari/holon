import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Build straight into the assets component's `static/`, which its build.rs walks
// and `include_bytes!`s. The console is served by `console-assets` (the deployable
// path) rather than the host's --static-dir, so there is no separate dist to copy.
export default defineConfig({
  plugins: [react()],
  build: { outDir: "../../../components/console-assets/static", emptyOutDir: true },
});
