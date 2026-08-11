// セッション別に AppEvent を束ねる純関数(docs/roadmap.md v0.5: 並行実行とダッシュボード)。
// AppEvent は全イベントに sessionId を含む(docs/architecture.md §4)ため、
// この振り分けだけで tree.ts の buildTree にセッション単位の配列を渡せる
// (buildTree 自体の仕様は変更しない。呼び出し側でセッション分割してから渡す)。

import type { AppEvent } from "./types";

/** sessionId ごとに、出現順を保ったままイベントを振り分ける。 */
export function groupBySession(events: AppEvent[]): Map<string, AppEvent[]> {
  const map = new Map<string, AppEvent[]>();
  for (const ev of events) {
    const list = map.get(ev.sessionId);
    if (list) {
      list.push(ev);
    } else {
      map.set(ev.sessionId, [ev]);
    }
  }
  return map;
}

export interface SessionSummary {
  agentId: string;
  status: "running" | "completed" | "failed" | "cancelled";
  startedAt: string | null;
}

/** 1 セッション分のイベント列(taskStarted で始まる想定)から要約を作る。
 * 実行ビューのタブ・ダッシュボードの帯のラベル用。 */
export function sessionSummary(events: AppEvent[]): SessionSummary {
  let agentId = "";
  let startedAt: string | null = null;
  let status: SessionSummary["status"] = "running";
  for (const ev of events) {
    switch (ev.kind) {
      case "taskStarted":
        agentId = ev.agentId;
        startedAt = ev.startedAt;
        status = "running";
        break;
      case "taskCompleted":
        status = "completed";
        break;
      case "taskFailed":
        status = "failed";
        break;
      case "taskCancelled":
        status = "cancelled";
        break;
    }
  }
  return { agentId, status, startedAt };
}
