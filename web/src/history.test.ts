import { describe, expect, it } from "vitest";
import type { ChatMessage } from "./api";
import { toUiMessages } from "./history";

const m = (seq: number, role: ChatMessage["role"], text: string): ChatMessage => ({
  id: `id-${seq}`, agent_id: "a", seq, role, text, created_at: "t",
});

describe("toUiMessages", () => {
  it("skips tool rows and orders by seq", () => {
    const out = toUiMessages([m(3, "assistant", "c"), m(2, "tool", "x"), m(1, "user", "a")]);
    expect(out.map((x) => [x.role, x.id])).toEqual([["user", "id-1"], ["assistant", "id-3"]]);
  });

  it("one text part, text preserved exactly", () => {
    const text = "# Hi\n\n```ts\nconst x = 1;\n```\n  日本語 🎉 é\n";
    const [u] = toUiMessages([m(1, "user", text)]);
    expect(u!.parts).toEqual([{ type: "text", text }]);
  });

  it("keeps consecutive assistant messages separate", () => {
    expect(toUiMessages([m(1, "assistant", "a"), m(2, "assistant", "b")])).toHaveLength(2);
  });

  it("does not mutate input", () => {
    const input = [m(2, "user", "b"), m(1, "user", "a")];
    toUiMessages(input);
    expect(input[0]!.seq).toBe(2);
  });
});
