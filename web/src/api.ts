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
const MAX_PAGES = 1000;

const bad = (what: string) => new ApiError(0, "bad_response", `The daemon returned a malformed ${what}.`);

/** Turns a non-2xx daemon response (`{error, message}` JSON) into an ApiError. */
export async function apiErrorFromResponse(res: Response): Promise<ApiError> {
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
  return new ApiError(res.status, code, message);
}

async function request(cfg: HostConfig, path: string): Promise<unknown> {
  let res: Response;
  try {
    res = await fetch(`${cfg.baseUrl}${path}`, { headers: { Authorization: `Bearer ${cfg.token}` } });
  } catch {
    throw new ApiError(0, "network", "Could not reach the daemon.");
  }
  if (!res.ok) throw await apiErrorFromResponse(res);
  try {
    return await res.json();
  } catch {
    throw new ApiError(res.status, "unknown", "The daemon returned an unreadable response.");
  }
}

export async function getAgent(cfg: HostConfig): Promise<AgentInfo> {
  const agents = await request(cfg, "/v1/agents?include_closed=true");
  if (!Array.isArray(agents)) throw bad("agent list");
  const a = (agents as unknown[]).find(
    (x) => typeof x === "object" && x !== null && (x as { id?: unknown }).id === cfg.agentId,
  ) as Record<string, unknown> | undefined;
  if (!a) throw new ApiError(404, "agent_not_found", "This agent no longer exists.");
  const color = a.color as { h?: unknown } | null | undefined;
  if (
    typeof a.name !== "string" ||
    typeof a.state !== "string" ||
    typeof color !== "object" || color === null ||
    typeof color.h !== "number" || !Number.isFinite(color.h)
  ) {
    throw bad("agent");
  }
  return { id: cfg.agentId, name: a.name, color: { h: color.h }, state: a.state as AgentState };
}

export async function getMessages(cfg: HostConfig): Promise<ChatMessage[]> {
  const all: ChatMessage[] = [];
  let after = 0;
  for (let pages = 0; pages < MAX_PAGES; pages++) {
    const page = (await request(
      cfg,
      `/v1/agents/${encodeURIComponent(cfg.agentId)}/messages?after=${after}&limit=${PAGE_LIMIT}`,
    )) as ChatMessage[];
    if (!Array.isArray(page)) throw bad("message list");
    all.push(...page);
    if (page.length < PAGE_LIMIT) return all;
    const last = page[page.length - 1]!.seq;
    if (typeof last !== "number" || last <= after) throw bad("message page (no progress)");
    after = last;
  }
  throw bad("message history (too many pages)");
}
