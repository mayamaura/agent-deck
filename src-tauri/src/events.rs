use serde::Serialize;

/// フロントへ emit する単一チャネル名(docs/architecture.md §4)。main.rs から使用。
/// bin/step2_check.rs はこのファイルを #[path] で共有するが Tauri emit を経由しないため
/// 参照しない(そちらのクレートルート単体では dead_code になるので allow)。
#[allow(dead_code)]
pub const EVENT_CHANNEL: &str = "agent://event";

/// フロントへ流すアプリ独自イベント。
/// SDK のイベント型をそのまま流さず、必ずこの型に変換する(docs/architecture.md §4)。
/// `agent_id: Option<String>` は None ならメインエージェント由来(docs/architecture.md §5)。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AppEvent {
    TaskStarted {
        session_id: String,
        agent_id: String,
        started_at: String,
    },
    AgentIntent {
        session_id: String,
        agent_id: Option<String>,
        text: String,
    },
    SubagentStarted {
        session_id: String,
        agent_id: String,
        display_name: String,
    },
    SubagentCompleted {
        session_id: String,
        agent_id: String,
        duration_ms: u64,
        total_tokens: Option<u64>,
    },
    SubagentFailed {
        session_id: String,
        agent_id: String,
        error: String,
    },
    ToolStarted {
        session_id: String,
        agent_id: Option<String>,
        tool_name: String,
    },
    ToolCompleted {
        session_id: String,
        agent_id: Option<String>,
        tool_name: String,
        success: bool,
    },
    PermissionRequested {
        session_id: String,
        request_id: String,
        // タグの "kind" と衝突するため permission_kind とする(write / shell など)
        permission_kind: String,
        detail: String,
    },
    UsageUpdated {
        session_id: String,
        current_tokens: u64,
        token_limit: Option<u64>,
    },
    TaskCompleted {
        session_id: String,
        summary: String,
        output_files: Vec<String>,
    },
    TaskFailed {
        session_id: String,
        error: String,
    },
    /// ユーザーによる中断(TaskFailed とは区別する)。
    TaskCancelled {
        session_id: String,
    },
}
