import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";

const here = path.dirname(fileURLToPath(import.meta.url));
const designDir = path.resolve(here, "../design");

// Inlines the generated theme bootstrap as the first <script> in <head> so the
// theme is applied before any stylesheet paints (dev and build).
function themeBootstrap(): Plugin {
  return {
    name: "smithagents-theme-bootstrap",
    transformIndexHtml() {
      const file = path.join(designDir, "generated/theme-bootstrap.js");
      const children = readFileSync(file, "utf8").replace(/<\/script/gi, "<\\/script");
      return [{ tag: "script", children, injectTo: "head-prepend" }];
    },
  };
}

export default defineConfig({
  plugins: [themeBootstrap(), react(), tailwindcss()],
  resolve: { alias: { "@": path.resolve(here, "src") } },
  server: { fs: { allow: [here, designDir] } },
});
