// Rust 側の型のミラー。変更時は src-tauri/src/ の対応する型と同期すること。

// src-tauri/src/agents.rs AgentSummary
export interface AgentSummary {
  id: string;
  name: string;
  description: string;
  sourcePath: string;
  scope: "personal";
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
  | { kind: "subagentStarted"; sessionId: string; agentId: string; displayName: string }
  | { kind: "subagentCompleted"; sessionId: string; agentId: string; durationMs: number; totalTokens: number | null }
  | { kind: "subagentFailed"; sessionId: string; agentId: string; error: string }
  | { kind: "toolStarted"; sessionId: string; agentId: string | null; toolName: string }
  | { kind: "toolCompleted"; sessionId: string; agentId: string | null; toolName: string; success: boolean }
  | { kind: "permissionRequested"; sessionId: string; requestId: string; permissionKind: string; detail: string }
  | { kind: "usageUpdated"; sessionId: string; currentTokens: number; tokenLimit: number | null }
  | { kind: "taskCompleted"; sessionId: string; summary: string; outputFiles: string[] }
  | { kind: "taskFailed"; sessionId: string; error: string };

export const EVENT_CHANNEL = "agent://event";
