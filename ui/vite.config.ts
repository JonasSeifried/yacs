import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const page = (name: string) => resolve(import.meta.dirname, `${name}.html`);

// The workspace version, which the relay reports too: the PWA compares them
// to notice it's an old copy the phone kept.
const cargo = readFileSync(resolve(import.meta.dirname, "../Cargo.toml"), "utf8");
const version = /\[workspace\.package\][^[]*?\nversion = "([^"]+)"/.exec(cargo)?.[1];
if (!version) throw new Error("no workspace version in Cargo.toml");

// Two builds from one codebase:
// - `--mode desktop`: the Tauri windows, loaded by the desktop app (dist/desktop).
// - `--mode web`: the PWA the relay serves at `/` (dist/web), with `public/`
//   (manifest, service worker, icons).
// `vite` (dev) serves every page and proxies the API to a local relay.
export default defineConfig(({ mode }) => {
  const web = mode === "web";
  return {
    plugins: [react()],
    define: { __APP_VERSION__: JSON.stringify(version) },
    clearScreen: false,
    publicDir: mode === "desktop" ? false : "public",
    server: {
      port: 1420,
      strictPort: true,
      proxy: { "/api": process.env.YACS_DEV_RELAY ?? "http://127.0.0.1:8080" },
    },
    // Module workers: the upload and download workers load the WASM core,
    // whose glue finds its .wasm file through `import.meta.url`.
    worker: { format: "es" },
    build: {
      target: "es2022",
      outDir: web ? "dist/web" : "dist/desktop",
      emptyOutDir: true,
      rollupOptions: {
        input: web ? { index: page("index") } : { spotlight: page("spotlight"), settings: page("settings") },
      },
    },
  };
});
