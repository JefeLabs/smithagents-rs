import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";

const here = path.dirname(fileURLToPath(import.meta.url));
const designDir = path.resolve(here, "../design");

// Inlines the generated theme bootstrap as the first <script> in <head> so the
// theme is applied before any stylesheet paints. In the built page the head is
// ordered: charset, CSP meta (with the bootstrap's sha256), bootstrap, rest.
const CSP = (hash: string) =>
  [
    "default-src 'none'",
    `script-src 'self' 'sha256-${hash}'`,
    "style-src 'self'",
    "style-src-attr 'unsafe-inline'",
    "img-src 'self' data:",
    "font-src 'self'",
    "connect-src http://127.0.0.1:* http://localhost:*",
    "base-uri 'none'",
    "form-action 'none'",
  ].join("; ");

function themeBootstrap(): Plugin {
  return {
    name: "smithagents-theme-bootstrap",
    transformIndexHtml: {
      order: "post",
      handler(html, ctx) {
        const file = path.join(designDir, "generated/theme-bootstrap.js");
        const children = readFileSync(file, "utf8").replace(/<\/script/gi, "<\\/script");
        if (ctx.server) return [{ tag: "script", children, injectTo: "head-prepend" }];
        const hash = createHash("sha256").update(children, "utf8").digest("base64");
        const charset = /<meta charset="[^"]*"\s*\/?>/i.exec(html);
        if (!charset) throw new Error("index.html must contain <meta charset>");
        const insert = `\n    <meta http-equiv="Content-Security-Policy" content="${CSP(hash)}" />\n    <script>${children}</script>`;
        const at = charset.index + charset[0].length;
        return html.slice(0, at) + insert + html.slice(at);
      },
    },
  };
}

export default defineConfig({
  plugins: [themeBootstrap(), react(), tailwindcss()],
  resolve: { alias: { "@": path.resolve(here, "src") } },
  server: { fs: { allow: [here, designDir] } },
});
