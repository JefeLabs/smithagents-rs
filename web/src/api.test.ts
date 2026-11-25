import { describe, expect, it, vi } from "vitest";
import { ApiError, getAgent, getMessages } from "./api";
import type { HostConfig } from "./host";

const cfg: HostConfig = { baseUrl: "http://127.0.0.1:1", token: "secret", agentId: "a1" };

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

function stub(fn: (url: string) => Response | Promise<Response>) {
  const f = vi.fn((url: string, _init?: RequestInit) => Promise.resolve(fn(url)));
  vi.stubGlobal("fetch", f);
  return f;
}

const agent = (id: string, extra = {}) => ({
  id, name: `n-${id}`, color: { l: 0.7, c: 0.1, h: 123 }, state: "idle",
  repo_id: "r", worktree_path: "/w", branch: "b", harness_session_id: null,
  created_at: "t", closed_at: null, ...extra,
});
const msg = (seq: number) => ({
  id: `m${seq}`, agent_id: "a1", seq, role: "user", text: `t${seq}`, created_at: "t",
});

describe("getAgent", () => {
  it("picks the agent by id, keeps only hue, sends bearer", async () => {
    const f = stub(() => json([agent("other"), agent("a1")]));
    const a = await getAgent(cfg);
    expect(a).toEqual({ id: "a1", name: "n-a1", color: { h: 123 }, state: "idle" });
    const [url, init] = f.mock.calls[0]!;
    expect(url).toBe("http://127.0.0.1:1/v1/agents?include_closed=true");
    expect(new Headers(init?.headers).get("authorization")).toBe("Bearer secret");
  });

  it("throws not_found-style ApiError when the agent is absent", async () => {
    stub(() => json([agent("other")]));
    await expect(getAgent(cfg)).rejects.toMatchObject({ status: 404, code: "agent_not_found" });
  });
});

describe("getMessages", () => {
  it("sends bearer and returns messages", async () => {
    const f = stub(() => json([msg(1)]));
    expect(await getMessages(cfg)).toEqual([msg(1)]);
    const [url, init] = f.mock.calls[0]!;
    expect(url).toContain("/v1/agents/a1/messages?");
    expect(new Headers(init?.headers).get("authorization")).toBe("Bearer secret");
  });

  it("pages until a short page", async () => {
    const f = stub((url) => {
      const after = Number(new URL(url).searchParams.get("after"));
      const limit = Number(new URL(url).searchParams.get("limit"));
      expect(limit).toBe(1000);
      if (after === 0) return json(Array.from({ length: 1000 }, (_, i) => msg(i + 1)));
      return json([msg(1001)]);
    });
    const all = await getMessages(cfg);
    expect(all).toHaveLength(1001);
    expect(f).toHaveBeenCalledTimes(2);
    expect(new URL(f.mock.calls[1]![0]).searchParams.get("after")).toBe("1000");
  });

  it.each([401, 404, 409])("maps %i daemon error body", async (status) => {
    stub(() => json({ error: "some_code", message: "boom" }, status));
    const e = await getMessages(cfg).catch((x) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect(e).toMatchObject({ status, code: "some_code", message: "boom" });
  });

  it("non-JSON error body becomes unknown", async () => {
    stub(() => new Response("<html>bad gateway</html>", { status: 502 }));
    await expect(getMessages(cfg)).rejects.toMatchObject({ status: 502, code: "unknown" });
  });

  it("network failure becomes status 0 network", async () => {
    vi.stubGlobal("fetch", vi.fn(() => Promise.reject(new TypeError("failed"))));
    await expect(getMessages(cfg)).rejects.toMatchObject({ status: 0, code: "network" });
  });
});
