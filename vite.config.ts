import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

// Two pages: the settings window (opened on demand) and the tiny recording
// indicator. No framework, so both load in a few milliseconds.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 5174,
    strictPort: true,
  },
  build: {
    target: "es2022",
    outDir: "dist",
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL("index.html", import.meta.url)),
        overlay: fileURLToPath(new URL("overlay.html", import.meta.url)),
      },
    },
  },
});
