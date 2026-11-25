import { readFileSync, readdirSync } from "node:fs";
import assert from "node:assert/strict";

const html = readFileSync("dist/index.html", "utf8");
const head = html.slice(html.indexOf("<head"), html.indexOf("</head>"));
const firstScript = head.indexOf("<script");
assert.ok(firstScript >= 0, "no script in head");
assert.ok(head.includes("__smithagentsApplyTheme"), "bootstrap not inlined");
const firstSheet = head.search(/<link[^>]*stylesheet|<style/);
assert.ok(firstSheet < 0 || firstScript < firstSheet, "bootstrap must precede first <link>/<style>");
assert.ok(head.indexOf("__smithagentsApplyTheme") < (firstSheet < 0 ? Infinity : firstSheet));

const cssFiles = readdirSync("dist/assets").filter((f) => f.endsWith(".css"));
assert.ok(cssFiles.length > 0, "no built css");
const css = cssFiles.map((f) => readFileSync(`dist/assets/${f}`, "utf8")).join("\n");
assert.ok(css.includes("--color-background"), "css lacks --color-background");
assert.ok(/\[data-theme=["']?smith-dark["']?\]/.test(css), "css lacks smith-dark rule");
console.log("build output OK");
