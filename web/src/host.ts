export type HostConfig = { baseUrl: string; token: string; agentId: string };

export class HostConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "HostConfigError";
  }
}

const LOOPBACK = /^http:\/\/(127\.0\.0\.1|localhost):\d{1,5}$/;

function validate(raw: unknown): HostConfig {
  if (typeof raw !== "object" || raw === null) {
    throw new HostConfigError("Host configuration is missing. Open this view from the SmithAgents desktop app.");
  }
  const o = raw as Record<string, unknown>;
  for (const key of ["baseUrl", "token", "agentId"] as const) {
    if (typeof o[key] !== "string" || o[key] === "") {
      throw new HostConfigError(`Host configuration is invalid: "${key}" must be a non-empty string.`);
    }
  }
  const { baseUrl, token, agentId } = o as HostConfig;
  const port = Number(baseUrl.slice(baseUrl.lastIndexOf(":") + 1));
  if (!LOOPBACK.test(baseUrl) || port < 1 || port > 65535) {
    throw new HostConfigError('Host configuration is invalid: "baseUrl" must be http://127.0.0.1:<port> or http://localhost:<port>.');
  }
  return { baseUrl, token, agentId };
}

export function getHostConfig(): HostConfig {
  const injected = (window as unknown as { __SMITH_HOST__?: unknown }).__SMITH_HOST__;
  if (injected !== undefined) return validate(injected);
  if (import.meta.env.DEV) {
    const q = new URLSearchParams(window.location.search);
    if (q.has("daemon") || q.has("token") || q.has("agent")) {
      try {
        return validate({ baseUrl: q.get("daemon"), token: q.get("token"), agentId: q.get("agent") });
      } catch (e) {
        // The marker string must survive minification if this branch is ever not eliminated.
        throw new HostConfigError(`smith-dev-query-fallback: ${(e as Error).message}`);
      }
    }
  }
  return validate(undefined);
}
