import { resolve } from "node:path";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// One page per window. Tauri loads them from the dev server in `tauri dev`
// and from dist/ in release builds.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    target: "es2022",
    rollupOptions: {
      input: {
        spotlight: resolve(import.meta.dirname, "spotlight.html"),
        settings: resolve(import.meta.dirname, "settings.html"),
      },
    },
  },
});
