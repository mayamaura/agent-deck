import { describe, expect, it } from "vitest";
import { buildTree } from "./tree";
import type { AppEvent } from "./types";

const noResponses = new Set<string>();

describe("buildTree", () => {
  it("メインのみの正常完了", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "survey-analyst", startedAt: "2026-08-12T00:00:00Z", prompt: "依頼" },
      { kind: "agentIntent", sessionId: "s1", agentId: null, text: "集計しています" },
      { kind: "taskCompleted", sessionId: "s1", summary: "完了しました", outputFiles: ["report.md"] },
    ];
    const tree = buildTree(events, noResponses);

    expect(tree.taskStatus).toBe("completed");
    expect(tree.summary).toBe("完了しました");
    expect(tree.outputFiles).toEqual(["report.md"]);
    expect(tree.main).not.toBeNull();
    expect(tree.main?.label).toBe("survey-analyst");
    expect(tree.main?.isMain).toBe(true);
    expect(tree.main?.status).toBe("completed");
    expect(tree.main?.currentIntent).toBe("集計しています");
    expect(tree.subagents).toEqual([]);
  });

  it("サブ開始→ツール→完了(duration・tokens 反映)", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "coordinator", startedAt: "2026-08-12T00:00:00Z", prompt: "依頼" },
      { kind: "subagentStarted", sessionId: "s1", agentId: "sub-1", toolCallId: "call-1", displayName: "data-cruncher" },
      { kind: "toolStarted", sessionId: "s1", agentId: "sub-1", toolCallId: "t-1", toolName: "read" },
      { kind: "toolCompleted", sessionId: "s1", agentId: "sub-1", toolCallId: "t-1", toolName: "read", success: true },
      {
        kind: "subagentCompleted",
        sessionId: "s1",
        agentId: "sub-1",
        toolCallId: "call-1",
        durationMs: 64000,
        totalTokens: 1200,
      },
      { kind: "taskCompleted", sessionId: "s1", summary: "ok", outputFiles: [] },
    ];
    const tree = buildTree(events, noResponses);

    expect(tree.subagents).toHaveLength(1);
    const sub = tree.subagents[0];
    expect(sub.key).toBe("sub-1");
    expect(sub.label).toBe("data-cruncher");
    expect(sub.isMain).toBe(false);
    expect(sub.status).toBe("completed");
    expect(sub.durationMs).toBe(64000);
    expect(sub.totalTokens).toBe(1200);
    expect(sub.tools).toEqual([{ toolCallId: "t-1", toolName: "read", status: "ok" }]);
  });

  it("サブ失敗", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "coordinator", startedAt: "2026-08-12T00:00:00Z", prompt: "依頼" },
      { kind: "subagentStarted", sessionId: "s1", agentId: "sub-1", toolCallId: "call-1", displayName: "helper" },
      { kind: "subagentFailed", sessionId: "s1", agentId: "sub-1", toolCallId: "call-1", error: "モデル呼び出しに失敗しました" },
      { kind: "taskFailed", sessionId: "s1", error: "サブエージェントの失敗により中断しました" },
    ];
    const tree = buildTree(events, noResponses);

    expect(tree.subagents).toHaveLength(1);
    expect(tree.subagents[0].status).toBe("failed");
    expect(tree.subagents[0].error).toBe("モデル呼び出しに失敗しました");
    expect(tree.taskStatus).toBe("failed");
    expect(tree.taskError).toBe("サブエージェントの失敗により中断しました");
    // メインもタスク失敗時に error が反映される。
    expect(tree.main?.error).toBe("サブエージェントの失敗により中断しました");
    expect(tree.main?.status).toBe("failed");
  });

  it("権限要求の行 attach と応答済み除外", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "coordinator", startedAt: "2026-08-12T00:00:00Z", prompt: "依頼" },
      { kind: "subagentStarted", sessionId: "s1", agentId: "sub-1", toolCallId: "call-1", displayName: "writer" },
      { kind: "toolStarted", sessionId: "s1", agentId: "sub-1", toolCallId: "t-1", toolName: "write" },
      {
        kind: "permissionRequested",
        sessionId: "s1",
        requestId: "req-1",
        permissionKind: "write",
        detail: "out.md",
        suggestedPattern: null,
      },
    ];

    const pending = buildTree(events, noResponses);
    // 直近の活動はサブ(sub-1)への toolStarted なので、サブ行に積まれる。
    expect(pending.main?.pendingPermissions).toEqual([]);
    expect(pending.subagents[0].pendingPermissions).toEqual([
      { requestId: "req-1", permissionKind: "write", detail: "out.md", suggestedPattern: null },
    ]);

    const responded = buildTree(events, new Set(["req-1"]));
    expect(responded.subagents[0].pendingPermissions).toEqual([]);
  });

  it("権限要求はメイン活動中ならメイン行に積まれる", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "writer", startedAt: "2026-08-12T00:00:00Z", prompt: "依頼" },
      { kind: "toolStarted", sessionId: "s1", agentId: null, toolCallId: "t-1", toolName: "write" },
      {
        kind: "permissionRequested",
        sessionId: "s1",
        requestId: "req-1",
        permissionKind: "write",
        detail: "out.md",
        suggestedPattern: "shell(python)",
      },
    ];
    const tree = buildTree(events, noResponses);
    expect(tree.main?.pendingPermissions).toHaveLength(1);
  });

  it("開始無しの toolCompleted は行を新規生成する", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "writer", startedAt: "2026-08-12T00:00:00Z", prompt: "依頼" },
      { kind: "toolCompleted", sessionId: "s1", agentId: null, toolCallId: "t-9", toolName: "write", success: false },
    ];
    const tree = buildTree(events, noResponses);
    expect(tree.main?.tools).toEqual([{ toolCallId: "t-9", toolName: "write", status: "failed" }]);
  });

  it("cancelled: 実行中の行は中断状態に確定する", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "coordinator", startedAt: "2026-08-12T00:00:00Z", prompt: "依頼" },
      { kind: "subagentStarted", sessionId: "s1", agentId: "sub-1", toolCallId: "call-1", displayName: "helper" },
      { kind: "taskCancelled", sessionId: "s1" },
    ];
    const tree = buildTree(events, noResponses);
    expect(tree.taskStatus).toBe("cancelled");
    expect(tree.main?.status).toBe("cancelled");
    expect(tree.subagents[0].status).toBe("cancelled");
  });

  it("usage の最新値反映", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "coordinator", startedAt: "2026-08-12T00:00:00Z", prompt: "依頼" },
      { kind: "usageUpdated", sessionId: "s1", currentTokens: 100, tokenLimit: null },
      { kind: "usageUpdated", sessionId: "s1", currentTokens: 4200, tokenLimit: 128000 },
    ];
    const tree = buildTree(events, noResponses);
    expect(tree.usage).toEqual({ currentTokens: 4200, tokenLimit: 128000 });
  });

  it("taskStarted が来ていない間は idle のまま", () => {
    const tree = buildTree([], noResponses);
    expect(tree.taskStatus).toBe("idle");
    expect(tree.main).toBeNull();
  });

  it("継続依頼(同一セッションで2回目の taskStarted)は過去ターンとして退避される", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "writer", startedAt: "2026-08-12T00:00:00Z", prompt: "最初の依頼" },
      { kind: "taskCompleted", sessionId: "s1", summary: "1回目の結果", outputFiles: ["a.md"] },
      { kind: "taskStarted", sessionId: "s1", agentId: "writer", startedAt: "2026-08-12T00:05:00Z", prompt: "続きの依頼" },
      { kind: "taskCompleted", sessionId: "s1", summary: "2回目の結果", outputFiles: [] },
    ];
    const tree = buildTree(events, noResponses);

    expect(tree.turns).toEqual([
      { prompt: "最初の依頼", summary: "1回目の結果", status: "completed", startedAt: "2026-08-12T00:00:00Z" },
    ]);
    expect(tree.prompt).toBe("続きの依頼");
    expect(tree.summary).toBe("2回目の結果");
    expect(tree.taskStatus).toBe("completed");
  });

  it("失敗したターンの退避は taskError を summary として残す", () => {
    const events: AppEvent[] = [
      { kind: "taskStarted", sessionId: "s1", agentId: "writer", startedAt: "2026-08-12T00:00:00Z", prompt: "最初の依頼" },
      { kind: "taskFailed", sessionId: "s1", error: "途中で失敗" },
      { kind: "taskStarted", sessionId: "s1", agentId: "writer", startedAt: "2026-08-12T00:05:00Z", prompt: "リトライして" },
    ];
    const tree = buildTree(events, noResponses);

    expect(tree.turns).toEqual([
      { prompt: "最初の依頼", summary: "途中で失敗", status: "failed", startedAt: "2026-08-12T00:00:00Z" },
    ]);
    expect(tree.taskStatus).toBe("running");
  });
});
