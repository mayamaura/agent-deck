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
  subagents: { name: string; durationMs: number }[];
}

// src-tauri/src/events.rs AppEvent(kind でタグ付けされた単一チャネル "agent://event")
export type AppEvent =
  | { kind: "taskStarted"; sessionId: string; agentId: string; startedAt: string }
  | { kind: "agentIntent"; sessionId: string; agentId: string | null; text: string }
  | { kind: "subagentStarted"; sessionId: string; agentId: string; toolCallId: string; displayName: string }
  | { kind: "subagentCompleted"; sessionId: string; agentId: string; toolCallId: string; durationMs: number; totalTokens: number | null }
  | { kind: "subagentFailed"; sessionId: string; agentId: string; toolCallId: string; error: string }
  | { kind: "toolStarted"; sessionId: string; agentId: string | null; toolCallId: string; toolName: string }
  | { kind: "toolCompleted"; sessionId: string; agentId: string | null; toolCallId: string; toolName: string; success: boolean }
  | { kind: "permissionRequested"; sessionId: string; requestId: string; permissionKind: string; detail: string }
  | { kind: "usageUpdated"; sessionId: string; currentTokens: number; tokenLimit: number | null }
  | { kind: "taskCompleted"; sessionId: string; summary: string; outputFiles: string[] }
  | { kind: "taskFailed"; sessionId: string; error: string }
  | { kind: "taskCancelled"; sessionId: string };

export const EVENT_CHANNEL = "agent://event";
