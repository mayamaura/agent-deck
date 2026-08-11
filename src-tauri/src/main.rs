// リリースビルドでコンソールウィンドウを出さない(Windows)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agents;
mod config;
// ステップ2(イベント可視化)で使用開始。使い始めたら allow を外すこと
#[allow(dead_code)]
mod events;
mod history;
// ステップ4(権限制御)で使用開始。使い始めたら allow を外すこと
#[allow(dead_code)]
mod permissions;

use config::AgentSettings;
use std::path::PathBuf;
use tauri::State;

/// data/ の解決結果を保持する。解決に失敗した場合もエラーを保持し、
/// 各コマンドがフロントへ理由を返す(docs/architecture.md §6.1: 起動時にエラーを出す)。
struct AppState {
    data_dir: Result<PathBuf, String>,
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

// --- 以下はステップ1(SDK 疎通)以降で copilot.rs とともに実装する ---

#[tauri::command]
fn start_task(_agent_id: String, _prompt: String) -> Result<String, String> {
    Err("未実装: ステップ1で github-copilot-sdk の疎通確認後に実装します".into())
}

#[tauri::command]
fn cancel_task(_session_id: String) -> Result<(), String> {
    Err("未実装: ステップ1で実装します".into())
}

#[tauri::command]
fn respond_permission(_request_id: String, _decision: bool) -> Result<(), String> {
    Err("未実装: ステップ4で実装します".into())
}

fn main() {
    tauri::Builder::default()
        .manage(AppState {
            data_dir: config::data_dir(),
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
