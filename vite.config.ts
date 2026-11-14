/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  // The Rust build's messages stay on screen.
  clearScreen: false,
  // Tauri loads the page from this fixed port in development (`devUrl` in tauri.conf.json): a
  // taken port is an error.
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // The backend, the engine builds and the Rust toolchain are not the page's sources.
      ignored: ["**/src-tauri/**", "**/runtimes/**", "**/.rust/**"],
    },
  },
  // Three pages: the main window, the dictation indicator and the quick panel.
  build: {
    rollupOptions: {
      input: { main: "index.html", indicator: "indicator.html", tray: "tray.html" },
    },
  },
  test: {
    include: ["src/**/*.test.ts"],
    // Stylesheets a test reads as text (`?raw`); others stay empty under Vitest.
    css: { include: [/tray\.css/, /indicator\.css/, /App\.css/] },
  },
});
