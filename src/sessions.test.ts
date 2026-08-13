import { describe, expect, it } from "vitest";
import { groupBySession, sessionSummary } from "./sessions";
import type { AppEvent } from "./types";

describe("groupBySession", () => {
  it("sessionId ごとに出現順を保って振り分ける", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "a", startedAt: "t0", prompt: "依頼" },
      { kind: "taskStarted", sessionId: "s2", agentId: "b", startedAt: "t0", prompt: "依頼" },
      { kind: "agentIntent", sessionId: "s1", agentId: null, text: "作業中" },
      { kind: "taskCompleted", sessionId: "s1", summary: "ok", outputFiles: [] },
      { kind: "taskFailed", sessionId: "s2", error: "だめでした" },
    ];
    const grouped = groupBySession(events);
    expect([...grouped.keys()]).toEqual(["s1", "s2"]);
    expect(grouped.get("s1")?.map((e) => e.kind)).toEqual(["taskStarted", "agentIntent", "taskCompleted"]);
    expect(grouped.get("s2")?.map((e) => e.kind)).toEqual(["taskStarted", "taskFailed"]);
  });

  it("空配列には何も含まれない", () => {
    expect(groupBySession([]).size).toBe(0);
  });
});

describe("sessionSummary", () => {
  it("taskStarted のみなら running", () => {
    const events: AppEvent[] = [{ kind: "taskStarted", sessionId: "s1", agentId: "survey-analyst", startedAt: "t0", prompt: "依頼" }];
    expect(sessionSummary(events)).toEqual({ agentId: "survey-analyst", status: "running", startedAt: "t0" });
  });

  it("taskCompleted で completed になる", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "a", startedAt: "t0", prompt: "依頼" },
      { kind: "taskCompleted", sessionId: "s1", summary: "ok", outputFiles: [] },
    ];
    expect(sessionSummary(events).status).toBe("completed");
  });

  it("taskFailed で failed になる", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "a", startedAt: "t0", prompt: "依頼" },
      { kind: "taskFailed", sessionId: "s1", error: "エラー" },
    ];
    expect(sessionSummary(events).status).toBe("failed");
  });

  it("taskCancelled で cancelled になる", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "a", startedAt: "t0", prompt: "依頼" },
      { kind: "taskCancelled", sessionId: "s1" },
    ];
    expect(sessionSummary(events).status).toBe("cancelled");
  });
});
