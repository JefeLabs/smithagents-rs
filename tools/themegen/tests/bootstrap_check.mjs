import fs from "node:fs";
import vm from "node:vm";
import assert from "node:assert/strict";

const code = fs.readFileSync(process.argv[2], "utf8");

function run({ host, media }) {
  const attrs = {};
  const props = {};
  const style = {
    colorScheme: "",
    setProperty(k, v) { props[k] = String(v); },
  };
  const documentElement = {
    style,
    setAttribute(k, v) { attrs[k] = String(v); },
  };
  const window = {};
  if (host !== undefined) window.__SMITH_THEME__ = host;
  if (media !== undefined) {
    window.matchMedia = (q) => ({ matches: media === "dark" && q.includes("dark") });
  }
  const ctx = { window, document: { documentElement } };
  ctx.globalThis = ctx;
  vm.runInNewContext(code, ctx);
  return { attrs, props, style, window };
}

const st = (o) => ({ theme: "smith-dark", font: "system", text_scale: 1, ...o });

// a
let r = run({ host: st({ theme: "smith-light" }), media: "dark" });
assert.equal(r.attrs["data-theme"], "smith-light");
assert.equal(r.style.colorScheme, "light");
// b
assert.equal(run({ media: "dark" }).attrs["data-theme"], "smith-dark");
assert.equal(run({ media: "light" }).attrs["data-theme"], "smith-light");
assert.equal(run({ media: "dark" }).style.colorScheme, "dark");
// c
assert.equal(run({ host: st({ theme: "ghost" }), media: "dark" }).attrs["data-theme"], "smith-dark");
assert.equal(run({ host: st({ theme: "ghost" }), media: "light" }).attrs["data-theme"], "smith-light");
// unknown font -> first font
r = run({ host: st({ font: "nope" }), media: "dark" });
assert.ok(r.attrs["data-font"].length > 0 && r.attrs["data-font"] !== "nope");
// d
for (const [input, want] of [[NaN, "1"], [Infinity, "1"], [99, "1.5"], [0.1, "0.85"], [1.2, "1.2"]]) {
  r = run({ host: st({ text_scale: input }), media: "dark" });
  assert.equal(r.props["--text-scale"], want, `scale ${input}`);
}
// e
r = run({ host: st({ theme: "smith-dark" }), media: "dark" });
assert.equal(r.attrs["data-theme"], "smith-dark");
r.window.__smithagentsApplyTheme(st({ theme: "smith-light", font: "mono", text_scale: 1.25 }));
assert.equal(r.attrs["data-theme"], "smith-light");
assert.equal(r.attrs["data-font"], "mono");
assert.equal(r.props["--text-scale"], "1.25");
assert.equal(r.style.colorScheme, "light");
// f
for (const bad of ["garbage", 42, null, [1]]) {
  r = run({ host: bad, media: "dark" });
  assert.equal(r.attrs["data-theme"], "smith-dark");
  r.window.__smithagentsApplyTheme(bad);
}
// g
r = run({});
assert.equal(r.attrs["data-theme"], "smith-dark");
r.window.__smithagentsApplyTheme(undefined);
console.log("bootstrap checks passed");
