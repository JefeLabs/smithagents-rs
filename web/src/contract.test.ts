import { readFileSync } from "node:fs";
import path from "node:path";
import { readUIMessageStream, type UIMessage } from "ai";
import { describe, expect, it } from "vitest";
import { makeTransport } from "./transport";

const FIXTURE = path.resolve(process.cwd(), "../crates/server/tests/fixtures/ui_stream_golden.sse");

describe("wire contract with the Rust encoder", () => {
  it("the SDK's own transport parser turns the golden stream into one assistant message", async () => {
    const bytes = readFileSync(FIXTURE);
    const text = bytes.toString("utf8");
    const expected = text
      .split("\n")
      .filter((l) => l.startsWith("data: {"))
      .map((l) => JSON.parse(l.slice(6)) as { type: string; delta?: string })
      .filter((c) => c.type === "text-delta")
      .map((c) => c.delta)
      .join("");
    expect(expected.length).toBeGreaterThan(0);

    const realFetch = globalThis.fetch;
    globalThis.fetch = (async () =>
      new Response(bytes, {
        status: 200,
        headers: { "content-type": "text/event-stream", "x-vercel-ai-ui-message-stream": "v1" },
      })) as typeof fetch;
    try {
      const transport = makeTransport({ baseUrl: "http://127.0.0.1:1", token: "t", agentId: "a" });
      const chunks = await transport.sendMessages({
        chatId: "c",
        messageId: undefined,
        trigger: "submit-message",
        messages: [{ id: "u", role: "user", parts: [{ type: "text", text: "hi" }] }],
        abortSignal: undefined,
      });
      const errors: unknown[] = [];
      let last: UIMessage | undefined;
      let count = 0;
      for await (const m of readUIMessageStream({ stream: chunks, onError: (e) => errors.push(e), terminateOnError: true })) {
        last = m;
        count++;
      }
      expect(count).toBeGreaterThan(0);
      expect(errors).toEqual([]);
      expect(last?.role).toBe("assistant");
      expect(last?.id).toBe("msg_golden");
      const parts = last!.parts.filter((p) => p.type === "text");
      expect(parts).toHaveLength(1);
      expect((parts[0] as { text: string }).text).toBe(expected);
    } finally {
      globalThis.fetch = realFetch;
    }
  });
});
