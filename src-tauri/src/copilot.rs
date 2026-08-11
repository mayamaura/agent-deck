// SDK セッションの実行と、SessionEvent → AppEvent 変換(docs/development.md §4 ステップ2)。
// SDK の型はこのモジュールの外に出さない(docs/architecture.md §4)。
//
// payload のフィールド名は、ローカルの cargo レジストリに展開された SDK ソースを直接読んで
// 確認したもの(github-copilot-sdk-1.0.9/src/generated/session_events.rs)。sdk-notes.md に
// 「未観測」とあった tool.* / subagent.* もこのソース読みで確認済み。

use crate::audit;
use crate::events::AppEvent;
use crate::permissions;
use github_copilot_sdk::handler::{PermissionHandler, PermissionResult};
use github_copilot_sdk::session_events::{
    AssistantIntentData, AssistantMessageData, AssistantUsageData, SessionErrorData,
    SessionIdleData, SessionUsageInfoData, SubagentCompletedData, SubagentFailedData,
    SubagentStartedData, ToolExecutionCompleteData, ToolExecutionStartData,
};
use github_copilot_sdk::types::{
    CustomAgentConfig, MessageOptions, PermissionRequestData, PermissionRequestKind, RequestId,
    SessionConfig, SessionEvent, SessionId,
};
use github_copilot_sdk::{CliProgram, Client, ClientOptions};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// エージェント 1 件分の SDK 用スペック(docs/sdk-notes.md「カスタムエージェント」節)。
/// `.agent.md` のパース結果(agents::AgentDefinition)から main.rs が組み立てる。
/// SDK の `CustomAgentConfig` へは `to_custom_agent_config` でこのモジュール内でのみ変換する。
#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    pub tools: Option<Vec<String>>,
    pub model: Option<String>,
    pub prompt: String,
}

/// AgentSpec → CustomAgentConfig。SDK 型はこの関数の戻り値としてのみこのモジュール内に留まる。
fn to_custom_agent_config(spec: &AgentSpec) -> CustomAgentConfig {
    let mut config =
        CustomAgentConfig::new(spec.name.clone(), spec.prompt.clone()).with_description(spec.description.clone());
    if let Some(display_name) = &spec.display_name {
        config = config.with_display_name(display_name.clone());
    }
    if let Some(tools) = &spec.tools {
        config = config.with_tools(tools.clone());
    }
    if let Some(model) = &spec.model {
        config = config.with_model(model.clone());
    }
    config
}

/// run_task 1 回分の入力(docs/development.md ステップ3)。
pub struct TaskSpec {
    pub prompt: String,
    /// TaskStarted.agent_id に使う、選択されたエージェントの id。
    pub agent_id: String,
    /// セッションに渡す全定義(選択外も委任候補として渡す。SDK 側が自動委任を判断する)。
    pub agents: Vec<AgentSpec>,
    /// with_agent に渡す name(agents 内のいずれかの name と一致すること)。
    pub selected_agent_name: String,
    pub working_directory: PathBuf,
    /// config.defaultModel。None なら SDK 既定に委ねる。
    pub session_model: Option<String>,
    /// このエージェントの入出力設定・許可/拒否ツール(docs/architecture.md §7.1)。
    pub rules: permissions::PermissionRules,
    /// respond_permission コマンドと run_task の PermissionHandler を橋渡しする。
    pub bridge: Arc<PermissionBridge>,
    /// 無人実行(スケジュール実行)か(docs/roadmap.md v0.4)。true のとき、Ask になった
    /// 権限要求は PermissionRequested を emit せず即座に拒否して実行全体を中断する
    /// (無人実行と承認ダイアログは両立しないため)。
    pub unattended: bool,
    /// 監査ログ(data/logs/)の置き場(docs/roadmap.md v0.6)。AuditWriter の生成に使う。
    /// ディレクトリの存在確認・作成は呼び出し側(main.rs)の責務。
    pub logs_dir: PathBuf,
}

/// UI からの承認応答を届けるブリッジ。main.rs の respond_permission コマンドと共有する
/// (AppState が保持し、start_task で TaskSpec に渡す)。
pub struct PermissionBridge {
    pending: std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
}

impl PermissionBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { pending: std::sync::Mutex::new(HashMap::new()) })
    }

    /// UI から届いた承認/拒否を該当タスクへ届ける。存在しない request_id はエラーにする
    /// (docs/development.md §3: エラーを握りつぶさない)。
    pub fn respond(&self, request_id: &str, approve: bool) -> Result<(), String> {
        let tx = self
            .pending
            .lock()
            .unwrap()
            .remove(request_id)
            .ok_or_else(|| format!("不明な承認要求です(既に応答済みか、対象タスクが終了しています): {request_id}"))?;
        // 受信側(run_task の PermissionHandler)がタスク終了で既に消えていても、
        // 送信失敗は握りつぶしてよい(完了直後の応答はタイミング差であり利用者側のエラーではない)。
        let _ = tx.send(approve);
        Ok(())
    }

    /// Ask になった要求ごとに応答待ちの受信側を登録する。
    fn register(&self, request_id: String) -> tokio::sync::oneshot::Receiver<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().unwrap().insert(request_id, tx);
        rx
    }

    /// タスク終了時の後始末(run_task の最後で呼ぶ)。未応答分の oneshot は
    /// Drop で自然にキャンセルされる(受信側の await は Err で解決する)。
    fn clear(&self) {
        self.pending.lock().unwrap().clear();
    }
}

/// PermissionRequestData → PermissionInput への防御的変換。
///
/// SDK 実ソース(github-copilot-sdk-1.0.9/src/generated/session_events.rs)で確認した実際の
/// ワイヤー形状: `extra["permissionRequest"]` 配下に kind ごとの詳細が入る。
/// - write: `fileName`(パス。`path` ではない)
/// - read: `path`
/// - shell: `fullCommandText`(コマンド全文)、`possiblePaths`
/// - mcp / custom-tool: `toolName`
/// CLI バージョン差に備え、他の候補キーも保険として試す。
/// 取れない情報があっても自動承認側には倒さない(write_path が None のままなら
/// decide() の出力フォルダ自動承認は成立せず Ask に落ちる)。
fn build_permission_input(data: &PermissionRequestData) -> permissions::PermissionInput {
    let permission_request = data.extra.get("permissionRequest");
    let kind_str = permission_kind_str(data.kind);

    // ツール種別名: kind を基本にしつつ、mcp/custom-tool など kind だけでは区別できない
    // ツールのパターン照合のために、より具体的な tool/toolName があれば優先する。
    let tool_name = permission_request
        .and_then(|pr| pr.get("toolName").or_else(|| pr.get("tool")))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or(kind_str);

    // write_path は「書き込み先」の意味なので write kind に限定する。read kind にも
    // 同名の "path" フィールド(読み込み対象)があり、区別せず拾うと出力フォルダの閲覧
    // (read)が書き込みの自動承認(decide() の outputDir 判定)に誤って乗ってしまう
    // (実機検証で確認: outputDir を view した read 要求が誤 Approve された)。
    let write_path = if matches!(data.kind, Some(PermissionRequestKind::Write)) {
        permission_request
            .and_then(|pr| pr.get("fileName").or_else(|| pr.get("path")))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
    } else {
        None
    };

    // read_path も write_path と同じ理由で kind を read に限定する(docs/roadmap.md v0.6:
    // 来歴 provenance.inputFiles 用。write との混同防止は write_path 側のコメント参照)。
    let read_path = if matches!(data.kind, Some(PermissionRequestKind::Read)) {
        permission_request
            .and_then(|pr| pr.get("path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
    } else {
        None
    };

    let detail = permission_request
        .and_then(|pr| {
            pr.get("fullCommandText")
                .or_else(|| pr.get("path"))
                .or_else(|| pr.get("fileName"))
                .or_else(|| pr.get("url"))
                .or_else(|| pr.get("command"))
                .or_else(|| pr.get("commandLine"))
                .or_else(|| pr.get("arguments"))
                .or_else(|| pr.get("intention"))
        })
        .and_then(|v| v.as_str())
        .map(str::to_string);

    permissions::PermissionInput { tool_name, detail, write_path, read_path }
}

fn permission_kind_str(kind: Option<PermissionRequestKind>) -> String {
    match kind {
        Some(PermissionRequestKind::Shell) => "shell",
        Some(PermissionRequestKind::Write) => "write",
        Some(PermissionRequestKind::Read) => "read",
        Some(PermissionRequestKind::Url) => "url",
        Some(PermissionRequestKind::Mcp) => "mcp",
        Some(PermissionRequestKind::CustomTool) => "custom-tool",
        Some(PermissionRequestKind::Memory) => "memory",
        Some(PermissionRequestKind::Hook) => "hook",
        _ => "unknown",
    }
    .to_string()
}

/// UI 確認をはさむ自前の PermissionHandler(docs/architecture.md §7.1)。
/// 判定は permissions::decide に委譲する。Ask になったものだけ PermissionRequested を
/// emit して bridge 経由の応答を待つ。ユーザーが拒否したら abort_tx で run_task の
/// select ループへ通知し、セッション全体を中断する(受け入れ条件7)。
///
/// PermissionHandler は #[async_trait] で定義されているが、その属性マクロを使うには
/// async-trait クレートを新規に直接依存として追加する必要がある(SDK は再エクスポートして
/// いない)。新規クレート追加は tauri-plugin-dialog 以外禁止のため、マクロが生成するのと
/// 同じ `Pin<Box<dyn Future>>` を返す形を手で書く。
struct UiPermissionHandler {
    rules: permissions::PermissionRules,
    bridge: Arc<PermissionBridge>,
    sink: Arc<dyn Fn(AppEvent) + Send + Sync>,
    /// ユーザーが拒否した(または無人実行で自動拒否した)際に run_task の select ループへ
    /// 中断を伝える。送る値は TaskFailed.error にそのまま使う理由文言。
    abort_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// 承認された(自動承認・ユーザー承認とも)書き込み先の絶対パスを集める共有コレクタ。
    /// run_task が作成し、EventContext とも同じ Arc を共有する(RunOutcome.output_files /
    /// TaskCompleted.output_files に同じ一覧を使うため。docs/development.md ステップ6)。
    output_files: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    /// 承認された読み込み対象の絶対パスを集める共有コレクタ(docs/roadmap.md v0.6:
    /// 来歴 provenance.inputFiles 用。output_files と同じ仕組み)。
    input_files: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    /// 無人実行フラグ(docs/roadmap.md v0.4)。TaskSpec.unattended をそのまま持つ。
    unattended: bool,
    /// 監査ログ(docs/roadmap.md v0.6)。全ての権限判定経路(Deny/Approve/Ask のいずれも)を
    /// record_permission で記録する(自動承認・自動拒否は AppEvent に出ないため、ここが唯一の記録経路)。
    audit: Arc<audit::AuditWriter>,
}

/// PermissionInput から PermissionAudit を組み立てる(docs/roadmap.md v0.6)。
/// UiPermissionHandler::handle の 5 判定経路(autoDenied/autoApproved/unattendedDenied/
/// userApproved/userDenied)で共通して使う。
fn permission_audit(input: &permissions::PermissionInput, decision: &str) -> audit::PermissionAudit {
    audit::PermissionAudit {
        timestamp: format_rfc3339_now(),
        decision: decision.to_string(),
        tool_name: input.tool_name.clone(),
        detail: input.detail.clone(),
        write_path: input.write_path.as_ref().map(|p| p.display().to_string()),
    }
}

impl PermissionHandler for UiPermissionHandler {
    fn handle<'life0, 'async_trait>(
        &'life0 self,
        session_id: SessionId,
        request_id: RequestId,
        data: PermissionRequestData,
    ) -> Pin<Box<dyn Future<Output = PermissionResult> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let session_id_str = session_id.to_string();
            // 観測用: extra の実形を記録する(docs/sdk-notes.md「形は CLI バージョン依存」)。
            eprintln!(
                "[permission] session={session_id_str} request={request_id} extra={}",
                serde_json::to_string(&data.extra).unwrap_or_else(|_| "<invalid json>".to_string())
            );

            let input = build_permission_input(&data);

            match permissions::decide(&self.rules, &input) {
                permissions::Decision::Deny => {
                    self.audit.record_permission(&session_id_str, &permission_audit(&input, "autoDenied"));
                    PermissionResult::reject("agents.json の deniedTools により拒否".to_string())
                }
                permissions::Decision::Approve => {
                    if let Some(path) = &input.write_path {
                        self.output_files.lock().unwrap().push(path.clone());
                    }
                    if let Some(path) = &input.read_path {
                        self.input_files.lock().unwrap().push(path.clone());
                    }
                    self.audit.record_permission(&session_id_str, &permission_audit(&input, "autoApproved"));
                    PermissionResult::approve_once()
                }
                permissions::Decision::Ask => {
                    let detail = match &input.detail {
                        Some(d) if !d.is_empty() => format!("{}: {d}", input.tool_name),
                        _ => input.tool_name.clone(),
                    };
                    if self.unattended {
                        // 無人実行と承認ダイアログは両立しない(docs/roadmap.md v0.4)。
                        // PermissionRequested は emit せず即座に拒否し、実行全体を中断する。
                        let reason = format!("無人実行のため、事前承認されていない操作を拒否しました: {detail}");
                        self.audit.record_permission(&session_id_str, &permission_audit(&input, "unattendedDenied"));
                        let _ = self.abort_tx.send(reason.clone());
                        return PermissionResult::reject(reason);
                    }
                    let request_id_str = request_id.to_string();
                    let permission_event = AppEvent::PermissionRequested {
                        session_id: session_id_str.clone(),
                        request_id: request_id_str.clone(),
                        permission_kind: input.tool_name.clone(),
                        detail,
                    };
                    // 監査ログは「全 AppEvent」が方針(docs/roadmap.md v0.6)。この後の
                    // record_permission(userApproved/userDenied)は最終判定のみを記す別記録。
                    self.audit.record_event(&session_id_str, &permission_event);
                    (self.sink)(permission_event);
                    let rx = self.bridge.register(request_id_str);
                    match rx.await {
                        Ok(true) => {
                            if let Some(path) = &input.write_path {
                                self.output_files.lock().unwrap().push(path.clone());
                            }
                            if let Some(path) = &input.read_path {
                                self.input_files.lock().unwrap().push(path.clone());
                            }
                            self.audit.record_permission(&session_id_str, &permission_audit(&input, "userApproved"));
                            PermissionResult::approve_once()
                        }
                        Ok(false) => {
                            // タスク全体を止める(拒否は個々のツール呼び出しだけでなく実行全体の中断)。
                            self.audit.record_permission(&session_id_str, &permission_audit(&input, "userDenied"));
                            let _ = self.abort_tx.send("権限が拒否されたため実行を中断しました".to_string());
                            PermissionResult::reject("ユーザーが拒否しました".to_string())
                        }
                        Err(_) => {
                            // 応答が届く前に送信側が消えた(タスク終了等)。安全側に倒して拒否する。
                            self.audit.record_permission(&session_id_str, &permission_audit(&input, "userDenied"));
                            PermissionResult::reject("応答を受信できなかったため拒否しました".to_string())
                        }
                    }
                }
            }
        })
    }
}

/// セッション中断の理由。session.idle(aborted=true) 到達時に TaskCancelled / TaskFailed の
/// どちらを emit するか決めるために EventContext が保持する(docs/sdk-notes.md「中断」節、
/// 受け入れ条件7: 権限拒否による中断は失敗として扱う)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbortReason {
    UserCancel,
    PermissionDenied,
}

/// CLI パスの解決(docs/architecture.md §1.2 の順): 設定値 → 環境変数 COPILOT_CLI_PATH → PATH 探索。
pub fn resolve_cli_path(configured: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = configured {
        return if p.is_file() {
            Ok(p.to_path_buf())
        } else {
            Err(format!("設定された copilotCliPath が見つかりません: {}", p.display()))
        };
    }
    if let Ok(env_value) = std::env::var("COPILOT_CLI_PATH") {
        let p = PathBuf::from(&env_value);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "環境変数 COPILOT_CLI_PATH が指すファイルが見つかりません: {env_value}"
            ))
        };
    }
    find_on_path().ok_or_else(|| {
        "Copilot CLI が見つかりません。設定の copilotCliPath か環境変数 COPILOT_CLI_PATH で \
         copilot.exe のパスを指定してください"
            .to_string()
    })
}

/// `where copilot` 相当の PATH 探索(Windows 専用アプリのため `where` 固定)。
fn find_on_path() -> Option<PathBuf> {
    let output = std::process::Command::new("where").arg("copilot").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first_line = text.lines().next()?.trim();
    (!first_line.is_empty()).then(|| PathBuf::from(first_line))
}

/// SDK イベント → AppEvent 変換の内部状態(対応表は docs/architecture.md §4)。
/// SDK の型はこの構造体の外に出さない。
pub struct EventContext {
    /// SessionEvent 自体には session_id フィールドが無いため、run_task が
    /// セッション作成直後に設定する。
    session_id: String,
    /// メインエージェント(envelope の agent_id が None)の直近の assistant.message。
    /// session.idle 時の TaskCompleted.summary に使う。サブエージェントの発言では上書きしない。
    last_main_message: String,
    /// tool_call_id → 開始時刻。subagent.completed/failed の payload に duration_ms が
    /// 無い場合のフォールバック計算に使う。
    subagent_started_at: HashMap<String, Instant>,
    /// tool_call_id → tool_name。tool.execution_complete の payload には tool_name が
    /// 無いため、tool.execution_start で覚えておいて補う。
    tool_names: HashMap<String, String>,
    /// run_task が abort() を呼んだ理由。session.idle(aborted=true) 到達時に
    /// TaskCancelled / TaskFailed のどちらを emit するか決めるために使う。
    abort_reason: Option<AbortReason>,
    /// abort_reason が PermissionDenied のときに TaskFailed.error として使う文言
    /// (ユーザー拒否/無人実行拒否で文言を変えるため。docs/roadmap.md v0.4)。
    abort_message: Option<String>,
    /// assistant.usage の input_tokens+output_tokens の総和(取れた分)。RunOutcome.total_tokens に使う。
    total_tokens: Option<u64>,
    /// subagent.completed/failed で確定した (name, duration_ms) の列。RunOutcome.subagents に使う。
    subagent_outcomes: Vec<SubagentOutcome>,
    /// 承認された書き込み先の共有コレクタ。run_task が UiPermissionHandler と同じ Arc を渡す
    /// (TaskCompleted.output_files に使う)。
    output_files: Arc<std::sync::Mutex<Vec<PathBuf>>>,
}

impl Default for EventContext {
    fn default() -> Self {
        Self::new()
    }
}

impl EventContext {
    pub fn new() -> Self {
        Self {
            session_id: String::new(),
            last_main_message: String::new(),
            subagent_started_at: HashMap::new(),
            tool_names: HashMap::new(),
            abort_reason: None,
            abort_message: None,
            total_tokens: None,
            subagent_outcomes: Vec::new(),
            output_files: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// run_task が session.abort() を呼ぶ直前に理由を記録する。同一モジュール内の
    /// run_task からのみ呼ぶため pub にしない(AbortReason 自体も非公開のため)。
    fn set_abort_reason(&mut self, reason: AbortReason) {
        self.abort_reason = Some(reason);
    }

    /// 権限拒否による中断を、TaskFailed.error に使う文言つきで記録する
    /// (ユーザー拒否/無人実行拒否で文言が異なる。docs/roadmap.md v0.4)。
    fn set_permission_denied(&mut self, message: String) {
        self.abort_reason = Some(AbortReason::PermissionDenied);
        self.abort_message = Some(message);
    }

    /// 1 SDK イベント → 0..n 個の AppEvent。対応表は docs/architecture.md §4。
    /// 注意: ストリーミング delta 系(assistant.message_delta 等)は使わない。
    /// 実行ごとに有無が揺れるため、確定本文の assistant.message のみを見る(sdk-notes.md)。
    pub fn convert(&mut self, ev: &SessionEvent) -> Vec<AppEvent> {
        match ev.event_type.as_str() {
            "assistant.intent" => self.on_assistant_intent(ev),
            "subagent.started" => self.on_subagent_started(ev),
            "subagent.completed" => self.on_subagent_completed(ev),
            "subagent.failed" => self.on_subagent_failed(ev),
            "tool.execution_start" => self.on_tool_started(ev),
            "tool.execution_complete" => self.on_tool_completed(ev),
            "assistant.usage" => self.on_assistant_usage(ev),
            "session.usage_info" => self.on_session_usage_info(ev),
            "assistant.message" => self.on_assistant_message(ev),
            "session.idle" => self.on_session_idle(ev),
            "session.error" => self.on_session_error(ev),
            _ => Vec::new(),
        }
    }

    fn on_assistant_intent(&self, ev: &SessionEvent) -> Vec<AppEvent> {
        let Some(data) = ev.typed_data::<AssistantIntentData>() else {
            return Vec::new();
        };
        vec![AppEvent::AgentIntent {
            session_id: self.session_id.clone(),
            agent_id: ev.agent_id.clone(),
            text: data.intent,
        }]
    }

    fn on_subagent_started(&mut self, ev: &SessionEvent) -> Vec<AppEvent> {
        let Some(data) = ev.typed_data::<SubagentStartedData>() else {
            return Vec::new();
        };
        self.subagent_started_at.insert(data.tool_call_id.clone(), Instant::now());
        // 観測用(docs/development.md ステップ5、bin/step5_check.rs の実機検証で参照)。
        eprintln!(
            "[subagent.started] envelope agent_id={:?} tool_call_id={} agent_name={}",
            ev.agent_id, data.tool_call_id, data.agent_name
        );
        // 相関方式(docs/development.md ステップ5。bin/step5_check.rs で実機確認済み、
        // CLI 1.0.79 / SDK 1.0.9、2026-08-12): subagent.started の時点で envelope の
        // agent_id は既に Some で付与されており、その値は「委任元の task ツール呼び出し
        // の tool_call_id」と一致していた(= data.tool_call_id と同値)。その後の
        // subagent.completed でも同じ agent_id が付与されることを確認済み
        // (CORRELATION ログ参照)。したがって envelope 値をそのままツリーの行キーに
        // 使うのが最も確実(on_tool_started/on_tool_completed は agent_id をそのまま
        // 透過するだけで正しく相関する。サブエージェント自身がツールを呼ぶケースは
        // 今回の観測プロンプトでは発生しなかったため個別確認はできていないが、
        // envelope agent_id の伝播は tool.execution_* も含め SDK 側の共通機構
        // なので同じ値になる見込み)。
        // 万一将来のバージョンで subagent.started 時点の agent_id が None になった
        // 場合の防御として、payload の agent_name を暫定キーにするフォールバックを
        // 残す(同名サブエージェントが同一実行内で複数回呼ばれると行が併合される
        // 制約が残るが、実機では一度も踏んでいない経路)。
        let agent_id = ev.agent_id.clone().unwrap_or_else(|| data.agent_name.clone());
        vec![AppEvent::SubagentStarted {
            session_id: self.session_id.clone(),
            agent_id,
            tool_call_id: data.tool_call_id,
            display_name: data.agent_display_name,
        }]
    }

    fn on_subagent_completed(&mut self, ev: &SessionEvent) -> Vec<AppEvent> {
        let Some(data) = ev.typed_data::<SubagentCompletedData>() else {
            return Vec::new();
        };
        let started_at = self.subagent_started_at.remove(&data.tool_call_id);
        let duration_ms = data
            .duration_ms
            .map(|v| v.max(0) as u64)
            .or_else(|| started_at.map(|s| s.elapsed().as_millis() as u64))
            .unwrap_or(0);
        // on_subagent_started と同じ式(同じ agent_name なら同じ行に確定する)。
        let agent_id = ev.agent_id.clone().unwrap_or_else(|| data.agent_name.clone());
        self.subagent_outcomes.push(SubagentOutcome { name: data.agent_name.clone(), duration_ms });
        vec![AppEvent::SubagentCompleted {
            session_id: self.session_id.clone(),
            agent_id,
            tool_call_id: data.tool_call_id,
            duration_ms,
            total_tokens: data.total_tokens.map(|v| v.max(0) as u64),
        }]
    }

    fn on_subagent_failed(&mut self, ev: &SessionEvent) -> Vec<AppEvent> {
        let Some(data) = ev.typed_data::<SubagentFailedData>() else {
            return Vec::new();
        };
        let started_at = self.subagent_started_at.remove(&data.tool_call_id);
        let duration_ms = data
            .duration_ms
            .map(|v| v.max(0) as u64)
            .or_else(|| started_at.map(|s| s.elapsed().as_millis() as u64))
            .unwrap_or(0);
        self.subagent_outcomes.push(SubagentOutcome { name: data.agent_name.clone(), duration_ms });
        let agent_id = ev.agent_id.clone().unwrap_or_else(|| data.agent_name.clone());
        vec![AppEvent::SubagentFailed {
            session_id: self.session_id.clone(),
            agent_id,
            tool_call_id: data.tool_call_id,
            error: data.error,
        }]
    }

    fn on_tool_started(&mut self, ev: &SessionEvent) -> Vec<AppEvent> {
        let Some(data) = ev.typed_data::<ToolExecutionStartData>() else {
            return Vec::new();
        };
        self.tool_names.insert(data.tool_call_id.clone(), data.tool_name.clone());
        vec![AppEvent::ToolStarted {
            session_id: self.session_id.clone(),
            agent_id: ev.agent_id.clone(),
            tool_call_id: data.tool_call_id,
            tool_name: data.tool_name,
        }]
    }

    fn on_tool_completed(&mut self, ev: &SessionEvent) -> Vec<AppEvent> {
        let Some(data) = ev.typed_data::<ToolExecutionCompleteData>() else {
            return Vec::new();
        };
        // tool.execution_complete の payload には tool_name が無い(tool_call_id のみ)ため、
        // tool.execution_start で覚えた名前を補う。
        let tool_name = self
            .tool_names
            .remove(&data.tool_call_id)
            .unwrap_or_else(|| data.tool_call_id.clone());
        vec![AppEvent::ToolCompleted {
            session_id: self.session_id.clone(),
            agent_id: ev.agent_id.clone(),
            tool_call_id: data.tool_call_id,
            tool_name,
            success: data.success,
        }]
    }

    fn on_assistant_usage(&mut self, ev: &SessionEvent) -> Vec<AppEvent> {
        let Some(data) = ev.typed_data::<AssistantUsageData>() else {
            return Vec::new();
        };
        // assistant.usage には current_tokens/token_limit が無いため、取れる input+output で代用。
        let current = (data.input_tokens.unwrap_or(0) + data.output_tokens.unwrap_or(0)).max(0) as u64;
        // RunOutcome.total_tokens は「呼び出しごとの input+output の総和」(docs/development.md
        // ステップ6)。session.usage_info の current_tokens(単発の現在値)とは別物。
        self.total_tokens = Some(self.total_tokens.unwrap_or(0) + current);
        vec![AppEvent::UsageUpdated {
            session_id: self.session_id.clone(),
            current_tokens: current,
            token_limit: None,
        }]
    }

    fn on_session_usage_info(&self, ev: &SessionEvent) -> Vec<AppEvent> {
        let Some(data) = ev.typed_data::<SessionUsageInfoData>() else {
            return Vec::new();
        };
        vec![AppEvent::UsageUpdated {
            session_id: self.session_id.clone(),
            current_tokens: data.current_tokens.max(0) as u64,
            token_limit: Some(data.token_limit.max(0) as u64),
        }]
    }

    fn on_assistant_message(&mut self, ev: &SessionEvent) -> Vec<AppEvent> {
        // メインエージェント分だけ保持する。サブエージェントの発言で summary が
        // 上書きされないようにするため(docs/architecture.md §5: agent_id は
        // サブエージェント由来イベントにのみ付与)。
        if ev.agent_id.is_none() {
            if let Some(data) = ev.typed_data::<AssistantMessageData>() {
                self.last_main_message = data.content;
            }
        }
        Vec::new()
    }

    /// 中断は `Session::abort()` の RPC 経由(disconnect ではない)。進行中の
    /// send_and_wait はいずれにせよ session.idle で解決されるため、ユーザー中断か
    /// 通常完了かは payload の aborted フラグで判別する(docs/sdk-notes.md「中断」節)。
    fn on_session_idle(&self, ev: &SessionEvent) -> Vec<AppEvent> {
        let aborted = ev.typed_data::<SessionIdleData>().and_then(|d| d.aborted).unwrap_or(false);
        if aborted {
            return match self.abort_reason {
                // 権限拒否による中断は「失敗」として扱う(受け入れ条件7)。
                Some(AbortReason::PermissionDenied) => vec![AppEvent::TaskFailed {
                    session_id: self.session_id.clone(),
                    error: self
                        .abort_message
                        .clone()
                        .unwrap_or_else(|| "権限が拒否されたため実行を中断しました".to_string()),
                }],
                _ => vec![AppEvent::TaskCancelled { session_id: self.session_id.clone() }],
            };
        }
        vec![AppEvent::TaskCompleted {
            session_id: self.session_id.clone(),
            summary: self.last_main_message.clone(),
            output_files: dedup_paths(&self.output_files),
        }]
    }

    fn on_session_error(&self, ev: &SessionEvent) -> Vec<AppEvent> {
        let error = ev
            .typed_data::<SessionErrorData>()
            .map(|d| format!("[{}] {}", d.error_type, d.message))
            .unwrap_or_else(|| "不明なエラーが発生しました".to_string());
        vec![AppEvent::TaskFailed {
            session_id: self.session_id.clone(),
            error,
        }]
    }
}

/// send_and_wait の待機上限。業務レポート生成は数十分かかることがあるため長めに取る。
const SEND_AND_WAIT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// send_and_wait 解決直後に届く session.idle/error のブロードキャストを拾うためのドレイン待ち。
/// SDK 内部では idle_waiter (send_and_wait の戻り値) を解決してから同じ関数内で購読者へ
/// broadcast するため、select! が send_and_wait 側を先に拾うと最後の 1 件を取りこぼし得る
/// (session.rs の handle_notification 実装を読んで確認)。取りこぼしても run_task 側の
/// フォールバックで TaskFailed 相当は必ず出す。
/// ponytail: 固定タイムアウトによる簡易ドレイン。取りこぼしが実運用で問題になれば、
/// send_and_wait を使わず購読ループ側だけで完了判定する設計に置き換える。
const DRAIN_TIMEOUT: Duration = Duration::from_millis(200);

enum Outcome {
    Completed,
    Failed(String),
}

/// 承認されたパス集合(書き込み先/読み込み対象とも)の重複除去済み絶対パス文字列一覧
/// (UiPermissionHandler / EventContext が共有する Arc<Mutex<Vec<PathBuf>>> から作る)。
fn dedup_paths(files: &std::sync::Mutex<Vec<PathBuf>>) -> Vec<String> {
    let mut list: Vec<String> = files.lock().unwrap().iter().map(|p| p.display().to_string()).collect();
    list.sort();
    list.dedup();
    list
}

/// run_task 1 回分の結果(docs/development.md ステップ6)。history::entry_from_outcome の入力になる。
/// タスク開始(TaskStarted emit)後に確定した終端状態のみを表す。開始前の失敗は
/// Err(String) のまま(履歴対象外。理由は run_task 側のコメント参照)。
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub session_id: String,
    pub status: TaskStatus,
    /// 完了時の最終メッセージ(失敗時はエラー文)。
    pub summary: String,
    /// 承認された書き込み先の絶対パス(重複除去済み)。
    pub output_files: Vec<String>,
    /// 承認された読み込み対象の絶対パス(重複除去済み。docs/roadmap.md v0.6: 来歴 provenance.inputFiles 用)。
    pub input_files: Vec<String>,
    /// assistant.usage の input_tokens+output_tokens の総和(取れた分)。
    pub total_tokens: Option<u64>,
    pub subagents: Vec<SubagentOutcome>,
    /// TaskStarted と同じ値。
    pub started_at: String,
    pub duration_ms: u64,
}

/// RunOutcome.status。SDK の型はここに持ち込まない(history.rs で "completed"/"failed"/"cancelled" に変換)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    /// history::entry_from_outcome と同じ文字列表現(docs/roadmap.md v0.6: provenance.status
    /// でも同じ表記を使うための共有ヘルパー。history.rs 側は変更不要のためそのまま残す)。
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubagentOutcome {
    pub name: String,
    pub duration_ms: u64,
}

/// TaskCompleted/TaskFailed/TaskCancelled のときだけ (status, summary) を返す。
/// run_task が select ループの2箇所(drain / 通常受信)で終端状態を拾うための小さな判別ヘルパー。
fn terminal_outcome_of(ev: &AppEvent) -> Option<(TaskStatus, String)> {
    match ev {
        AppEvent::TaskCompleted { summary, .. } => Some((TaskStatus::Completed, summary.clone())),
        AppEvent::TaskFailed { error, .. } => Some((TaskStatus::Failed, error.clone())),
        AppEvent::TaskCancelled { .. } => Some((TaskStatus::Cancelled, String::new())),
        _ => None,
    }
}

/// タスク 1 本の実行(Client 起動 → セッション作成 → 購読 → send_and_wait → 後始末)。
/// sink には変換済み AppEvent を渡すだけで、emit するのは呼び出し側の責務(main.rs)。
///
/// 戻り値は Result<RunOutcome, String> だが、Err になるのは TaskStarted を emit する前
/// (Client 起動・セッション作成)の失敗のみ。開始後に判明した失敗・中断は
/// Ok(RunOutcome { status: Failed/Cancelled, .. }) として返す(呼び出し側の main.rs が
/// Ok/Err を問わず同じ経路で履歴に書けるようにするため。開始前の失敗はセッションが
/// 存在せず履歴の主キーである session_id が無いため、そもそも履歴対象外)。
pub async fn run_task(
    cli_path: PathBuf,
    spec: TaskSpec,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
    sink: impl Fn(AppEvent) + Send + Sync + 'static,
) -> Result<RunOutcome, String> {
    let client = Client::start(ClientOptions::new().with_program(CliProgram::Path(cli_path)))
        .await
        .map_err(|e| format!("Copilot CLI を起動できません: {e}"))?;

    // sink を Arc<dyn Fn> にしておき、PermissionHandler(別スレッド/タスクで呼ばれる)にも
    // 同じ関数を共有させる。
    let sink: Arc<dyn Fn(AppEvent) + Send + Sync> = Arc::new(sink);
    let bridge = spec.bridge.clone();
    // ユーザーが権限要求を拒否した(または無人実行で自動拒否した)際に、PermissionHandler から
    // select ループへ中断を伝える経路。送る値はそのまま TaskFailed.error の文言に使う。
    let (deny_abort_tx, mut deny_abort_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    // 承認された書き込み先の共有コレクタ。UiPermissionHandler と EventContext の両方に
    // 同じ Arc を渡す(RunOutcome.output_files / TaskCompleted.output_files を一致させるため)。
    let output_files: Arc<std::sync::Mutex<Vec<PathBuf>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    // 承認された読み込み対象の共有コレクタ(docs/roadmap.md v0.6: 来歴 provenance.inputFiles 用)。
    let input_files: Arc<std::sync::Mutex<Vec<PathBuf>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    // 監査ログ(docs/roadmap.md v0.6)。ファイル自体は session_id 判明後の初回書き込みで
    // 遅延生成されるため、この時点(セッション作成前)で作ってよい。
    let audit = Arc::new(audit::AuditWriter::new(&spec.logs_dir));
    let permission_handler = Arc::new(UiPermissionHandler {
        rules: spec.rules.clone(),
        bridge: bridge.clone(),
        sink: Arc::clone(&sink),
        abort_tx: deny_abort_tx,
        output_files: Arc::clone(&output_files),
        input_files: Arc::clone(&input_files),
        unattended: spec.unattended,
        audit: Arc::clone(&audit),
    });

    // rules.denied_tools のうち括弧を含まない単純名(例 "write")は SessionConfig の
    // excluded_tools にも渡し、多重に防御する(docs/sdk-notes.md「ツール制限」節: excluded_tools
    // は単純名のみ扱えるため、shell(rm) のようなパターン付きは PermissionHandler 側だけに委ねる)。
    let simple_denied: Vec<String> =
        spec.rules.denied_tools.iter().filter(|t| !t.contains('(')).cloned().collect();

    let custom_agents: Vec<CustomAgentConfig> = spec.agents.iter().map(to_custom_agent_config).collect();
    let mut config = SessionConfig::default()
        .with_permission_handler(permission_handler)
        .with_custom_agents(custom_agents)
        .with_agent(spec.selected_agent_name.clone())
        .with_working_directory(spec.working_directory.clone());
    if !simple_denied.is_empty() {
        config = config.with_excluded_tools(simple_denied);
    }
    if let Some(model) = &spec.session_model {
        config = config.with_model(model.clone());
    }

    let session = match client.create_session(config).await {
        Ok(s) => s,
        Err(e) => {
            if let Err(stop_err) = client.stop().await {
                eprintln!("Client の停止に失敗しました: {stop_err}");
            }
            bridge.clear();
            return Err(format!("セッションを作成できません: {e}"));
        }
    };

    let session_id = session.id().to_string();
    let started_at = format_rfc3339_now();
    // RunOutcome.duration_ms は「開始時刻からの所要」(docs/development.md ステップ6)。
    let task_start = Instant::now();
    let task_started_event =
        AppEvent::TaskStarted { session_id: session_id.clone(), agent_id: spec.agent_id, started_at: started_at.clone() };
    audit.record_event(&session_id, &task_started_event);
    sink(task_started_event);

    let mut ctx = EventContext::new();
    ctx.session_id = session_id.clone();
    ctx.output_files = Arc::clone(&output_files);
    let mut events = session.subscribe();
    // session.error 由来の TaskFailed、または aborted な session.idle 由来の TaskCancelled が
    // convert() 側で既に送られたかどうか。送られていれば run_task 側の失敗フォールバックで重複させない。
    let mut terminal_sent = false;
    // convert() が終端 AppEvent を出した際の (status, summary)。RunOutcome の元になる。
    let mut terminal_status: Option<TaskStatus> = None;
    let mut terminal_summary = String::new();
    // cancel は一度シグナルを受けたら abort() を呼ぶだけで、以降は select! の対象から外す
    // (oneshot::Receiver は完了後の再 poll を想定していないため)。
    let mut cancel_requested = false;
    // 権限拒否による中断も同様に一度だけ(複数のツール呼び出しがそれぞれ拒否されても二重に
    // abort() を呼ばない)。
    let mut permission_abort_requested = false;

    let send_fut = session.send_and_wait(MessageOptions::new(spec.prompt).with_wait_timeout(SEND_AND_WAIT_TIMEOUT));
    tokio::pin!(send_fut);

    let outcome = loop {
        tokio::select! {
            _ = &mut cancel, if !cancel_requested => {
                cancel_requested = true;
                // 中断の正道は abort()(disconnect ではない)。send_and_wait は
                // 引き続き session.idle(aborted=true)で解決されるのを待つ
                // (docs/sdk-notes.md「中断」節)。
                ctx.set_abort_reason(AbortReason::UserCancel);
                if let Err(e) = session.abort().await {
                    eprintln!("セッションの中断に失敗しました: {e}");
                }
            }
            Some(reason) = deny_abort_rx.recv(), if !permission_abort_requested => {
                permission_abort_requested = true;
                // 受け入れ条件7: 権限拒否は実行全体の中断につながる。
                ctx.set_permission_denied(reason);
                if let Err(e) = session.abort().await {
                    eprintln!("セッションの中断に失敗しました: {e}");
                }
            }
            send_result = &mut send_fut => {
                loop {
                    match tokio::time::timeout(DRAIN_TIMEOUT, events.recv()).await {
                        Ok(Ok(event)) => {
                            for app_event in ctx.convert(&event) {
                                if let Some((status, summary)) = terminal_outcome_of(&app_event) {
                                    terminal_sent = true;
                                    terminal_status = Some(status);
                                    terminal_summary = summary;
                                }
                                audit.record_event(&session_id, &app_event);
                                sink(app_event);
                            }
                        }
                        _ => break,
                    }
                }
                break match send_result {
                    Ok(_) => Outcome::Completed,
                    Err(e) => Outcome::Failed(format!("タスクの実行に失敗しました: {e}")),
                };
            }
            event = events.recv() => {
                if let Ok(event) = event {
                    for app_event in ctx.convert(&event) {
                        if let Some((status, summary)) = terminal_outcome_of(&app_event) {
                            terminal_sent = true;
                            terminal_status = Some(status);
                            terminal_summary = summary;
                        }
                        audit.record_event(&session_id, &app_event);
                        sink(app_event);
                    }
                }
                // Lagged/Closed はここでは無視して継続する(次のイテレーションで再試行)。
            }
        }
    };

    if let Err(e) = session.disconnect().await {
        eprintln!("セッション切断に失敗しました: {e}");
    }
    if let Err(e) = client.stop().await {
        eprintln!("Client の停止に失敗しました: {e}");
    }
    // 未応答の承認要求が残っていれば破棄する(oneshot の Drop で受信側は自然にキャンセルされる)。
    bridge.clear();

    // terminal_status が既にあれば convert() が emit した終端 AppEvent(TaskCompleted/Failed/
    // Cancelled)をそのまま採用する。無ければ(DRAIN_TIMEOUT 内に session.idle 等の
    // ブロードキャストを拾えなかった稀なケース)send_and_wait 自体の結果で代用する。
    // 後者の Failed は従来どおりここで TaskFailed を補って emit する(エラーを握りつぶさない)。
    let (status, summary) = match terminal_status {
        Some(status) => (status, terminal_summary),
        None => match &outcome {
            Outcome::Completed => (TaskStatus::Completed, ctx.last_main_message.clone()),
            Outcome::Failed(e) => {
                if !terminal_sent {
                    let ev = AppEvent::TaskFailed { session_id: session_id.clone(), error: e.clone() };
                    audit.record_event(&session_id, &ev);
                    sink(ev);
                }
                (TaskStatus::Failed, e.clone())
            }
        },
    };

    Ok(RunOutcome {
        session_id,
        status,
        summary,
        output_files: dedup_paths(&output_files),
        input_files: dedup_paths(&input_files),
        total_tokens: ctx.total_tokens,
        subagents: ctx.subagent_outcomes,
        started_at,
        duration_ms: task_start.elapsed().as_millis() as u64,
    })
}

/// std に無い RFC3339 風フォーマット(秒精度)。新規クレート追加禁止(chrono 等不可)のため自前実装。
/// Howard Hinnant の civil_from_days アルゴリズム(グレゴリオ暦、UTC 前提)。
/// pub(crate): sync.rs のマニフェスト syncedAt にも再利用する(同じ日時算出ロジックを二重実装しない)。
pub(crate) fn format_rfc3339_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let days = secs.div_euclid(86400);
    let time_of_day = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// エポックからの日数 → (年, 月, 日)。 https://howardhinnant.github.io/date_algorithms.html
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // SessionEvent は struct リテラルで組み立てず、実際の受信経路と同じ JSON デシリアライズ
    // 経由で作る(将来 SDK 側にフィールドが増えてもテストが壊れにくい)。
    fn mk_event(event_type: &str, agent_id: Option<&str>, data: serde_json::Value) -> SessionEvent {
        let mut wire = json!({
            "id": "evt",
            "timestamp": "2026-08-12T00:00:00Z",
            "type": event_type,
            "data": data,
        });
        if let Some(id) = agent_id {
            wire["agentId"] = json!(id);
        }
        serde_json::from_value(wire).expect("valid SessionEvent fixture")
    }

    #[test]
    fn civil_from_days_epoch_is_1970_01_01() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn unknown_event_is_ignored() {
        let mut ctx = EventContext::new();
        let ev = mk_event("session.some_future_event", None, json!({}));
        assert!(ctx.convert(&ev).is_empty());
    }

    #[test]
    fn subagent_started_then_completed_uses_payload_duration_when_present() {
        let mut ctx = EventContext::new();
        ctx.session_id = "sess-1".into();
        let started = mk_event(
            "subagent.started",
            Some("sub-1"),
            json!({
                "agentDescription": "調査を行う",
                "agentDisplayName": "Explorer",
                "agentName": "explorer",
                "toolCallId": "call-1"
            }),
        );
        let out = ctx.convert(&started);
        assert_eq!(out.len(), 1);
        match &out[0] {
            AppEvent::SubagentStarted { agent_id, display_name, .. } => {
                assert_eq!(agent_id, "sub-1");
                assert_eq!(display_name, "Explorer");
            }
            other => panic!("expected SubagentStarted, got {other:?}"),
        }

        let completed = mk_event(
            "subagent.completed",
            Some("sub-1"),
            json!({
                "agentDisplayName": "Explorer",
                "agentName": "explorer",
                "toolCallId": "call-1",
                "durationMs": 1234,
                "totalTokens": 42
            }),
        );
        let out = ctx.convert(&completed);
        assert_eq!(out.len(), 1);
        match &out[0] {
            AppEvent::SubagentCompleted { duration_ms, total_tokens, .. } => {
                assert_eq!(*duration_ms, 1234);
                assert_eq!(*total_tokens, Some(42));
            }
            other => panic!("expected SubagentCompleted, got {other:?}"),
        }
    }

    #[test]
    fn subagent_completed_falls_back_to_measured_duration_when_payload_omits_it() {
        let mut ctx = EventContext::new();
        let started = mk_event(
            "subagent.started",
            None,
            json!({
                "agentDescription": "d",
                "agentDisplayName": "Explorer",
                "agentName": "explorer",
                "toolCallId": "call-2"
            }),
        );
        ctx.convert(&started);
        std::thread::sleep(Duration::from_millis(10));

        let completed = mk_event(
            "subagent.completed",
            None,
            json!({
                "agentDisplayName": "Explorer",
                "agentName": "explorer",
                "toolCallId": "call-2"
            }),
        );
        let out = ctx.convert(&completed);
        match &out[0] {
            AppEvent::SubagentCompleted { duration_ms, .. } => {
                assert!(*duration_ms >= 10, "duration_ms={duration_ms} should be >= 10");
            }
            other => panic!("expected SubagentCompleted, got {other:?}"),
        }
    }

    #[test]
    fn tool_completed_reuses_tool_name_from_start_event() {
        let mut ctx = EventContext::new();
        let start = mk_event("tool.execution_start", None, json!({"toolCallId": "t1", "toolName": "write"}));
        assert!(ctx.convert(&start).len() == 1);
        let complete = mk_event("tool.execution_complete", None, json!({"toolCallId": "t1", "success": true}));
        let out = ctx.convert(&complete);
        match &out[0] {
            AppEvent::ToolCompleted { tool_name, success, .. } => {
                assert_eq!(tool_name, "write");
                assert!(*success);
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
    }

    #[test]
    fn session_idle_uses_last_main_assistant_message_as_summary() {
        let mut ctx = EventContext::new();
        ctx.session_id = "sess-1".into();

        // サブエージェントの発言は summary に反映されない。
        let sub_msg = mk_event(
            "assistant.message",
            Some("sub-1"),
            json!({"content": "サブエージェントの発言", "messageId": "m0"}),
        );
        assert!(ctx.convert(&sub_msg).is_empty());

        let main_msg = mk_event("assistant.message", None, json!({"content": "完了しました", "messageId": "m1"}));
        assert!(ctx.convert(&main_msg).is_empty());

        let idle = mk_event("session.idle", None, json!({}));
        let out = ctx.convert(&idle);
        assert_eq!(out.len(), 1);
        match &out[0] {
            AppEvent::TaskCompleted { summary, output_files, .. } => {
                assert_eq!(summary, "完了しました");
                assert!(output_files.is_empty());
            }
            other => panic!("expected TaskCompleted, got {other:?}"),
        }
    }

    #[test]
    fn session_idle_with_aborted_flag_becomes_task_cancelled() {
        let mut ctx = EventContext::new();
        ctx.session_id = "sess-1".into();

        let idle = mk_event("session.idle", None, json!({"aborted": true}));
        let out = ctx.convert(&idle);
        assert_eq!(out.len(), 1);
        match &out[0] {
            AppEvent::TaskCancelled { session_id } => assert_eq!(session_id, "sess-1"),
            other => panic!("expected TaskCancelled, got {other:?}"),
        }
    }

    #[test]
    fn session_error_becomes_task_failed() {
        let mut ctx = EventContext::new();
        let err = mk_event(
            "session.error",
            None,
            json!({"errorType": "quota", "message": "予算上限に達しました"}),
        );
        let out = ctx.convert(&err);
        assert_eq!(out.len(), 1);
        match &out[0] {
            AppEvent::TaskFailed { error, .. } => {
                assert!(error.contains("予算上限に達しました"));
            }
            other => panic!("expected TaskFailed, got {other:?}"),
        }
    }

    #[test]
    fn resolve_cli_path_accepts_existing_configured_file() {
        let exe = std::env::current_exe().expect("current exe should resolve");
        let result = resolve_cli_path(Some(&exe));
        assert_eq!(result.unwrap(), exe);
    }

    #[test]
    fn resolve_cli_path_reports_friendly_error_for_missing_configured_file() {
        let missing = std::env::temp_dir().join("agent-deck-does-not-exist.exe");
        let err = resolve_cli_path(Some(&missing)).unwrap_err();
        assert!(err.contains("copilotCliPath"));
    }
}
