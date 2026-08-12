// 実行ビューのツリー構築(docs/development.md ステップ5、docs/requirements.md §3.3)。
// AppEvent の配列から純粋にツリー状態を組み立てる。副作用・SDK 型は持ち込まない。
// 経過時間の「実行中は1秒ごとに更新」表示は、AppEvent に壁時計のタイムスタンプが
// 無い(TaskStarted.startedAt 以外は無い)ため、この関数の責務にしない。
// App.tsx 側が受信時刻とインターバルで計算する(表示だけの関心事のため)。

import type { AppEvent } from "./types";

export interface ToolRow {
  toolCallId: string;
  toolName: string;
  status: "running" | "ok" | "failed";
}

export interface AgentRow {
  key: string; // メインは "main"、サブは copilot.rs が解決した agent_id
  label: string;
  isMain: boolean;
  status: "running" | "completed" | "failed" | "cancelled";
  currentIntent: string | null;
  tools: ToolRow[];
  durationMs: number | null;
  totalTokens: number | null;
  error: string | null;
  pendingPermissions: {
    requestId: string;
    permissionKind: string;
    detail: string;
    // 「常に許可」ボタンに使うパターン(copilot::suggest_allow_pattern が提案)。
    // null なら App.tsx はボタンを出さない(write は無条件許可を提案しない設計)。
    suggestedPattern: string | null;
  }[];
  // エージェントが ask_user ツールで質問した(v1.0 経路A)。pendingPermissions と同じ
  // 「直近に活動した行」ヒューリスティックで所有行を決める。
  pendingUserInputs: {
    requestId: string;
    question: string;
    choices: string[];
    allowFreeform: boolean;
  }[];
}

export interface TreeState {
  main: AgentRow | null;
  subagents: AgentRow[]; // 出現順
  usage: { currentTokens: number; tokenLimit: number | null } | null;
  taskStatus: "idle" | "running" | "completed" | "failed" | "cancelled";
  summary: string | null;
  taskError: string | null;
  outputFiles: string[];
  startedAt: string | null;
}

function newRow(key: string, label: string, isMain: boolean): AgentRow {
  return {
    key,
    label,
    isMain,
    status: "running",
    currentIntent: null,
    tools: [],
    durationMs: null,
    totalTokens: null,
    error: null,
    pendingPermissions: [],
    pendingUserInputs: [],
  };
}

/** key が "main" なら main 行、それ以外はサブ行(無ければ生成)を返す。 */
function rowByKey(state: TreeState, key: string, fallbackLabel: string): AgentRow {
  if (key === "main") {
    if (!state.main) {
      state.main = newRow("main", fallbackLabel, true);
    }
    return state.main;
  }
  const found = state.subagents.find((r) => r.key === key);
  if (found) return found;
  const row = newRow(key, fallbackLabel, false);
  state.subagents.push(row);
  return row;
}

/** AppEvent の agentId(null = メイン)からツリーの行を引く共通ルール。 */
function rowFor(state: TreeState, agentId: string | null): AgentRow {
  return agentId == null ? rowByKey(state, "main", "main") : rowByKey(state, agentId, agentId);
}

function finalizeRunningRows(state: TreeState, status: AgentRow["status"]) {
  if (state.main && state.main.status === "running") state.main.status = status;
  for (const row of state.subagents) {
    if (row.status === "running") row.status = status;
  }
}

function upsertTool(row: AgentRow, toolCallId: string, toolName: string, status: ToolRow["status"]) {
  const found = row.tools.find((t) => t.toolCallId === toolCallId);
  if (found) {
    found.status = status;
    return;
  }
  // 開始イベントを取りこぼしていても完了イベント単独で行を作る(耐性)。
  row.tools.push({ toolCallId, toolName, status });
}

/**
 * AppEvent の列と、応答済み権限要求 requestId の集合からツリー状態を組み立てる。
 *
 * respondedRequestIds が必要な理由: respond_permission コマンドは AppEvent を
 * 発行しない(architecture.md §3 の戻り値は () のみ)ため、「応答済みかどうか」は
 * イベント列だけからは判定できない。呼び出し側(App.tsx)が応答時に requestId を
 * 記録し、ここへ渡して pendingPermissions から除外する。
 */
export function buildTree(events: AppEvent[], respondedRequestIds: ReadonlySet<string>): TreeState {
  const state: TreeState = {
    main: null,
    subagents: [],
    usage: null,
    taskStatus: "idle",
    summary: null,
    taskError: null,
    outputFiles: [],
    startedAt: null,
  };

  // PermissionRequested には所有エージェントの相関 ID が無い(events.rs 参照。
  // PermissionRequestData の型付きフィールドに agent_id 相当が無いため)。
  // 直近に活動した行を担当行とみなすヒューリスティックで補う。
  // v0.1 はサブエージェントの委任も含め逐次実行(複数タスクの並行実行はスコープ外
  // = requirements.md §2.2)なので、ある時点で活動しているのは常に1行という前提が
  // 成立する。
  // ponytail: 将来 SDK が並行サブエージェントの権限要求を同時に出すようになったら
  // このヒューリスティックは破綻する。そのときは PermissionRequestData の
  // tool_call_id を PermissionRequested に足して相関する方式に upgrade する。
  let activeKey = "main";

  for (const ev of events) {
    switch (ev.kind) {
      case "taskStarted": {
        if (state.taskStatus !== "idle") {
          // 同一配列に複数実行分のイベントが混ざっていても直近の実行だけを反映する
          // (App.tsx は通常 taskStarted ごとにログをリセットするが、念のための防御)。
          state.main = null;
          state.subagents = [];
          state.usage = null;
          state.summary = null;
          state.taskError = null;
          state.outputFiles = [];
        }
        const row = rowByKey(state, "main", ev.agentId);
        row.label = ev.agentId;
        row.status = "running";
        state.startedAt = ev.startedAt;
        state.taskStatus = "running";
        activeKey = "main";
        break;
      }
      case "agentIntent": {
        rowFor(state, ev.agentId).currentIntent = ev.text;
        activeKey = ev.agentId ?? "main";
        break;
      }
      case "subagentStarted": {
        const row = rowByKey(state, ev.agentId, ev.displayName);
        row.label = ev.displayName;
        row.status = "running";
        activeKey = ev.agentId;
        break;
      }
      case "subagentCompleted": {
        const row = rowByKey(state, ev.agentId, ev.agentId);
        row.status = "completed";
        row.durationMs = ev.durationMs;
        row.totalTokens = ev.totalTokens;
        activeKey = "main";
        break;
      }
      case "subagentFailed": {
        const row = rowByKey(state, ev.agentId, ev.agentId);
        row.status = "failed";
        row.error = ev.error;
        activeKey = "main";
        break;
      }
      case "toolStarted": {
        upsertTool(rowFor(state, ev.agentId), ev.toolCallId, ev.toolName, "running");
        activeKey = ev.agentId ?? "main";
        break;
      }
      case "toolCompleted": {
        upsertTool(rowFor(state, ev.agentId), ev.toolCallId, ev.toolName, ev.success ? "ok" : "failed");
        activeKey = ev.agentId ?? "main";
        break;
      }
      case "permissionRequested": {
        if (!respondedRequestIds.has(ev.requestId)) {
          rowByKey(state, activeKey, activeKey).pendingPermissions.push({
            requestId: ev.requestId,
            permissionKind: ev.permissionKind,
            detail: ev.detail,
            suggestedPattern: ev.suggestedPattern,
          });
        }
        break;
      }
      case "userInputRequested": {
        if (!respondedRequestIds.has(ev.requestId)) {
          rowByKey(state, activeKey, activeKey).pendingUserInputs.push({
            requestId: ev.requestId,
            question: ev.question,
            choices: ev.choices,
            allowFreeform: ev.allowFreeform,
          });
        }
        break;
      }
      case "usageUpdated": {
        state.usage = { currentTokens: ev.currentTokens, tokenLimit: ev.tokenLimit };
        break;
      }
      case "taskCompleted": {
        state.taskStatus = "completed";
        state.summary = ev.summary;
        state.outputFiles = ev.outputFiles;
        finalizeRunningRows(state, "completed");
        break;
      }
      case "taskFailed": {
        state.taskStatus = "failed";
        state.taskError = ev.error;
        if (state.main) state.main.error = ev.error;
        finalizeRunningRows(state, "failed");
        break;
      }
      case "taskCancelled": {
        state.taskStatus = "cancelled";
        finalizeRunningRows(state, "cancelled");
        break;
      }
    }
  }

  return state;
}
