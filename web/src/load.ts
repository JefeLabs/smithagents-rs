import type { UIMessage } from "ai";
import { getAgent, getMessages, type AgentInfo } from "./api";
import { getHostConfig, type HostConfig } from "./host";
import { toUiMessages } from "./history";

export type ChatProps = { cfg: HostConfig; agent: AgentInfo; initialMessages: UIMessage[] };

export async function loadChatProps(readConfig: () => HostConfig = getHostConfig): Promise<ChatProps> {
  const cfg = readConfig();
  const [agent, messages] = await Promise.all([getAgent(cfg), getMessages(cfg)]);
  return { cfg, agent, initialMessages: toUiMessages(messages) };
}
