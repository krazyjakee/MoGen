import { defineConfig } from "vite";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..");

export default defineConfig({
  // Allow `import.meta.glob` to reach the example .mog files that live in
  // `<repo>/examples` — outside Vite's normal root.
  server: {
    port: 5173,
    strictPort: false,
    fs: { allow: [here, repoRoot] },
  },
  // Wasm needs to be served as a static asset; treat the bg.wasm file as a
  // first-class asset rather than letting Vite try to pre-bundle it.
  assetsInclude: ["**/*.wasm"],
});
