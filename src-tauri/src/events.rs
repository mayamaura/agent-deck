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
        /// この実行の依頼文。継続依頼(resume)時に会話をターン単位で表示するために使う
        /// (フロントの tree.ts が taskStarted ごとにターンを区切る)。
        prompt: String,
    },
    AgentIntent {
        session_id: String,
        agent_id: Option<String>,
        text: String,
    },
    SubagentStarted {
        session_id: String,
        agent_id: String,
        // ツリー上の行の同一性を保つための相関 ID(docs/development.md ステップ5)。
        // subagent.started/completed/failed の payload に必ず含まれる。
        tool_call_id: String,
        display_name: String,
    },
    SubagentCompleted {
        session_id: String,
        agent_id: String,
        tool_call_id: String,
        duration_ms: u64,
        total_tokens: Option<u64>,
    },
    SubagentFailed {
        session_id: String,
        agent_id: String,
        tool_call_id: String,
        error: String,
    },
    ToolStarted {
        session_id: String,
        agent_id: Option<String>,
        tool_call_id: String,
        tool_name: String,
    },
    ToolCompleted {
        session_id: String,
        agent_id: Option<String>,
        tool_call_id: String,
        tool_name: String,
        success: bool,
    },
    PermissionRequested {
        session_id: String,
        request_id: String,
        // タグの "kind" と衝突するため permission_kind とする(write / shell など)
        permission_kind: String,
        detail: String,
        // 「常に許可」ボタンに使うパターン提案(copilot::suggest_allow_pattern)。
        // None ならフロントはボタンを出さない(write は無条件書き込み許可の骨抜きを防ぐため提案しない)。
        suggested_pattern: Option<String>,
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
    /// ユーザーが「常に許可」を選んだ際、agents.json へ永続化すべきパターンをフロントへ知らせる
    /// (docs/architecture.md §7.1 の拡張)。実際の永続化は main.rs の sink 側が担当する
    /// (SDK 型・設定ファイルへの書き込みを PermissionHandler に持ち込まないため)。
    /// agent_id はサブエージェント由来の場合に備えた情報用の Option(現状 SDK の権限要求には
    /// エージェント相関 ID が無いため常に None。どのエージェントの agents.json を書き換えるかは
    /// main.rs の spawn_task が既に知っている agent_id を使う)。
    AllowRuleAdded {
        session_id: String,
        agent_id: Option<String>,
        pattern: String,
    },
    /// エージェントが ask_user ツールで質問した(v1.0: 経路A、docs/sdk-notes.md「ユーザー入力」節)。
    /// request_id は SDK の UserInputHandler::handle が受け取らない(request_id 引数が無い。
    /// sdk-notes.md の想定と食い違う。copilot.rs UiUserInputHandler が独自採番する)。
    /// choices が空なら自由入力のみ(フロントは選択肢ボタンを出さない)。
    UserInputRequested {
        session_id: String,
        request_id: String,
        question: String,
        choices: Vec<String>,
        allow_freeform: bool,
    },
}
