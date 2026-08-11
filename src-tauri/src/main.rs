// リリースビルドでコンソールウィンドウを出さない(Windows)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agents;
mod config;
mod copilot;
mod events;
mod history;
mod permissions;

use config::AgentSettings;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

/// v0.1 の同時実行数上限。1 本固定だが、上限をこの定数に集約しておくことで
/// 将来の並行実行(roadmap.md v0.5)対応時にここだけ変更すれば済むようにする
/// (docs/open-questions.md #4 / roadmap.md「セッション同時1本をデータ構造に焼き込まない」)。
const MAX_CONCURRENT_TASKS: usize = 1;

/// 実行中タスクの管理情報。session_id は TaskStarted イベント受信時に埋まる
/// (start_task 呼び出し時点ではまだセッションが存在しないため)。
struct RunningTask {
    session_id: String,
    cancel: tokio::sync::oneshot::Sender<()>,
}

/// data/ の解決結果を保持する。解決に失敗した場合もエラーを保持し、
/// 各コマンドがフロントへ理由を返す(docs/architecture.md §6.1: 起動時にエラーを出す)。
struct AppState {
    data_dir: Result<PathBuf, String>,
    // MAX_CONCURRENT_TASKS == 1 の間は Option で足りる。
    running: Mutex<Option<RunningTask>>,
    /// respond_permission コマンドと copilot::run_task の PermissionHandler を橋渡しする
    /// (docs/architecture.md §7.1)。
    bridge: std::sync::Arc<copilot::PermissionBridge>,
}

impl AppState {
    fn data_dir(&self) -> Result<&PathBuf, String> {
        self.data_dir.as_ref().map_err(Clone::clone)
    }
}

#[tauri::command]
fn list_agents(state: State<AppState>) -> Result<Vec<agents::AgentSummary>, String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    if cfg.agent_dirs.is_empty() {
        return Err("エージェント定義フォルダが未設定です。data/config.json の agentDirs を設定してください".into());
    }
    agents::scan(&cfg.agent_dirs)
}

#[tauri::command]
fn get_agent_config(state: State<AppState>, agent_id: String) -> Result<Option<AgentSettings>, String> {
    let data_dir = state.data_dir()?;
    Ok(config::load_agents_config(data_dir)?.agents.get(&agent_id).cloned())
}

#[tauri::command]
fn save_agent_config(state: State<AppState>, agent_id: String, settings: AgentSettings) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let mut cfg = config::load_agents_config(data_dir)?;
    cfg.version = 1;
    cfg.agents.insert(agent_id, settings);
    config::save_agents_config(data_dir, &cfg)
}

#[tauri::command]
fn list_history(state: State<AppState>, limit: usize) -> Result<Vec<history::HistoryEntry>, String> {
    let data_dir = state.data_dir()?;
    history::list(data_dir, limit)
}

#[tauri::command]
fn open_output_folder(state: State<AppState>, agent_id: String) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_agents_config(data_dir)?;
    let dir = cfg
        .agents
        .get(&agent_id)
        .and_then(|a| a.output_dir.clone())
        .ok_or("出力フォルダが未設定です")?;
    // Windows 専用アプリなので explorer を直接呼ぶ(プラグイン不要)
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()
        .map_err(|e| format!("エクスプローラを起動できません: {e}"))?;
    Ok(())
}

#[tauri::command]
fn start_task(app: tauri::AppHandle, state: State<AppState>, agent_id: String, prompt: String) -> Result<(), String> {
    {
        let running = state.running.lock().unwrap();
        let running_count = if running.is_some() { 1 } else { 0 };
        if running_count >= MAX_CONCURRENT_TASKS {
            return Err("タスクが実行中です".into());
        }
    }

    let data_dir = state.data_dir()?.clone();
    let cfg = config::load_app_config(&data_dir)?;
    let cli_path = copilot::resolve_cli_path(cfg.copilot_cli_path.as_deref())?;

    // エージェント定義を全件読み、選択された agent_id に一致するものを探す。
    // 選択外の定義もセッションに渡す(SDK 側の自動委任の候補にするため。docs/sdk-notes.md「カスタムエージェント」節)。
    let definitions = agents::scan_definitions(&cfg.agent_dirs)?;
    let selected = definitions
        .iter()
        .find(|d| d.id == agent_id)
        .ok_or_else(|| format!("エージェント {agent_id} が見つかりません"))?
        .clone();
    let agent_specs: Vec<copilot::AgentSpec> = definitions
        .iter()
        .map(|d| copilot::AgentSpec {
            name: d.name.clone(),
            display_name: None,
            description: d.description.clone(),
            tools: d.tools.clone(),
            model: d.model.clone(),
            prompt: d.body.clone(),
        })
        .collect();

    // 作業ディレクトリ: agents.json の inputDir の親、無ければ専用ワークスペース
    // (docs/architecture.md §7.2: ユーザープロファイル直下や無関係なファイルを含む
    // 親フォルダを作業ディレクトリにしない)。
    let agents_cfg = config::load_agents_config(&data_dir)?;
    let working_directory = match agents_cfg.agents.get(&agent_id).and_then(|s| s.input_dir.clone()) {
        Some(input_dir) => input_dir.parent().map(PathBuf::from).unwrap_or(input_dir),
        None => {
            let dir = data_dir.join("workspace").join(&agent_id);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("ワークスペースフォルダを作成できません({}): {e}", dir.display()))?;
            dir
        }
    };

    // session_model は暫定運用: SDK 側の仕様(CustomAgentConfig.model はセッションモデルへの
    // フォールバック付き上書き)により、エージェント定義の model が実質最優先になる。
    // ここで渡す config.defaultModel はその親(フォールバック先)。優先順位の明文化は
    // docs/open-questions.md #3 が未決のため、確定させない(暫定コメントとして残す)。
    // rules は agents.json の該当エージェント設定から構成する。未設定なら既定
    // (allowed/denied 空、output_dir 無し、auto_approve true。docs/architecture.md §7.1)。
    let rules = agents_cfg.agents.get(&agent_id).cloned().unwrap_or_default();

    // 履歴書き込み用に、TaskSpec へ move する前に複製しておく(docs/development.md ステップ6)。
    let prompt_for_history = prompt.clone();
    let spec = copilot::TaskSpec {
        prompt,
        agent_id: agent_id.clone(),
        agents: agent_specs,
        selected_agent_name: selected.name,
        working_directory,
        session_model: cfg.default_model.clone(),
        rules,
        bridge: state.bridge.clone(),
    };

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    {
        // 冒頭のチェックとこの set の間に別の start_task が割り込む可能性があるため、
        // 同一ロック内で再チェックしてから登録する(TOCTOU 防止)。
        let mut running = state.running.lock().unwrap();
        if running.is_some() {
            return Err("タスクが実行中です".into());
        }
        *running = Some(RunningTask {
            session_id: String::new(),
            cancel: cancel_tx,
        });
    }

    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let sink_handle = app_handle.clone();
        let sink = move |ev: events::AppEvent| {
            // TaskStarted でセッション ID が判明した時点で RunningTask に反映する
            // (cancel_task が session_id を突き合わせられるようにするため)。
            if let events::AppEvent::TaskStarted { session_id, .. } = &ev {
                if let Some(state) = sink_handle.try_state::<AppState>() {
                    if let Some(running) = state.running.lock().unwrap().as_mut() {
                        running.session_id = session_id.clone();
                    }
                }
            }
            if let Err(e) = sink_handle.emit(events::EVENT_CHANNEL, &ev) {
                eprintln!("イベント送信に失敗しました: {e}");
            }
        };

        match copilot::run_task(cli_path, spec, cancel_rx, sink).await {
            Ok(outcome) => {
                // 監査ログ: 履歴ファイルに残らない summary(完了時の最終メッセージ/失敗時のエラー文)
                // をここで記録しておく。
                eprintln!(
                    "[history] session={} status={:?} summary={}",
                    outcome.session_id, outcome.status, outcome.summary
                );
                // タスク自体は(成功/失敗/中断のいずれでも)終わっているため、追記に失敗しても
                // TaskFailed イベントは出さず eprintln に留める(UI にはもう出しようがない)。
                let entry = history::entry_from_outcome(&agent_id, &prompt_for_history, &outcome);
                if let Err(e) = history::append(&data_dir, &entry) {
                    eprintln!("履歴の追記に失敗しました: {e}");
                }
            }
            // TaskStarted 前(セッション起動失敗等)の失敗。履歴の主キーである session_id が
            // 無いため履歴対象外(copilot::run_task のコメント参照)。
            Err(e) => eprintln!("タスクの実行に失敗しました: {e}"),
        }

        if let Some(state) = app_handle.try_state::<AppState>() {
            *state.running.lock().unwrap() = None;
        }
    });

    Ok(())
}

#[tauri::command]
fn cancel_task(state: State<AppState>, session_id: String) -> Result<(), String> {
    let mut running = state.running.lock().unwrap();
    match running.take() {
        Some(task) if task.session_id == session_id => {
            // 受信側(run_task)が既に終了していれば送信は失敗するが、握りつぶしてよい
            // (二重中断や完了直後の中断は正常なタイミング差であり、エラーではない)。
            let _ = task.cancel.send(());
            Ok(())
        }
        Some(other) => {
            let running_id = other.session_id.clone();
            *running = Some(other);
            Err(format!(
                "指定されたセッション({session_id})は実行中ではありません(実行中: {running_id})"
            ))
        }
        None => Err("実行中のタスクはありません".into()),
    }
}

#[tauri::command]
fn respond_permission(state: State<AppState>, request_id: String, decision: bool) -> Result<(), String> {
    state.bridge.respond(&request_id, decision)
}

fn main() {
    tauri::Builder::default()
        // フォルダ選択ダイアログ用(docs/requirements.md §3.4)。
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            data_dir: config::data_dir(),
            running: Mutex::new(None),
            bridge: copilot::PermissionBridge::new(),
        })
        .invoke_handler(tauri::generate_handler![
            list_agents,
            get_agent_config,
            save_agent_config,
            list_history,
            open_output_folder,
            start_task,
            cancel_task,
            respond_permission,
        ])
        .run(tauri::generate_context!())
        .expect("Tauri の起動に失敗しました");
}
