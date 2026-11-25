import { describe, expect, it } from "vitest";
import { cn } from "./utils";

describe("cn", () => {
  it("merges theme surface utilities as colors", () => {
    expect(cn("bg-card", "bg-center-pane")).toBe("bg-center-pane");
    expect(cn("bg-center-pane", "bg-card-2")).toBe("bg-card-2");
    expect(cn("text-card-foreground", "text-center-pane-foreground")).toBe("text-center-pane-foreground");
  });
  it("still treats bg-center as a position", () => {
    expect(cn("bg-center-pane", "bg-center")).toBe("bg-center-pane bg-center");
  });
});
