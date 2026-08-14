import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri-specific settings: fixed dev port, ignore src-tauri/ in the watcher.
// See https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
