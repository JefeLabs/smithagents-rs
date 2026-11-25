import type { UIMessage } from "ai";
import type { ChatMessage } from "./api";

export function toUiMessages(messages: ChatMessage[]): UIMessage[] {
  return [...messages]
    .sort((a, b) => a.seq - b.seq)
    .flatMap((m): UIMessage[] =>
      m.role === "tool"
        ? []
        : [{ id: m.id, role: m.role, parts: [{ type: "text", text: m.text }] }],
    );
}
