import { useChat } from "@ai-sdk/react";
import type { UIMessage } from "ai";
import { useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent, type MouseEvent, type UIEvent } from "react";
import { ApiError, getAgent, getMessages, type AgentInfo } from "./api";
import { toUiMessages } from "./history";
import type { HostConfig } from "./host";
import { isSafeHref, Markdown } from "./Markdown";
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
  // auxclick also fires for the right button: only the middle button opens the link.
  if (event.type === "auxclick" && event.button !== 1) return;
  const href = anchor.getAttribute("href");
  if (!isSafeHref(href)) return;
  if (window.__smithagentsOpenExternal) window.__smithagentsOpenExternal(href);
  else window.open(href, "_blank", "noopener,noreferrer");
}

const POLL_MS = 2000;
const isWorking = (state: string) => state === "working" || state === "quiet";

export function Chat({ cfg, agent, initialMessages }: { cfg: HostConfig; agent: AgentInfo; initialMessages: UIMessage[] }) {
  const transport = useMemo(() => makeTransport(cfg), [cfg]);
  const [draft, setDraft] = useState("");
  const lastSent = useRef("");
  const { messages, sendMessage, setMessages, status, error, clearError } = useChat({
    transport,
    messages: initialMessages,
    onError: (e) => {
      if (!(e instanceof ApiError)) return;
      if (e.status > 0) {
        // The daemon answered with a rejection before any stream: it did not accept the message.
        setMessages((ms) => (ms[ms.length - 1]?.role === "user" ? ms.slice(0, -1) : ms));
        setDraft(lastSent.current);
      } else {
        // Network failure: the daemon may already have stored the message. Keep the bubble
        // and resync once with what the daemon has.
        void getMessages(cfg).then((h) => setMessages(toUiMessages(h)), () => {});
      }
    },
  });
  const [pollError, setPollError] = useState<string | null>(null);
  const [agentWorking, setAgentWorking] = useState(isWorking(agent.state));
  const busy = status === "submitted" || status === "streaming" || agentWorking;

  useEffect(() => {
    if (!agentWorking || pollError !== null) return;
    let live = true;
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      try {
        const a = await getAgent(cfg);
        if (!live) return;
        if (!isWorking(a.state)) {
          const history = toUiMessages(await getMessages(cfg));
          if (!live) return;
          setMessages(history);
          setAgentWorking(false);
          return;
        }
      } catch (e) {
        if (!live) return;
        if (e instanceof ApiError && (e.status === 401 || e.status === 404)) {
          setPollError(e.message);
          return;
        }
        // transient: keep polling
      }
      timer = setTimeout(() => void tick(), POLL_MS);
    };
    timer = setTimeout(() => void tick(), POLL_MS);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [agentWorking, pollError, cfg, setMessages]);

  const stick = useRef(true);
  const bottom = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (stick.current) bottom.current?.scrollIntoView?.({ block: "end" });
  }, [messages]);
  const onScroll = (e: UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };

  const submit = () => {
    const text = draft.trim();
    if (!text || busy) return;
    lastSent.current = text;
    setDraft("");
    clearError();
    stick.current = true;
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
      onAuxClick={openExternal}
    >
      <header className="border-border bg-card border-b px-4 py-2 font-semibold">{agent.name}</header>
      <div className="flex-1 space-y-3 overflow-y-auto p-4" onScroll={onScroll}>
        {messages.map((m) => (
          <div key={m.id} className={m.role === "user" ? "flex justify-end" : "flex justify-start"}>
            <div className={m.role === "user" ? "bg-muted max-w-[80%] rounded-lg px-3 py-2 whitespace-pre-wrap" : "max-w-[80%] px-1 py-2"}>
              {m.role === "user" ? textOf(m) : <Markdown text={textOf(m)} />}
            </div>
          </div>
        ))}
        <div ref={bottom} />
      </div>
      {agentWorking && pollError === null && (
        <div role="status" className="text-muted-foreground border-border border-t px-4 py-2 text-sm">
          This agent is still working. The reply will appear here when it finishes.
        </div>
      )}
      {pollError !== null && (
        <div role="alert" className="text-status-error border-border border-t px-4 py-2 text-sm">
          {pollError}
        </div>
      )}
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
