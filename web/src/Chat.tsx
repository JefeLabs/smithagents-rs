import { useChat } from "@ai-sdk/react";
import type { UIMessage } from "ai";
import { useMemo, useState, type CSSProperties, type KeyboardEvent, type MouseEvent } from "react";
import { ApiError, type AgentInfo } from "./api";
import type { HostConfig } from "./host";
import { Markdown } from "./Markdown";
import { makeTransport } from "./transport";

declare global {
  interface Window {
    __smithagentsOpenExternal?: (href: string) => void;
  }
}

function errorText(error: Error): string {
  if (error instanceof ApiError) {
    return error.code === "turn_in_progress" ? "This agent is still working on the last message" : error.message;
  }
  return error.message || "Something went wrong.";
}

function textOf(m: UIMessage): string {
  return m.parts.flatMap((p) => (p.type === "text" ? [p.text] : [])).join("");
}

function openExternal(event: MouseEvent) {
  const anchor = (event.target as Element).closest("a");
  if (!anchor) return;
  event.preventDefault();
  const href = anchor.getAttribute("href");
  if (!href) return;
  if (window.__smithagentsOpenExternal) window.__smithagentsOpenExternal(href);
  else window.open(href, "_blank", "noopener,noreferrer");
}

export function Chat({ cfg, agent, initialMessages }: { cfg: HostConfig; agent: AgentInfo; initialMessages: UIMessage[] }) {
  const transport = useMemo(() => makeTransport(cfg), [cfg]);
  const { messages, sendMessage, status, error, clearError } = useChat({ transport, messages: initialMessages });
  const [draft, setDraft] = useState("");
  const busy = status === "submitted" || status === "streaming";

  const submit = () => {
    const text = draft.trim();
    if (!text || busy) return;
    setDraft("");
    clearError();
    void sendMessage({ text });
  };
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
    }
  };

  return (
    <div
      className="bg-background text-foreground border-agent flex h-screen flex-col border-l-4"
      style={{ "--agent-h": agent.color.h } as CSSProperties}
      onClick={openExternal}
    >
      <header className="border-border bg-card border-b px-4 py-2 font-semibold">{agent.name}</header>
      <div className="flex-1 space-y-3 overflow-y-auto p-4">
        {messages.map((m) => (
          <div key={m.id} className={m.role === "user" ? "flex justify-end" : "flex justify-start"}>
            <div className={m.role === "user" ? "bg-muted max-w-[80%] rounded-lg px-3 py-2 whitespace-pre-wrap" : "max-w-[80%] px-1 py-2"}>
              {m.role === "user" ? textOf(m) : <Markdown text={textOf(m)} />}
            </div>
          </div>
        ))}
      </div>
      {error && (
        <div role="alert" className="text-status-error border-border border-t px-4 py-2 text-sm">
          {errorText(error)}
        </div>
      )}
      <div className="border-border border-t p-3">
        <textarea
          aria-label="Message"
          className="bg-card border-border focus-visible:ring-ring w-full resize-none rounded-md border p-2 outline-none focus-visible:ring-2 disabled:opacity-60"
          rows={2}
          value={draft}
          disabled={busy}
          placeholder="Message the agent (Enter to send, Shift+Enter for a new line)"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKeyDown}
        />
      </div>
    </div>
  );
}
