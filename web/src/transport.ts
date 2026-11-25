import { DefaultChatTransport, type UIMessage } from "ai";
import { ApiError, apiErrorFromResponse } from "./api";
import type { HostConfig } from "./host";

function lastUserText(messages: UIMessage[]): string {
  const last = [...messages].reverse().find((m) => m.role === "user");
  return (last?.parts ?? []).flatMap((p) => (p.type === "text" ? [p.text] : [])).join("");
}

export function makeTransport(cfg: HostConfig): DefaultChatTransport<UIMessage> {
  return new DefaultChatTransport<UIMessage>({
    api: `${cfg.baseUrl}/v1/agents/${encodeURIComponent(cfg.agentId)}/chat`,
    headers: () => ({ Authorization: `Bearer ${cfg.token}` }),
    prepareSendMessagesRequest: ({ messages }) => ({ body: { text: lastUserText(messages) } }),
    fetch: (async (input: RequestInfo | URL, init?: RequestInit) => {
      let res: Response;
      try {
        res = await globalThis.fetch(input, init);
      } catch (e) {
        if (e instanceof DOMException && e.name === "AbortError") throw e;
        throw new ApiError(0, "network", "Could not reach the daemon.");
      }
      if (!res.ok) throw await apiErrorFromResponse(res);
      return res;
    }) as typeof fetch,
  });
}
