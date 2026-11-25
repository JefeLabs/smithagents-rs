import { afterEach, describe, expect, it, vi } from "vitest";
import { getHostConfig, HostConfigError } from "./host";

const good = { baseUrl: "http://127.0.0.1:4545", token: "s3cr3t-value", agentId: "a-1" };

function setHost(v: unknown) {
  (window as unknown as { __SMITH_HOST__?: unknown }).__SMITH_HOST__ = v;
}

afterEach(() => {
  delete (window as unknown as { __SMITH_HOST__?: unknown }).__SMITH_HOST__;
  vi.unstubAllEnvs();
  window.history.replaceState(null, "", "/");
});

describe("getHostConfig", () => {
  it("reads the injected object", () => {
    setHost(good);
    expect(getHostConfig()).toEqual(good);
  });

  it("accepts localhost", () => {
    setHost({ ...good, baseUrl: "http://localhost:80" });
    expect(getHostConfig().baseUrl).toBe("http://localhost:80");
  });

  it.each([
    ["non-object", "nope"],
    ["empty token", { ...good, token: "" }],
    ["missing agentId", { baseUrl: good.baseUrl, token: "t" }],
    ["numeric token", { ...good, token: 5 }],
    ["remote host", { ...good, baseUrl: "http://evil.example:80" }],
    ["https", { ...good, baseUrl: "https://127.0.0.1:80" }],
    ["port 0", { ...good, baseUrl: "http://127.0.0.1:0" }],
    ["port too large", { ...good, baseUrl: "http://127.0.0.1:65536" }],
    ["no port", { ...good, baseUrl: "http://127.0.0.1" }],
    ["path suffix", { ...good, baseUrl: "http://127.0.0.1:80/x" }],
  ])("rejects %s without leaking the token", (_n, v) => {
    setHost(v);
    try {
      getHostConfig();
      expect.unreachable();
    } catch (e) {
      expect(e).toBeInstanceOf(HostConfigError);
      expect((e as Error).message).not.toContain("s3cr3t-value");
    }
  });

  it("throws when nothing is configured", () => {
    expect(() => getHostConfig()).toThrow(HostConfigError);
  });

  it("falls back to query params in dev", () => {
    vi.stubEnv("DEV", true);
    window.history.replaceState(null, "", "/?daemon=http://127.0.0.1:9&token=t&agent=ag");
    expect(getHostConfig()).toEqual({ baseUrl: "http://127.0.0.1:9", token: "t", agentId: "ag" });
  });

  it("ignores query params outside dev", () => {
    vi.stubEnv("DEV", false);
    window.history.replaceState(null, "", "/?daemon=http://127.0.0.1:9&token=t&agent=ag");
    expect(() => getHostConfig()).toThrow(HostConfigError);
  });

  it("prefers the injected object over query params", () => {
    vi.stubEnv("DEV", true);
    window.history.replaceState(null, "", "/?daemon=http://127.0.0.1:9&token=q&agent=q");
    setHost(good);
    expect(getHostConfig()).toEqual(good);
  });
});
