import { readFileSync, readdirSync } from "node:fs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";

const html = readFileSync("dist/index.html", "utf8");
const head = html.slice(html.indexOf("<head"), html.indexOf("</head>"));
const firstScript = head.indexOf("<script");
assert.ok(firstScript >= 0, "no script in head");
const charsetAt = head.indexOf("<meta charset");
const cspAt = head.indexOf('http-equiv="Content-Security-Policy"');
assert.ok(charsetAt >= 0 && charsetAt < 1024, "charset must be within the first 1024 bytes");
assert.ok(cspAt > charsetAt, "CSP meta must follow charset");
assert.ok(cspAt < firstScript, "CSP meta must precede the first script");
const boot = /<script>([\s\S]*?)<\/script>/.exec(head.slice(firstScript));
assert.ok(boot && head.slice(firstScript).startsWith("<script>"), "first script must be the attribute-less bootstrap");
assert.ok(boot[1].includes("__smithagentsApplyTheme"), "bootstrap not inlined");
const hash = createHash("sha256").update(boot[1], "utf8").digest("base64");
const csp = /http-equiv="Content-Security-Policy" content="([^"]*)"/.exec(head)[1];
assert.ok(csp.includes(`'sha256-${hash}'`), "CSP hash must equal sha256 of the bootstrap");
assert.ok(csp.startsWith("default-src 'none'"), "CSP default-src");
assert.ok(!/unsafe-inline|unsafe-eval/.test(csp.replace("style-src-attr 'unsafe-inline'", "")), "CSP too loose");
const firstSheet = head.search(/<link[^>]*stylesheet|<style/);
assert.ok(firstSheet < 0 || firstScript < firstSheet, "bootstrap must precede first <link>/<style>");

const cssFiles = readdirSync("dist/assets").filter((f) => f.endsWith(".css"));
assert.ok(cssFiles.length > 0, "no built css");
const css = cssFiles.map((f) => readFileSync(`dist/assets/${f}`, "utf8")).join("\n");
assert.ok(css.includes("--color-background"), "css lacks --color-background");
assert.ok(css.includes("--color-card-2"), "css lacks --color-card-2");
assert.ok(css.includes("--color-center"), "css lacks --color-center");
assert.ok(/\[data-theme=["']?smith-dark["']?\]/.test(css), "css lacks smith-dark rule");


const jsFiles = readdirSync("dist/assets").filter((f) => f.endsWith(".js"));
const js = jsFiles.map((f) => readFileSync(`dist/assets/${f}`, "utf8")).join("\n");
assert.ok(!js.includes("smith-dev-query-fallback"), "dev query-param fallback leaked into production build");
assert.ok(!js.includes("URLSearchParams"), "URLSearchParams in production build");
assert.ok(!js.includes("location.search"), "location.search in production build");
assert.ok(!js.includes('"daemon"'), "dev query key leaked into production build");
console.log("build output OK");
