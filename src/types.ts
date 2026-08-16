// Rust 側の型のミラー。変更時は src-tauri/src/ の対応する型と同期すること。

// src-tauri/src/agents.rs AgentScope
export type AgentScope = "personal" | "shared";

// src-tauri/src/agents.rs AgentSummary
export interface AgentSummary {
  id: string;
  name: string;
  description: string;
  sourcePath: string;
  scope: AgentScope;
  // 共有定義のバージョン(sha256 先頭8桁)。個人定義は null。
  version: string | null;
  // true = 同名の個人定義に隠されている(実行には使われないが一覧には残る)。
  shadowed: boolean;
}

// src-tauri/src/main.rs AgentDefinitionDto(get_agent_definition の戻り値)
export interface AgentDefinitionDto {
  id: string;
  name: string;
  description: string;
  tools: string[] | null;
  model: string | null;
  body: string;
  scope: AgentScope;
  version: string | null;
  sourcePath: string;
}

// src-tauri/src/copilot.rs DraftedAgent(draft_agent_definition の戻り値。docs/roadmap.md v1.1)
// model は含まない(古いモデル名を書かれるのを避けるため生成させない)。
export interface DraftedAgent {
  name: string;
  description: string;
  tools: string[] | null;
  body: string;
}

// src-tauri/src/sync.rs SyncSummary(sync_shared_agents_cmd の戻り値)
export interface SyncSummary {
  added: number;
  updated: number;
  removed: number;
  syncedAt: string;
}

// src-tauri/src/main.rs AppConfigDto(get_app_config の戻り値)
export interface AppConfigDto {
  sharedAgentsSource: string | null;
  defaultModel: string | null;
  updateSource: string | null;
  currentVersion: string;
  // 管理者ポリシー(data/policy.json)の forcedDeniedTools(docs/roadmap.md v0.6)。
  // 空配列なら UI 側で非表示にする。設定画面からは変更できない(表示のみ)。
  forcedDeniedTools: string[];
}

// src-tauri/src/main.rs UpdateInfoDto(check_for_updates の戻り値)
export interface UpdateInfoDto {
  version: string;
  notes: string;
  hashOk: boolean;
}

// src-tauri/src/config.rs AgentSettings
export interface AgentSettings {
  inputDir: string | null;
  outputDir: string | null;
  allowedTools: string[];
  deniedTools: string[];
  autoApproveWriteInOutputDir: boolean;
}

// src-tauri/src/history.rs HistoryEntry
export interface HistoryEntry {
  sessionId: string;
  agentId: string;
  prompt: string;
  startedAt: string;
  durationMs: number;
  status: "completed" | "failed" | "cancelled";
  outputFiles: string[];
  totalTokens: number | null;
  // 完了時の最終メッセージ(失敗時はエラー文)。旧履歴行には無いため空文字のことがある。
  summary: string;
  subagents: { name: string; durationMs: number }[];
  // "manual" / "scheduled"(docs/roadmap.md v0.4)。
  trigger: "manual" | "scheduled";
}

// src-tauri/src/schedule.rs Recurrence(weekday: 0=日〜6=土)
export type Recurrence =
  | { type: "daily"; time: string }
  | { type: "weekly"; weekday: number; time: string }
  | { type: "monthly"; day: number; time: string };

// src-tauri/src/schedule.rs Schedule(data/schedules.json の1件)
export interface Schedule {
  id: string;
  agentId: string;
  prompt: string;
  recurrence: Recurrence;
  enabled: boolean;
  lastRunAt: string | null;
}

// src-tauri/src/main.rs QueueStatusDto(get_queue_status の戻り値)
export interface QueueStatusDto {
  queued: number;
}

// src-tauri/src/events.rs AppEvent(kind でタグ付けされた単一チャネル "agent://event")
export type AppEvent =
  | { kind: "taskStarted"; sessionId: string; agentId: string; startedAt: string; prompt: string }
  | { kind: "agentIntent"; sessionId: string; agentId: string | null; text: string }
  | { kind: "subagentStarted"; sessionId: string; agentId: string; toolCallId: string; displayName: string }
  | { kind: "subagentCompleted"; sessionId: string; agentId: string; toolCallId: string; durationMs: number; totalTokens: number | null }
  | { kind: "subagentFailed"; sessionId: string; agentId: string; toolCallId: string; error: string }
  | { kind: "toolStarted"; sessionId: string; agentId: string | null; toolCallId: string; toolName: string }
  | { kind: "toolCompleted"; sessionId: string; agentId: string | null; toolCallId: string; toolName: string; success: boolean }
  | { kind: "permissionRequested"; sessionId: string; requestId: string; permissionKind: string; detail: string; suggestedPattern: string | null }
  | { kind: "usageUpdated"; sessionId: string; currentTokens: number; tokenLimit: number | null }
  | { kind: "taskCompleted"; sessionId: string; summary: string; outputFiles: string[] }
  | { kind: "taskFailed"; sessionId: string; error: string }
  | { kind: "taskCancelled"; sessionId: string }
  | { kind: "allowRuleAdded"; sessionId: string; agentId: string | null; pattern: string }
  | { kind: "userInputRequested"; sessionId: string; requestId: string; question: string; choices: string[]; allowFreeform: boolean };

export const EVENT_CHANNEL = "agent://event";
