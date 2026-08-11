// SDK セッションの実行と、SessionEvent → AppEvent 変換(docs/development.md §4 ステップ2)。
// SDK の型はこのモジュールの外に出さない(docs/architecture.md §4)。
//
// payload のフィールド名は、ローカルの cargo レジストリに展開された SDK ソースを直接読んで
// 確認したもの(github-copilot-sdk-1.0.9/src/generated/session_events.rs)。sdk-notes.md に
// 「未観測」とあった tool.* / subagent.* もこのソース読みで確認済み。

use crate::events::AppEvent;
use github_copilot_sdk::handler::ApproveAllHandler;
use github_copilot_sdk::session_events::{
    AssistantIntentData, AssistantMessageData, AssistantUsageData, SessionErrorData,
    SessionUsageInfoData, SubagentCompletedData, SubagentFailedData, SubagentStartedData,
    ToolExecutionCompleteData, ToolExecutionStartData,
};
use github_copilot_sdk::types::{MessageOptions, SessionConfig, SessionEvent};
use github_copilot_sdk::{CliProgram, Client, ClientOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        }
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
            "session.idle" => self.on_session_idle(),
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
        // envelope の agent_id がサブエージェント固有の識別子(docs/architecture.md §5)。
        // 万一 started の時点で未付与でも tool_call_id で代替し、completed/failed 側の
        // フォールバック(下記)と一貫させる。
        let agent_id = ev.agent_id.clone().unwrap_or_else(|| data.tool_call_id.clone());
        vec![AppEvent::SubagentStarted {
            session_id: self.session_id.clone(),
            agent_id,
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
        let agent_id = ev.agent_id.clone().unwrap_or_else(|| data.tool_call_id.clone());
        vec![AppEvent::SubagentCompleted {
            session_id: self.session_id.clone(),
            agent_id,
            duration_ms,
            total_tokens: data.total_tokens.map(|v| v.max(0) as u64),
        }]
    }

    fn on_subagent_failed(&mut self, ev: &SessionEvent) -> Vec<AppEvent> {
        let Some(data) = ev.typed_data::<SubagentFailedData>() else {
            return Vec::new();
        };
        self.subagent_started_at.remove(&data.tool_call_id);
        let agent_id = ev.agent_id.clone().unwrap_or_else(|| data.tool_call_id.clone());
        vec![AppEvent::SubagentFailed {
            session_id: self.session_id.clone(),
            agent_id,
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
            tool_name,
            success: data.success,
        }]
    }

    fn on_assistant_usage(&self, ev: &SessionEvent) -> Vec<AppEvent> {
        let Some(data) = ev.typed_data::<AssistantUsageData>() else {
            return Vec::new();
        };
        // assistant.usage には current_tokens/token_limit が無いため、取れる input+output で代用。
        let current = data.input_tokens.unwrap_or(0) + data.output_tokens.unwrap_or(0);
        vec![AppEvent::UsageUpdated {
            session_id: self.session_id.clone(),
            current_tokens: current.max(0) as u64,
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

    fn on_session_idle(&self) -> Vec<AppEvent> {
        vec![AppEvent::TaskCompleted {
            session_id: self.session_id.clone(),
            summary: self.last_main_message.clone(),
            // ステップ6(履歴接続)で出力ファイル一覧を埋める。
            output_files: Vec::new(),
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
    Cancelled,
    Failed(String),
}

/// タスク 1 本の実行(Client 起動 → セッション作成 → 購読 → send_and_wait → 後始末)。
/// sink には変換済み AppEvent を渡すだけで、emit するのは呼び出し側の責務(main.rs)。
pub async fn run_task(
    cli_path: PathBuf,
    prompt: String,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
    sink: impl Fn(AppEvent) + Send + Sync + 'static,
) -> Result<(), String> {
    let client = Client::start(ClientOptions::new().with_program(CliProgram::Path(cli_path)))
        .await
        .map_err(|e| format!("Copilot CLI を起動できません: {e}"))?;

    // TODO(ステップ4): permissions::decide を呼ぶ自前 PermissionHandler に差し替える。
    // それまでは全承認固定(ApproveAllHandler)。
    let session = match client
        .create_session(SessionConfig::default().with_permission_handler(Arc::new(ApproveAllHandler)))
        .await
    {
        Ok(s) => s,
        Err(e) => {
            if let Err(stop_err) = client.stop().await {
                eprintln!("Client の停止に失敗しました: {stop_err}");
            }
            return Err(format!("セッションを作成できません: {e}"));
        }
    };

    let session_id = session.id().to_string();
    sink(AppEvent::TaskStarted {
        session_id: session_id.clone(),
        // TODO(ステップ3): エージェント選択を接続するまでは空文字。
        agent_id: String::new(),
        started_at: format_rfc3339_now(),
    });

    let mut ctx = EventContext::new();
    ctx.session_id = session_id.clone();
    let mut events = session.subscribe();
    // session.error 由来の TaskFailed が convert() 側で既に送られたかどうか。
    // 送られていれば run_task 側の失敗フォールバックで重複させない。
    let mut terminal_sent = false;

    let send_fut = session.send_and_wait(MessageOptions::new(prompt).with_wait_timeout(SEND_AND_WAIT_TIMEOUT));
    tokio::pin!(send_fut);

    let outcome = loop {
        tokio::select! {
            _ = &mut cancel => {
                break Outcome::Cancelled;
            }
            send_result = &mut send_fut => {
                loop {
                    match tokio::time::timeout(DRAIN_TIMEOUT, events.recv()).await {
                        Ok(Ok(event)) => {
                            for app_event in ctx.convert(&event) {
                                if matches!(app_event, AppEvent::TaskCompleted { .. } | AppEvent::TaskFailed { .. }) {
                                    terminal_sent = true;
                                }
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
                        if matches!(app_event, AppEvent::TaskCompleted { .. } | AppEvent::TaskFailed { .. }) {
                            terminal_sent = true;
                        }
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

    match outcome {
        Outcome::Cancelled => {
            sink(AppEvent::TaskCancelled { session_id });
            Ok(())
        }
        Outcome::Completed => Ok(()),
        Outcome::Failed(e) => {
            // session.error 経由なら convert() 側で TaskFailed 済み。タイムアウト等
            // クライアント側のみの失敗はここで補う(エラーを握りつぶさない)。
            if !terminal_sent {
                sink(AppEvent::TaskFailed { session_id, error: e.clone() });
            }
            Err(e)
        }
    }
}

/// std に無い RFC3339 風フォーマット(秒精度)。新規クレート追加禁止(chrono 等不可)のため自前実装。
/// Howard Hinnant の civil_from_days アルゴリズム(グレゴリオ暦、UTC 前提)。
fn format_rfc3339_now() -> String {
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
