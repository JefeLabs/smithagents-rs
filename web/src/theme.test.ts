import { readFileSync } from "node:fs";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";

const script = readFileSync(
  path.resolve(__dirname, "../../design/generated/theme-bootstrap.js"),
  "utf8",
);

function run() {
  new Function(script)();
}

function reset() {
  const root = document.documentElement;
  root.removeAttribute("data-theme");
  root.removeAttribute("data-font");
  root.removeAttribute("style");
  delete (window as any).__SMITH_THEME__;
}

function stubMedia(dark: boolean) {
  window.matchMedia = ((q: string) => ({ matches: dark, media: q })) as any;
}

describe("theme bootstrap", () => {
  afterEach(reset);

  it("applies host state", () => {
    (window as any).__SMITH_THEME__ = {
      theme: "smith-light",
      appearance: "light",
      text_scale: 1.2,
      font: "system",
    };
    run();
    const root = document.documentElement;
    expect(root.getAttribute("data-theme")).toBe("smith-light");
    expect(root.getAttribute("data-font")).toBe("system");
    expect(root.style.getPropertyValue("--text-scale")).toBe("1.2");
  });

  it("defaults to dark when media prefers dark and no host state", () => {
    stubMedia(true);
    run();
    const root = document.documentElement;
    expect(root.getAttribute("data-theme")).toBe("smith-dark");
    expect(root.style.getPropertyValue("--text-scale")).toBe("1");
  });

  it("defaults to light when media prefers light and no host state", () => {
    stubMedia(false);
    run();
    expect(document.documentElement.getAttribute("data-theme")).toBe("smith-light");
  });
});
