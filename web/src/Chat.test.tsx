import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Chat } from "./Chat";
import { HostConfigError } from "./host";
import { App } from "./App";
import { loadChatProps } from "./load";
import type { AgentInfo } from "./api";
import type { UIMessage } from "ai";

const cfg = { baseUrl: "http://127.0.0.1:9", token: "SECRET-TOKEN", agentId: "ag1" };
const agent: AgentInfo = { id: "ag1", name: "Scout", color: { h: 123 }, state: "idle" };
const enc = new TextEncoder();

function sse(chunks: object[], gate?: Promise<void>, split = false): Response {
  const bytes = enc.encode(`: keep-alive\n\n${chunks.map((c) => `data: ${JSON.stringify(c)}\n\n`).join("")}`);
  const body = new ReadableStream<Uint8Array>({
    async start(ctrl) {
      const feed = (b: Uint8Array) => {
        if (!split) return ctrl.enqueue(b);
        for (const x of b) ctrl.enqueue(new Uint8Array([x]));
      };
      feed(bytes);
      if (gate) await gate;
      feed(enc.encode(": keep-alive\n\ndata: [DONE]\n\n"));
      ctrl.close();
    },
  });
  return new Response(body, { status: 200, headers: { "content-type": "text/event-stream", "x-vercel-ai-ui-message-stream": "v1" } });
}
const textStream = (text: string) => [
  { type: "start", messageId: "m1" },
  { type: "start-step" },
  { type: "text-start", id: "t" },
  { type: "text-delta", id: "t", delta: text },
  { type: "text-end", id: "t" },
  { type: "finish-step" },
  { type: "finish" },
];
const history: UIMessage[] = [
  { id: "h1", role: "user", parts: [{ type: "text", text: "earlier question" }] },
  { id: "h2", role: "assistant", parts: [{ type: "text", text: "earlier answer" }] },
];
const mount = (messages: UIMessage[] = history) => render(<Chat cfg={cfg} agent={agent} initialMessages={messages} />);

let fetchMock: ReturnType<typeof vi.fn>;
beforeEach(() => {
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  delete (window as { __smithagentsOpenExternal?: unknown }).__smithagentsOpenExternal;
});

describe("Chat", () => {
  it("renders history, agent name, color hue and border", () => {
    const { container } = mount();
    expect(screen.getByText("earlier question")).toBeTruthy();
    expect(screen.getByText("earlier answer")).toBeTruthy();
    expect(screen.getByText("Scout")).toBeTruthy();
    const root = container.firstElementChild as HTMLElement;
    expect(root.style.getPropertyValue("--agent-h")).toBe("123");
    expect(root.className).toContain("border-l-4");
    expect(root.className).toContain("border-agent");
  });

  it("streams text, posts {text} with bearer, and re-enables the composer after [DONE]", async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    fetchMock.mockResolvedValue(sse(textStream("Hello streamed caf\u00e9 \u2615"), gate, true));
    mount([]);
    const box = screen.getByRole("textbox") as HTMLTextAreaElement;
    await userEvent.type(box, "  hi there  {Enter}");
    await screen.findByText("Hello streamed caf\u00e9 \u2615");
    expect(box.disabled).toBe(true);
    release();
    await waitFor(() => expect(box.disabled).toBe(false));
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe("http://127.0.0.1:9/v1/agents/ag1/chat");
    expect(JSON.parse(init.body)).toEqual({ text: "hi there" });
    expect(init.headers.Authorization ?? init.headers.authorization).toBe("Bearer SECRET-TOKEN");
  });

  it("Shift+Enter inserts a newline and empty input is ignored", async () => {
    mount([]);
    const box = screen.getByRole("textbox") as HTMLTextAreaElement;
    await userEvent.type(box, "a{Shift>}{Enter}{/Shift}b");
    expect(box.value).toBe("a\nb");
    expect(fetchMock).not.toHaveBeenCalled();
    await userEvent.clear(box);
    await userEvent.type(box, "   {Enter}");
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("shows the 409 banner and recovers", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ error: "turn_in_progress", message: "busy" }), { status: 409, headers: { "content-type": "application/json" } }),
    );
    mount([]);
    const box = screen.getByRole("textbox") as HTMLTextAreaElement;
    await userEvent.type(box, "one{Enter}");
    expect(await screen.findByText("This agent is still working on the last message")).toBeTruthy();
    await waitFor(() => expect(box.disabled).toBe(false));
    expect(screen.queryByText("one", { selector: "div" })).toBeNull();
    await waitFor(() => expect(box.value).toBe("one"));
    await userEvent.clear(box);
    await userEvent.type(box, "two");
    fetchMock.mockResolvedValueOnce(sse(textStream("recovered")));
    await userEvent.type(box, "{Enter}");
    await screen.findByText("recovered");
    await waitFor(() => expect(screen.queryByText("This agent is still working on the last message")).toBeNull());
  });

  it("shows other daemon error messages", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ error: "agent_closed", message: "Agent is closed." }), { status: 409 }),
    );
    mount([]);
    await userEvent.type(screen.getByRole("textbox"), "x{Enter}");
    expect(await screen.findByText("Agent is closed.")).toBeTruthy();
  });

  it("shows stream error chunks and re-enables the composer", async () => {
    fetchMock.mockResolvedValueOnce(sse([{ type: "start" }, { type: "error", errorText: "model exploded" }, { type: "finish" }]));
    mount([]);
    const box = screen.getByRole("textbox") as HTMLTextAreaElement;
    await userEvent.type(box, "x{Enter}");
    expect(await screen.findByText("model exploded")).toBeTruthy();
    await waitFor(() => expect(box.disabled).toBe(false));
  });

  it("renders hostile markdown inertly and routes link clicks to the host", async () => {
    const hook = vi.fn();
    (window as { __smithagentsOpenExternal?: (h: string) => void }).__smithagentsOpenExternal = hook;
    const md = "[x](https://example.com)\n\n<script>alert(1)</script>\n\n</script>\n\n[bad](javascript:alert(1))";
    const { container } = mount([{ id: "a", role: "assistant", parts: [{ type: "text", text: md }] }]);
    expect(container.querySelector("script")).toBeNull();
    const links = Array.from(container.querySelectorAll("a"));
    expect(links).toHaveLength(1);
    const a = links[0]!;
    expect(a.getAttribute("href")).toBe("https://example.com");
    expect(a.getAttribute("rel")).toBe("noreferrer noopener");
    expect(a.getAttribute("target")).toBe("_blank");
    expect(container.textContent).toContain("bad");
    expect(container.querySelector('a[href^="javascript"]')).toBeNull();
    const notPrevented = fireEvent.click(a);
    expect(notPrevented).toBe(false);
    expect(hook).toHaveBeenCalledWith("https://example.com");
  });

  it("falls back to window.open when the host hook is absent", () => {
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    const { container } = mount([{ id: "a", role: "assistant", parts: [{ type: "text", text: "[x](https://example.com)" }] }]);
    expect(fireEvent.click(container.querySelector("a")!)).toBe(false);
    expect(open).toHaveBeenCalledWith("https://example.com", "_blank", "noopener,noreferrer");
  });
});

describe("markdown images and links", () => {
  it("never renders an img or a network-capable attribute", () => {
    const md = [
      "![inline](https://evil.example/t.png?leak=1)",
      "![ref][r]",
      "[![linked](https://evil.example/i.png)](https://example.com)",
      "[mail](mailto:a@b.c)",
      "",
      "[r]: https://evil.example/r.png",
    ].join("\n\n");
    const { container } = mount([{ id: "a", role: "assistant", parts: [{ type: "text", text: md }] }]);
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("[src]")).toBeNull();
    expect(container.querySelector('a[href^="mailto"]')).toBeNull();
    expect(container.textContent).toContain("inline");
    expect(container.textContent).toContain("linked");
    expect(container.querySelectorAll("a")).toHaveLength(1);
  });
});

describe("rejoin a working agent", () => {
  it("shows a note, polls until idle, then reloads history", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      let polls = 0;
      fetchMock.mockImplementation(async (url: string) => {
        const j = (b: unknown) => new Response(JSON.stringify(b), { status: 200 });
        if (url.includes("/v1/agents?")) {
          polls++;
          return j([{ ...agent, state: polls < 2 ? "working" : "idle" }]);
        }
        return j([{ id: "n1", agent_id: "ag1", seq: 1, role: "assistant", text: "fresh reply", created_at: "" }]);
      });
      render(<Chat cfg={cfg} agent={{ ...agent, state: "working" }} initialMessages={[]} />);
      expect(screen.getByText("This agent is still working. The reply will appear here when it finishes.")).toBeTruthy();
      expect((screen.getByRole("textbox") as HTMLTextAreaElement).disabled).toBe(true);
      await vi.advanceTimersByTimeAsync(2100);
      expect(screen.queryByText("fresh reply")).toBeNull();
      await vi.advanceTimersByTimeAsync(2100);
      await screen.findByText("fresh reply");
      expect(screen.queryByText(/still working\. The reply/)).toBeNull();
      expect((screen.getByRole("textbox") as HTMLTextAreaElement).disabled).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("scrolling", () => {
  it("scrolls to the bottom on render and when a message arrives", async () => {
    const spy = vi.fn();
    Element.prototype.scrollIntoView = spy;
    try {
      fetchMock.mockResolvedValue(sse(textStream("hi")));
      mount([]);
      expect(spy).toHaveBeenCalled();
      const before = spy.mock.calls.length;
      await userEvent.type(screen.getByRole("textbox"), "x{Enter}");
      await screen.findByText("hi");
      expect(spy.mock.calls.length).toBeGreaterThan(before);
    } finally {
      delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView;
    }
  });
});

describe("click policy", () => {
  it("ignores a non-http href and handles middle-click", () => {
    const hook = vi.fn();
    window.__smithagentsOpenExternal = hook;
    const { container } = mount([{ id: "a", role: "assistant", parts: [{ type: "text", text: "[x](https://example.com)" }] }]);
    const a = container.querySelector("a")!;
    a.setAttribute("href", "javascript:alert(1)");
    fireEvent.click(a);
    expect(hook).not.toHaveBeenCalled();
    a.setAttribute("href", "https://example.com");
    expect(fireEvent(a, new MouseEvent("auxclick", { bubbles: true, cancelable: true, button: 1 }))).toBe(false);
    expect(hook).toHaveBeenCalledWith("https://example.com");
  });
});

describe("App", () => {
  it("renders the error panel for a config failure and never the token", async () => {
    render(<App load={() => loadChatProps(() => { throw new HostConfigError("Host configuration is missing."); })} />);
    await screen.findByText("Could not open this chat");
    expect(screen.getByText("Host configuration is missing.")).toBeTruthy();
    expect(document.body.textContent).not.toContain("SECRET-TOKEN");
  });
});

describe("loadChatProps", () => {
  it("rejects with the config error and never the token", async () => {
    const err = await loadChatProps(() => {
      throw new HostConfigError("Host configuration is missing.");
    }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(HostConfigError);
    expect((err as Error).message).not.toContain("SECRET-TOKEN");
  });
});
