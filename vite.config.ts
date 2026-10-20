/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Tauri expects a fixed dev port and serves the built files from dist/.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  // The Rust side rebuilds itself; watching target/ exhausts inotify watches.
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**", "**/target/**"] } },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true },
  // Unit tests for the pure helpers in src/lib (DOM tests opt into jsdom per file).
  test: { include: ["src/**/*.test.ts"], environment: "node" },
});
