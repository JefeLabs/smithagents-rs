import type { HostConfig } from "./host";

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

export type AgentState = "idle" | "working" | "quiet" | "done" | "dead";

/** Only the hue is exposed: lightness and chroma come from the theme. */
export type AgentInfo = { id: string; name: string; color: { h: number }; state: AgentState };

export type ChatMessage = {
  id: string;
  agent_id: string;
  seq: number;
  role: "user" | "assistant" | "tool";
  text: string;
  created_at: string;
};

const PAGE_LIMIT = 1000; // daemon MAX_MESSAGE_LIMIT

async function request(cfg: HostConfig, path: string): Promise<unknown> {
  let res: Response;
  try {
    res = await fetch(`${cfg.baseUrl}${path}`, { headers: { Authorization: `Bearer ${cfg.token}` } });
  } catch {
    throw new ApiError(0, "network", "Could not reach the daemon.");
  }
  if (!res.ok) {
    let code = "unknown";
    let message = `Request failed (${res.status}).`;
    try {
      const body: unknown = await res.json();
      if (typeof body === "object" && body !== null) {
        const b = body as { error?: unknown; message?: unknown };
        if (typeof b.error === "string") code = b.error;
        if (typeof b.message === "string") message = b.message;
      }
    } catch {
      // non-JSON body: keep defaults
    }
    throw new ApiError(res.status, code, message);
  }
  try {
    return await res.json();
  } catch {
    throw new ApiError(res.status, "unknown", "The daemon returned an unreadable response.");
  }
}

type RawAgent = { id: string; name: string; color: { h: number }; state: AgentState };

export async function getAgent(cfg: HostConfig): Promise<AgentInfo> {
  const agents = (await request(cfg, "/v1/agents?include_closed=true")) as RawAgent[];
  const a = agents.find((x) => x.id === cfg.agentId);
  if (!a) throw new ApiError(404, "agent_not_found", "This agent no longer exists.");
  return { id: a.id, name: a.name, color: { h: a.color.h }, state: a.state };
}

export async function getMessages(cfg: HostConfig): Promise<ChatMessage[]> {
  const all: ChatMessage[] = [];
  let after = 0;
  for (;;) {
    const page = (await request(
      cfg,
      `/v1/agents/${encodeURIComponent(cfg.agentId)}/messages?after=${after}&limit=${PAGE_LIMIT}`,
    )) as ChatMessage[];
    all.push(...page);
    if (page.length < PAGE_LIMIT) return all;
    after = page[page.length - 1]!.seq;
  }
}
