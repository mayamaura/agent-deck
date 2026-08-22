// リリースビルドでコンソールウィンドウを出さない(Windows)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agents;
mod audit;
mod config;
mod copilot;
mod events;
mod history;
mod permissions;
mod schedule;
mod sync;
mod update;

use config::{AgentSettings, AppConfig};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{Emitter, Manager, State};

/// 実行中タスク 1 本分の管理情報(docs/roadmap.md v0.5: 並行実行)。
/// キーは AppState.running の run_id(起動時に採番。SDK のセッション作成前は
/// session_id が定まらないため、こちらを HashMap のキーにする)。
/// session_id は TaskStarted イベント受信時に埋まる。
struct RunningTask {
    run_id: String,
    session_id: String,
    agent_id: String,
    /// 同一 outputDir を使うタスクの同時実行を防ぐための判定に使う(docs/roadmap.md v0.5)。
    output_dir: Option<PathBuf>,
    cancel: tokio::sync::oneshot::Sender<()>,
}

/// スケジュール発火 → キュー投入された 1 件(docs/roadmap.md v0.4: 発火が重なっても
/// キューで直列消化する)。
struct QueuedRun {
    agent_id: String,
    prompt: String,
}

/// data/ の解決結果を保持する。解決に失敗した場合もエラーを保持し、
/// 各コマンドがフロントへ理由を返す(docs/architecture.md §6.1: 起動時にエラーを出す)。
struct AppState {
    data_dir: Result<PathBuf, String>,
    /// 実行中タスクの集合(docs/roadmap.md v0.5: 並行実行)。キーは run_id。
    running: Mutex<HashMap<String, RunningTask>>,
    /// run_id 採番用のカウンタ。
    next_run_id: AtomicU64,
    /// respond_permission コマンドと copilot::run_task の PermissionHandler を橋渡しする
    /// (docs/architecture.md §7.1)。
    bridge: std::sync::Arc<copilot::PermissionBridge>,
    /// respond_user_input コマンドと copilot::run_task の UserInputHandler を橋渡しする
    /// (v1.0: エージェントからの質問への回答、経路A)。
    user_input_bridge: std::sync::Arc<copilot::UserInputBridge>,
    /// スケジュール実行の待機キュー(docs/roadmap.md v0.4)。
    queue: Mutex<VecDeque<QueuedRun>>,
}

impl AppState {
    fn data_dir(&self) -> Result<&PathBuf, String> {
        self.data_dir.as_ref().map_err(Clone::clone)
    }
}

/// 個人スコープの走査対象フォルダ一覧(決定ログ 2026-08-12)。
/// agentDirs が設定されていればそれを全て、未設定なら data/agents/ を自動作成して使う
/// (初回起動でもエラーにせず個人スコープが動くようにするため。旧: agentDirs 未設定はエラー)。
fn personal_scan_dirs(cfg: &AppConfig, data_dir: &Path) -> Result<Vec<PathBuf>, String> {
    if cfg.agent_dirs.is_empty() {
        Ok(vec![config::personal_agent_dir(cfg, data_dir)?])
    } else {
        Ok(cfg.agent_dirs.clone())
    }
}

/// エージェント ID をファイル名の一部として使う箇所(create_agent_definition)向けの検証。
/// パス区切りや `..` を含む ID はフォルダ脱出につながるため拒否する
/// (docs/development.md §3: パス判定は正規化済み絶対パスで、の趣旨に沿い、
/// そもそも脱出可能な文字を入力段階で弾く)。
fn validate_agent_id(agent_id: &str) -> Result<(), String> {
    if agent_id.is_empty() || agent_id.contains(['/', '\\']) || agent_id == "." || agent_id == ".." {
        return Err(format!("不正なエージェント ID です: {agent_id}"));
    }
    Ok(())
}

/// 同じ outputDir を使う実行中タスクがあれば、拒否理由の文言を返す(docs/roadmap.md v0.5:
/// 成果物の混線防止)。outputDir 未設定同士(どちらも None)は対象外(制限しない)。
/// パス比較は正規化済み絶対パスで行う(docs/development.md §3、permissions::normalize_possibly_missing
/// を再利用)。純関数に切り出してユニットテストで担保する(spawn_task から呼ぶ)。
fn output_dir_conflict(running: &[(String, Option<PathBuf>)], candidate: &Option<PathBuf>) -> Option<String> {
    let candidate_dir = candidate.as_ref()?;
    let candidate_norm = permissions::normalize_possibly_missing(candidate_dir)?;
    for (agent_id, dir) in running {
        let Some(dir) = dir else { continue };
        if permissions::normalize_possibly_missing(dir).as_deref() == Some(candidate_norm.as_path()) {
            return Some(format!("同じ出力フォルダを使うタスクが実行中です({agent_id})"));
        }
    }
    None
}

/// provenance.agentVersion の計算(docs/roadmap.md v0.6): 共有定義は同期マニフェストの
/// sha256 先頭8桁、個人定義はファイル内容の sha256 先頭8桁を、書き込み時点でその都度計算する
/// (agents::AgentDefinition.version はスキャン時点のキャッシュのため使わない)。
/// 読めない・見つからない場合はタスク結果自体には影響させず "(unknown)" で埋める。
fn agent_version_for_provenance(data_dir: &Path, scope: agents::AgentScope, source_path: &Path) -> String {
    const UNKNOWN: &str = "(unknown)";
    match scope {
        agents::AgentScope::Shared => {
            let meta_path = config::shared_agents_meta_path(data_dir);
            let Some(file_name) = source_path.file_name().and_then(|n| n.to_str()) else {
                return UNKNOWN.to_string();
            };
            match sync::load_manifest(&meta_path) {
                Ok(Some(manifest)) => manifest
                    .files
                    .get(file_name)
                    .map(|e| e.sha256[..8].to_string())
                    .unwrap_or_else(|| UNKNOWN.to_string()),
                _ => UNKNOWN.to_string(),
            }
        }
        agents::AgentScope::Personal => std::fs::read(source_path)
            .map(|bytes| agents::sha256_hex(&bytes)[..8].to_string())
            .unwrap_or_else(|_| UNKNOWN.to_string()),
    }
}

/// frontmatter + 本文の .agent.md テキストを組み立てる(save/create_agent_definition 共通)。
fn render_agent_md(name: &str, description: &str, model: &Option<String>, tools: &Option<Vec<String>>, body: &str) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {name}\n"));
    out.push_str(&format!("description: {description}\n"));
    if let Some(model) = model {
        out.push_str(&format!("model: {model}\n"));
    }
    if let Some(tools) = tools {
        let list = tools.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ");
        out.push_str(&format!("tools: [{list}]\n"));
    }
    out.push_str("---\n\n");
    out.push_str(body.trim());
    out.push('\n');
    out
}

#[tauri::command]
fn list_agents(state: State<AppState>) -> Result<Vec<agents::AgentSummary>, String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    let personal_dirs = personal_scan_dirs(&cfg, data_dir)?;
    let shared_dir = config::shared_agents_dir(data_dir);
    agents::scan(&personal_dirs, &shared_dir)
}

/// エディタ表示用の完全な定義(docs/roadmap.md v0.2)。SDK 型は含まない。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentDefinitionDto {
    id: String,
    name: String,
    description: String,
    tools: Option<Vec<String>>,
    model: Option<String>,
    body: String,
    scope: agents::AgentScope,
    version: Option<String>,
    source_path: PathBuf,
}

impl From<agents::AgentDefinition> for AgentDefinitionDto {
    fn from(d: agents::AgentDefinition) -> Self {
        Self {
            id: d.id,
            name: d.name,
            description: d.description,
            tools: d.tools,
            model: d.model,
            body: d.body,
            scope: d.scope,
            version: d.version,
            source_path: d.source_path,
        }
    }
}

#[tauri::command]
fn get_agent_definition(state: State<AppState>, agent_id: String) -> Result<AgentDefinitionDto, String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    let personal_dirs = personal_scan_dirs(&cfg, data_dir)?;
    let shared_dir = config::shared_agents_dir(data_dir);
    let definitions = agents::scan_definitions(&personal_dirs, &shared_dir)?;
    // shadowed(個人版に隠された共有版)は返さない。個人が勝つ(決定ログ 2026-08-12)。
    definitions
        .into_iter()
        .find(|d| d.id == agent_id && !d.shadowed)
        .map(AgentDefinitionDto::from)
        .ok_or_else(|| format!("エージェント定義が見つかりません: {agent_id}"))
}

#[tauri::command]
fn save_agent_definition(
    state: State<AppState>,
    agent_id: String,
    name: String,
    description: String,
    tools: Option<Vec<String>>,
    model: Option<String>,
    body: String,
) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    let personal_dirs = personal_scan_dirs(&cfg, data_dir)?;
    let shared_dir = config::shared_agents_dir(data_dir);
    let definitions = agents::scan_definitions(&personal_dirs, &shared_dir)?;
    let existing = definitions
        .iter()
        .find(|d| d.id == agent_id && d.scope == agents::AgentScope::Personal)
        .ok_or_else(|| format!("個人のエージェント定義が見つかりません(共有定義は編集できません): {agent_id}"))?;
    let content = render_agent_md(&name, &description, &model, &tools, &body);
    std::fs::write(&existing.source_path, content)
        .map_err(|e| format!("{} に保存できません: {e}", existing.source_path.display()))
}

#[tauri::command]
fn create_agent_definition(
    state: State<AppState>,
    agent_id: String,
    name: String,
    description: String,
    tools: Option<Vec<String>>,
    model: Option<String>,
    body: String,
) -> Result<(), String> {
    validate_agent_id(&agent_id)?;
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    let personal_dirs = personal_scan_dirs(&cfg, data_dir)?;
    let shared_dir = config::shared_agents_dir(data_dir);
    let definitions = agents::scan_definitions(&personal_dirs, &shared_dir)?;
    if definitions.iter().any(|d| d.id == agent_id) {
        return Err(format!("エージェント定義が既に存在します: {agent_id}"));
    }
    let target_dir = config::personal_agent_dir(&cfg, data_dir)?;
    let path = target_dir.join(format!("{agent_id}.agent.md"));
    let content = render_agent_md(&name, &description, &model, &tools, &body);
    std::fs::write(&path, content).map_err(|e| format!("{} に保存できません: {e}", path.display()))
}

/// 共有定義を同じ id で個人スコープへコピーする(「複製して編集」。決定ログ 2026-08-12)。
/// 個人優先の dedup により、以後は複製したファイルが実行・編集対象になる。
/// エージェント定義の下書きを Copilot に作らせる(docs/roadmap.md v1.1 (b))。
/// 返すのはテキストだけで、ファイルに書くのは利用者が「保存」を押したとき
/// (save_agent_definition)。生成セッションはツールを持たない(copilot::draft_agent)。
#[tauri::command]
async fn draft_agent_definition(app: tauri::AppHandle, request: String) -> Result<copilot::DraftedAgent, String> {
    if request.trim().is_empty() {
        return Err("どんなエージェントを作りたいかを入力してください".to_string());
    }
    // State のガード(!Send)を await に跨がせないよう、必要な値をここで取り切る。
    let (cli_path, model, workdir) = {
        let state = app.state::<AppState>();
        let data_dir = state.data_dir()?.clone();
        let cfg = config::load_app_config(&data_dir)?;
        let cli_path = copilot::resolve_cli_path(cfg.copilot_cli_path.as_deref())?;
        // 下書き生成はファイルに触らないが、SDK に渡す作業フォルダはアプリ管理下に限定する
        // (docs/architecture.md §7.2)。
        let workdir = data_dir.join("workspace");
        std::fs::create_dir_all(&workdir)
            .map_err(|e| format!("ワークスペースフォルダを作成できません({}): {e}", workdir.display()))?;
        (cli_path, cfg.default_model.clone(), workdir)
    };
    copilot::draft_agent(cli_path, model, workdir, request).await
}

/// 定義エディタのモデル選択肢(docs/requirements.md §3.4)。一覧はアプリに持たず毎回
/// Copilot に問い合わせる: 契約プランごとの差と、将来のモデル追加・廃止に自動で追随する。
#[tauri::command]
async fn list_models(app: tauri::AppHandle) -> Result<copilot::ModelCatalog, String> {
    // State のガード(!Send)を await に跨がせないよう、必要な値をここで取り切る。
    let cli_path = {
        let state = app.state::<AppState>();
        let data_dir = state.data_dir()?.clone();
        let cfg = config::load_app_config(&data_dir)?;
        copilot::resolve_cli_path(cfg.copilot_cli_path.as_deref())?
    };
    copilot::list_models(cli_path).await
}

#[tauri::command]
fn duplicate_agent(state: State<AppState>, agent_id: String) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    let personal_dirs = personal_scan_dirs(&cfg, data_dir)?;
    let shared_dir = config::shared_agents_dir(data_dir);
    let definitions = agents::scan_definitions(&personal_dirs, &shared_dir)?;
    if definitions.iter().any(|d| d.id == agent_id && d.scope == agents::AgentScope::Personal) {
        return Err(format!("個人版が既に存在します: {agent_id}"));
    }
    let shared = definitions
        .iter()
        .find(|d| d.id == agent_id && d.scope == agents::AgentScope::Shared)
        .ok_or_else(|| format!("共有エージェント定義が見つかりません: {agent_id}"))?;
    let target_dir = config::personal_agent_dir(&cfg, data_dir)?;
    let dest = target_dir.join(format!("{agent_id}.agent.md"));
    let text = std::fs::read_to_string(&shared.source_path)
        .map_err(|e| format!("{} を読めません: {e}", shared.source_path.display()))?;
    std::fs::write(&dest, text).map_err(|e| format!("{} に保存できません: {e}", dest.display()))
}

/// 個人定義のエージェント ID(= ファイル名)を作成後に変更する(ユーザー要望)。
/// ID は agents.json / schedules.json / data/workspace/<id> のキーでもあるので、
/// .agent.md のリネームだけでは設定とスケジュールが迷子になる。まとめて移し替える。
/// 履歴・監査ログ(data/logs)は「その時点で何が動いたか」の記録なので書き換えない。
#[tauri::command]
fn rename_agent_definition(state: State<AppState>, agent_id: String, new_id: String) -> Result<(), String> {
    validate_agent_id(&new_id)?;
    if new_id == agent_id {
        return Ok(());
    }
    let data_dir = state.data_dir()?;
    // 実行中に定義を動かすと、その実行の追い返信・スケジュールが旧 ID を指したままになるので止める。
    if state.running.lock().unwrap().values().any(|t| t.agent_id == agent_id) {
        return Err(format!("実行中のエージェントは ID を変更できません: {agent_id}"));
    }
    let cfg = config::load_app_config(data_dir)?;
    let personal_dirs = personal_scan_dirs(&cfg, data_dir)?;
    let shared_dir = config::shared_agents_dir(data_dir);
    let definitions = agents::scan_definitions(&personal_dirs, &shared_dir)?;
    if definitions.iter().any(|d| d.id == new_id) {
        return Err(format!("エージェント定義が既に存在します: {new_id}"));
    }
    let existing = definitions
        .iter()
        .find(|d| d.id == agent_id && d.scope == agents::AgentScope::Personal)
        .ok_or_else(|| format!("個人のエージェント定義が見つかりません(共有定義は ID を変更できません): {agent_id}"))?;
    let dest = existing.source_path.with_file_name(format!("{new_id}.agent.md"));
    std::fs::rename(&existing.source_path, &dest).map_err(|e| {
        format!("{} を {} に変更できません: {e}", existing.source_path.display(), dest.display())
    })?;
    rename_agent_data(data_dir, &agent_id, &new_id)
}

/// エージェント ID をキーに持つデータ(入出力設定・スケジュール・既定の作業フォルダ)を
/// 新 ID へ移す。rename_agent_definition が .agent.md のリネーム後に呼ぶ。
/// Tauri の State を触らないのでユニットテストできる。
fn rename_agent_data(data_dir: &Path, old_id: &str, new_id: &str) -> Result<(), String> {
    let mut agents_cfg = config::load_agents_config(data_dir)?;
    if let Some(settings) = agents_cfg.agents.remove(old_id) {
        agents_cfg.version = 1;
        agents_cfg.agents.insert(new_id.to_string(), settings);
        config::save_agents_config(data_dir, &agents_cfg)?;
    }

    let mut schedules = schedule::load(data_dir)?;
    let mut changed = false;
    for s in schedules.schedules.iter_mut().filter(|s| s.agent_id == old_id) {
        s.agent_id = new_id.to_string();
        changed = true;
    }
    if changed {
        schedule::save(data_dir, &schedules)?;
    }

    // 入力フォルダ未設定時の既定の作業フォルダ(spawn_task_inner が data/workspace/<id> を使う)。
    let workspace = data_dir.join("workspace");
    let (old_dir, new_dir) = (workspace.join(old_id), workspace.join(new_id));
    if old_dir.is_dir() && !new_dir.exists() {
        std::fs::rename(&old_dir, &new_dir)
            .map_err(|e| format!("{} を {} に変更できません: {e}", old_dir.display(), new_dir.display()))?;
    }
    Ok(())
}

#[tauri::command]
fn delete_agent_definition(state: State<AppState>, agent_id: String) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    let personal_dirs = personal_scan_dirs(&cfg, data_dir)?;
    let shared_dir = config::shared_agents_dir(data_dir);
    let definitions = agents::scan_definitions(&personal_dirs, &shared_dir)?;
    let existing = definitions
        .iter()
        .find(|d| d.id == agent_id && d.scope == agents::AgentScope::Personal)
        .ok_or_else(|| format!("個人のエージェント定義が見つかりません: {agent_id}"))?;
    std::fs::remove_file(&existing.source_path)
        .map_err(|e| format!("{} を削除できません: {e}", existing.source_path.display()))
}

#[tauri::command]
fn sync_shared_agents_cmd(state: State<AppState>) -> Result<sync::SyncSummary, String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    let source = cfg
        .shared_agents_source
        .ok_or("共有元フォルダが未設定です。設定画面で指定してください")?;
    let shared_dir = config::shared_agents_dir(data_dir);
    let meta_path = config::shared_agents_meta_path(data_dir);
    sync::sync_shared_agents(&source, &shared_dir, &meta_path)
}

/// GUI に見せるアプリ設定の一部(docs/roadmap.md v0.2 / v0.3)。設定はファイルが正であり
/// GUI はそれを読み書きするだけ(docs/development.md §3)。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AppConfigDto {
    shared_agents_source: Option<PathBuf>,
    default_model: Option<String>,
    update_source: Option<PathBuf>,
    /// 表示用の現行バージョン(docs/roadmap.md v0.3、UI に「agent-deck vX.Y.Z」を出す用途)。
    current_version: String,
    /// 管理者ポリシー(data/policy.json)の forcedDeniedTools(docs/roadmap.md v0.6)。
    /// 空なら UI 側で非表示にする。設定画面からは変更できない(表示のみ)。
    forced_denied_tools: Vec<String>,
}

#[tauri::command]
fn get_app_config(state: State<AppState>) -> Result<AppConfigDto, String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    let policy = config::load_policy(data_dir)?;
    Ok(AppConfigDto {
        shared_agents_source: cfg.shared_agents_source,
        default_model: cfg.default_model,
        update_source: cfg.update_source,
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        forced_denied_tools: policy.map(|p| p.forced_denied_tools).unwrap_or_default(),
    })
}

#[tauri::command]
fn save_shared_agents_source(state: State<AppState>, path: Option<String>) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let mut cfg = config::load_app_config(data_dir)?;
    cfg.shared_agents_source = path.map(PathBuf::from);
    config::save_app_config(data_dir, &cfg)
}

/// check_for_updates の戻り値(docs/roadmap.md v0.3.0)。file_path は UI に出さない
/// (「配布フォルダを開く」は updateSource そのものを開くため不要。update::UpdateInfo 参照)。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInfoDto {
    version: String,
    notes: String,
    hash_ok: bool,
}

impl From<update::UpdateInfo> for UpdateInfoDto {
    fn from(u: update::UpdateInfo) -> Self {
        Self { version: u.version, notes: u.notes, hash_ok: u.hash_ok }
    }
}

/// updateSource が未設定なら Ok(None)(未設定は「更新確認をしない」であってエラーではない)。
#[tauri::command]
fn check_for_updates(state: State<AppState>) -> Result<Option<UpdateInfoDto>, String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    let Some(source) = cfg.update_source else { return Ok(None) };
    let info = update::check_updates(&source, env!("CARGO_PKG_VERSION"))?;
    Ok(info.map(UpdateInfoDto::from))
}

#[tauri::command]
fn save_update_source(state: State<AppState>, path: Option<String>) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let mut cfg = config::load_app_config(data_dir)?;
    cfg.update_source = path.map(PathBuf::from);
    config::save_app_config(data_dir, &cfg)
}

#[tauri::command]
fn open_update_folder(state: State<AppState>) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_app_config(data_dir)?;
    let dir = cfg.update_source.ok_or("更新配布フォルダが未設定です")?;
    open_in_explorer(&dir)
}

/// 出力フォルダ・作業フォルダが未設定のときの既定(docs/architecture.md §7.2)。
/// 両方未設定ならこの 1 つのフォルダが出力先と作業先を兼ねる。
fn default_workspace(data_dir: &Path, agent_id: &str) -> PathBuf {
    data_dir.join("workspace").join(agent_id)
}

/// Windows 専用アプリなので explorer を直接呼ぶ(プラグイン不要)。
fn open_in_explorer(dir: &Path) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(dir)
        .spawn()
        .map_err(|e| format!("エクスプローラを起動できません: {e}"))?;
    Ok(())
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

/// 出力／作業フォルダをエクスプローラで開く(docs/requirements.md §3.6)。未設定でも
/// start_task と同じ既定を開く — 未設定時はそこが実際の書き込み先なので、
/// 「未設定です」で断ると中身を確認できない。
fn open_agent_dir(
    state: State<AppState>,
    agent_id: &str,
    pick: fn(&AgentSettings) -> Option<PathBuf>,
) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let cfg = config::load_agents_config(data_dir)?;
    let dir = cfg
        .agents
        .get(agent_id)
        .and_then(pick)
        .unwrap_or_else(|| default_workspace(data_dir, agent_id));
    std::fs::create_dir_all(&dir).map_err(|e| format!("フォルダを作成できません({}): {e}", dir.display()))?;
    open_in_explorer(&dir)
}

#[tauri::command]
fn open_output_folder(state: State<AppState>, agent_id: String) -> Result<(), String> {
    open_agent_dir(state, &agent_id, |a| a.output_dir.clone())
}

#[tauri::command]
fn open_work_folder(state: State<AppState>, agent_id: String) -> Result<(), String> {
    open_agent_dir(state, &agent_id, |a| a.work_dir.clone())
}

/// 監査ログフォルダ(data/logs/)をエクスプローラで開く(docs/roadmap.md v0.6)。
/// エージェント非依存(セッション横断で全て data/logs/ 配下にあるため)。
#[tauri::command]
fn open_logs_folder(state: State<AppState>) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let dir = data_dir.join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{} を作成できません: {e}", dir.display()))?;
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()
        .map_err(|e| format!("エクスプローラを起動できません: {e}"))?;
    Ok(())
}

#[tauri::command]
fn start_task(app: tauri::AppHandle, agent_id: String, prompt: String) -> Result<(), String> {
    spawn_task(app, agent_id, prompt, "manual", false)
}

/// タスク完了後の追い返信(v1.0 経路B: docs/sdk-notes.md「セッション再開」節)。
/// 同じ session_id を resume_session_id として spawn_task_inner に渡す。実行中のセッションは
/// resume の対象にできない(継続すべき送信がまだ終わっていない)ため先に拒否する。
#[tauri::command]
fn reply_task(app: tauri::AppHandle, session_id: String, agent_id: String, message: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let already_running = state.running.lock().unwrap().values().any(|t| t.session_id == session_id);
    if already_running {
        return Err("実行中のセッションには返信できません".to_string());
    }
    spawn_task_inner(app, agent_id, message, "manual", false, Some(session_id))
}

/// 手動実行(start_task)とスケジュール実行(スケジューラループ)の共通処理
/// (docs/roadmap.md v0.4)。エージェント定義解決 → TaskSpec 構築 → 実行の spawn まで行う。
/// trigger は履歴の trigger 欄("manual"/"scheduled")、unattended は無人実行フラグ
/// (true なら Ask になった権限要求を即座に拒否する。docs/architecture.md §7.1)。
fn spawn_task(
    app: tauri::AppHandle,
    agent_id: String,
    prompt: String,
    trigger: &'static str,
    unattended: bool,
) -> Result<(), String> {
    spawn_task_inner(app, agent_id, prompt, trigger, unattended, None)
}

/// spawn_task / reply_task の共通本体(v1.0: 追い返信のため抽出)。resume_session_id が
/// Some なら TaskSpec に載せ、copilot::run_task が create_session ではなく resume_session を使う
/// (docs/sdk-notes.md「セッション再開」節)。
fn spawn_task_inner(
    app: tauri::AppHandle,
    agent_id: String,
    prompt: String,
    trigger: &'static str,
    unattended: bool,
    resume_session_id: Option<String>,
) -> Result<(), String> {
    let state = app.state::<AppState>();

    let data_dir = state.data_dir()?.clone();
    let cfg = config::load_app_config(&data_dir)?;
    let cli_path = copilot::resolve_cli_path(cfg.copilot_cli_path.as_deref())?;

    // 監査ログの置き場(docs/roadmap.md v0.6)。ファイル自体は copilot::run_task 側が
    // セッションID判明後に遅延生成するが、ディレクトリそのものはここで用意する。
    let logs_dir = data_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)
        .map_err(|e| format!("{} を作成できません: {e}", logs_dir.display()))?;

    // エージェント定義を全件(個人+共有)読み、選択された agent_id に一致するものを探す。
    // 選択外の定義もセッションに渡す(SDK 側の自動委任の候補にするため。docs/sdk-notes.md「カスタムエージェント」節)。
    // shadowed(個人版に隠された共有版)は実行候補から除外する(決定ログ 2026-08-12: 個人優先)。
    let personal_dirs = personal_scan_dirs(&cfg, &data_dir)?;
    let shared_dir = config::shared_agents_dir(&data_dir);
    let definitions: Vec<agents::AgentDefinition> = agents::scan_definitions(&personal_dirs, &shared_dir)?
        .into_iter()
        .filter(|d| !d.shadowed)
        .collect();
    if definitions.is_empty() {
        return Err(
            "エージェント定義がありません。agents フォルダに .agent.md を置くか、設定の共有元フォルダから同期してください"
                .to_string(),
        );
    }
    let selected = definitions
        .iter()
        .find(|d| d.id == agent_id)
        .ok_or_else(|| format!("エージェント {agent_id} が見つかりません"))?
        .clone();
    // 来歴(provenance)用に、selected の一部を TaskSpec へ move する前に複製しておく
    // (docs/roadmap.md v0.6)。
    let agent_name = selected.name.clone();
    let agent_source_path = selected.source_path.clone();
    let agent_scope = selected.scope;
    let model_for_provenance = selected
        .model
        .clone()
        .or_else(|| cfg.default_model.clone())
        .unwrap_or_else(|| "(SDK 既定)".to_string());
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

    // 作業ディレクトリ: agents.json の workDir、無ければ専用ワークスペース
    // (docs/architecture.md §7.2)。inputDir/outputDir からは推測しない —
    // 入力フォルダの親を使うと、入力に Documents を選んだだけでプロファイル直下が
    // 作業ディレクトリになってしまうため。
    let agents_cfg = config::load_agents_config(&data_dir)?;
    let default_dir = default_workspace(&data_dir, &agent_id);
    let working_directory = agents_cfg
        .agents
        .get(&agent_id)
        .and_then(|s| s.work_dir.clone())
        .unwrap_or_else(|| default_dir.clone());
    std::fs::create_dir_all(&working_directory)
        .map_err(|e| format!("作業フォルダを作成できません({}): {e}", working_directory.display()))?;

    // session_model は暫定運用: SDK 側の仕様(CustomAgentConfig.model はセッションモデルへの
    // フォールバック付き上書き)により、エージェント定義の model が実質最優先になる。
    // ここで渡す config.defaultModel はその親(フォールバック先)。優先順位の明文化は
    // docs/open-questions.md #3 が未決のため、確定させない(暫定コメントとして残す)。
    // rules は agents.json の該当エージェント設定から構成する。未設定なら既定
    // (allowed/denied 空、output_dir 無し、auto_approve true。docs/architecture.md §7.1)。
    let mut rules = agents_cfg.agents.get(&agent_id).cloned().unwrap_or_default();
    // 管理者ポリシー(data/policy.json)の forcedDeniedTools をマージする(docs/roadmap.md v0.6)。
    // UI からは変更できない(get_app_config で一覧表示するだけ)。
    if let Some(policy) = config::load_policy(&data_dir)? {
        rules.denied_tools = config::merge_forced_denied_tools(rules.denied_tools, &policy.forced_denied_tools);
    }
    // 出力フォルダ未設定なら作業フォルダと同じ既定に落とす(docs/architecture.md §7.2)。
    // 「成果物がどこにも決まっていない」状態を作らないため。自動承認(§7.1)と
    // [環境情報] の出力先もこの解決済みの値を見る。agents.json 自体は書き換えない。
    if rules.output_dir.is_none() {
        std::fs::create_dir_all(&default_dir)
            .map_err(|e| format!("出力フォルダを作成できません({}): {e}", default_dir.display()))?;
        rules.output_dir = Some(default_dir);
    }
    // 実行中タスクと同じ outputDir なら起動を拒否する(docs/roadmap.md v0.5: 成果物の混線防止)。
    let output_dir = rules.output_dir.clone();

    // 履歴書き込み用に、TaskSpec へ move する前に複製しておく(docs/development.md ステップ6)。
    let prompt_for_history = prompt.clone();
    // TaskStarted 前の起動失敗を UI に通知する際のセッション ID(下の Err アーム参照)。
    // resume なら元セッションへ失敗ターンとして届き、新規なら run_id の擬似セッションになる。
    let session_id_for_err = resume_session_id.clone();
    let spec = copilot::TaskSpec {
        prompt,
        agent_id: agent_id.clone(),
        agents: agent_specs,
        selected_agent_name: selected.name,
        working_directory,
        session_model: cfg.default_model.clone(),
        rules,
        bridge: state.bridge.clone(),
        user_input_bridge: state.user_input_bridge.clone(),
        unattended,
        logs_dir: logs_dir.clone(),
        resume_session_id,
    };

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let run_id = {
        // 実行数上限・同一 outputDir 排他の判定から登録までを同一ロック内で行う(TOCTOU 防止)。
        let mut running = state.running.lock().unwrap();
        if running.len() >= cfg.max_concurrent_tasks {
            return Err(format!("同時実行数の上限({})に達しています", cfg.max_concurrent_tasks));
        }
        let running_list: Vec<(String, Option<PathBuf>)> =
            running.values().map(|t| (t.agent_id.clone(), t.output_dir.clone())).collect();
        if let Some(reason) = output_dir_conflict(&running_list, &output_dir) {
            return Err(reason);
        }
        let run_id = format!("run-{}", state.next_run_id.fetch_add(1, Ordering::Relaxed));
        running.insert(
            run_id.clone(),
            RunningTask {
                run_id: run_id.clone(),
                session_id: String::new(),
                agent_id: agent_id.clone(),
                output_dir,
                cancel: cancel_tx,
            },
        );
        run_id
    };

    let app_handle = app.clone();
    let run_id_for_task = run_id.clone();
    tauri::async_runtime::spawn(async move {
        let sink_handle = app_handle.clone();
        let run_id_for_sink = run_id_for_task.clone();
        // 「常に許可」で agents.json へ書き戻す対象エージェント(docs/architecture.md §7.1 拡張)。
        // AllowRuleAdded イベント自体の agent_id は使わない(PermissionHandler はサブエージェント
        // 相関を持てないため常に None。どのエージェントに書くかは、ここで spawn_task が
        // 既に知っている agent_id を使う。copilot.rs のコメント参照)。
        let agent_id_for_sink = agent_id.clone();
        let sink = move |ev: events::AppEvent| {
            // TaskStarted でセッション ID が判明した時点で RunningTask に反映する
            // (cancel_task が session_id を突き合わせられるようにするため)。
            if let events::AppEvent::TaskStarted { session_id, .. } = &ev {
                if let Some(state) = sink_handle.try_state::<AppState>() {
                    if let Some(running) = state.running.lock().unwrap().get_mut(&run_id_for_sink) {
                        running.session_id = session_id.clone();
                    }
                }
            }
            // 「常に許可」の永続化(docs/architecture.md §7.1 拡張)。PermissionHandler 側では
            // SDK 型・設定ファイルへの書き込みを行わない設計のため、ここ(sink)で agents.json に
            // 書き戻す。失敗しても実行中のタスク自体は継続する(eprintln のみ)。
            if let events::AppEvent::AllowRuleAdded { pattern, .. } = &ev {
                if let Some(state) = sink_handle.try_state::<AppState>() {
                    if let Ok(data_dir) = state.data_dir() {
                        match config::load_agents_config(data_dir) {
                            Ok(mut cfg) => {
                                let entry = cfg
                                    .agents
                                    .entry(agent_id_for_sink.clone())
                                    .or_insert_with(AgentSettings::default);
                                if !entry.allowed_tools.contains(pattern) {
                                    entry.allowed_tools.push(pattern.clone());
                                    if let Err(e) = config::save_agents_config(data_dir, &cfg) {
                                        eprintln!("常に許可ルールの保存に失敗しました: {e}");
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("agents.json の読み込みに失敗しました(常に許可ルールを保存できません): {e}")
                            }
                        }
                    }
                }
            }
            if let Err(e) = sink_handle.emit(events::EVENT_CHANNEL, &ev) {
                eprintln!("イベント送信に失敗しました: {e}");
            }
        };

        match copilot::run_task(cli_path, spec, cancel_rx, sink).await {
            Ok(outcome) => {
                // デバッグ用: 履歴ファイルに残らない summary(完了時の最終メッセージ/失敗時のエラー文)
                // をここで記録しておく(data/logs/session-*.jsonl の監査ログとは別物)。
                eprintln!(
                    "[history] session={} status={:?} summary={}",
                    outcome.session_id, outcome.status, outcome.summary
                );
                // タスク自体は(成功/失敗/中断のいずれでも)終わっているため、追記に失敗しても
                // TaskFailed イベントは出さず eprintln に留める(UI にはもう出しようがない)。
                let entry = history::entry_from_outcome(&agent_id, &prompt_for_history, &outcome, trigger);
                if let Err(e) = history::append(&data_dir, &entry) {
                    eprintln!("履歴の追記に失敗しました: {e}");
                }
                // 来歴(provenance)の記録(docs/roadmap.md v0.6)。history 追記と同様、タスク自体は
                // 既に終わっているため、失敗しても eprintln に留める(TaskFailed化しない)。
                let agent_version = agent_version_for_provenance(&data_dir, agent_scope, &agent_source_path);
                let prov = audit::Provenance {
                    session_id: outcome.session_id.clone(),
                    agent_id: agent_id.clone(),
                    agent_name: agent_name.clone(),
                    agent_version,
                    agent_source_path: agent_source_path.display().to_string(),
                    model: model_for_provenance.clone(),
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                    prompt: prompt_for_history.clone(),
                    started_at: outcome.started_at.clone(),
                    duration_ms: outcome.duration_ms,
                    status: outcome.status.as_str().to_string(),
                    input_files: outcome.input_files.clone(),
                    output_files: outcome.output_files.clone(),
                };
                if let Err(e) = audit::write_provenance(&logs_dir, &prov) {
                    eprintln!("来歴の記録に失敗しました: {e}");
                }
                notify_task_result(&app_handle, &agent_id, &outcome);
            }
            // TaskStarted 前(セッション起動失敗等)の失敗。履歴の主キーである session_id が
            // 無いため履歴対象外(copilot::run_task のコメント参照)だが、UI に理由を出すため
            // 擬似セッションとして TaskStarted + TaskFailed を emit する(エラーを握りつぶさない。
            // フロントはこれで「起動中…」表示も解除する)。
            Err(e) => {
                eprintln!("タスクの実行に失敗しました: {e}");
                let sid = session_id_for_err.unwrap_or_else(|| run_id_for_task.clone());
                let evs = [
                    events::AppEvent::TaskStarted {
                        session_id: sid.clone(),
                        agent_id: agent_id.clone(),
                        started_at: copilot::format_rfc3339_now(),
                        prompt: prompt_for_history.clone(),
                    },
                    events::AppEvent::TaskFailed {
                        session_id: sid,
                        error: format!("タスクを開始できませんでした: {e}"),
                    },
                ];
                for ev in evs {
                    if let Err(e) = app_handle.emit(events::EVENT_CHANNEL, &ev) {
                        eprintln!("イベント送信に失敗しました: {e}");
                    }
                }
            }
        }

        if let Some(state) = app_handle.try_state::<AppState>() {
            state.running.lock().unwrap().remove(&run_id_for_task);
        }
    });

    Ok(())
}

/// タスク終了時の Windows トースト通知(docs/roadmap.md v0.4)。ベストエフォート:
/// 送信に失敗してもタスクの結果自体には影響させない(eprintln のみ)。中断は通知しない
/// (指示どおりの動作であり、失敗として騒ぐ必要が無いため)。
fn notify_task_result(app: &tauri::AppHandle, agent_id: &str, outcome: &copilot::RunOutcome) {
    use tauri_plugin_notification::NotificationExt;
    let body = match outcome.status {
        copilot::TaskStatus::Completed => format!("{agent_id} のタスクが完了しました"),
        copilot::TaskStatus::Failed => format!("失敗しました: {}", outcome.summary),
        copilot::TaskStatus::Cancelled => return,
    };
    if let Err(e) = app.notification().builder().title("agent-deck").body(body).show() {
        eprintln!("通知の送信に失敗しました: {e}");
    }
}

/// スケジューラのポーリング間隔(docs/roadmap.md v0.4)。アプリ(ウィンドウ)起動中のみ動作する。
const SCHEDULER_TICK: Duration = Duration::from_secs(30);

/// setup フックから spawn するスケジューラループ本体。
async fn scheduler_loop(app: tauri::AppHandle) {
    loop {
        tokio::time::sleep(SCHEDULER_TICK).await;
        if let Err(e) = scheduler_tick(&app) {
            eprintln!("スケジューラの処理に失敗しました: {e}");
        }
    }
}

/// スケジューラ1回分の処理: (1) 発火判定 → キュー投入 → last_run_at 書き戻し、
/// (2) 実行中でなければキュー先頭を1件実行する(docs/roadmap.md v0.4: 直列消化)。
fn scheduler_tick(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let data_dir = state.data_dir()?.clone();
    let cfg = config::load_app_config(&data_dir)?;

    let mut file = schedule::load(&data_dir)?;
    let now = chrono::Local::now();
    let mut due_runs = Vec::new();
    for sch in file.schedules.iter_mut() {
        match schedule::is_due(sch, now) {
            Ok(true) => {
                due_runs.push(QueuedRun { agent_id: sch.agent_id.clone(), prompt: sch.prompt.clone() });
                sch.last_run_at = Some(now.to_rfc3339());
            }
            Ok(false) => {}
            Err(e) => eprintln!("スケジュール {} の発火判定に失敗しました: {e}", sch.id),
        }
    }
    if !due_runs.is_empty() {
        schedule::save(&data_dir, &file)?;
        state.queue.lock().unwrap().extend(due_runs);
    }

    // v0.5: 「実行中でなければ」ではなく「実行数 < 上限」でキューを消化する(docs/roadmap.md v0.5)。
    let should_try =
        state.running.lock().unwrap().len() < cfg.max_concurrent_tasks && !state.queue.lock().unwrap().is_empty();
    if should_try {
        let run = state.queue.lock().unwrap().pop_front();
        if let Some(run) = run {
            if let Err(e) = spawn_task(app.clone(), run.agent_id.clone(), run.prompt.clone(), "scheduled", true) {
                // 手動実行との競合など。キューの先頭へ戻して次回 tick で再試行する
                // (再試行禁止の対象はタスクの失敗であり、開始そのものの取りこぼし防止は別問題)。
                state.queue.lock().unwrap().push_front(run);
                eprintln!("スケジュール実行を開始できませんでした(次回再試行): {e}");
            }
        }
    }
    Ok(())
}

#[tauri::command]
fn list_schedules(state: State<AppState>) -> Result<Vec<schedule::Schedule>, String> {
    let data_dir = state.data_dir()?;
    Ok(schedule::load(data_dir)?.schedules)
}

/// 新規・既存を id で判別して upsert する(id はフロントが新規作成時に発行する)。
#[tauri::command]
fn save_schedule(state: State<AppState>, schedule: schedule::Schedule) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let mut file = schedule::load(data_dir)?;
    match file.schedules.iter_mut().find(|s| s.id == schedule.id) {
        Some(existing) => *existing = schedule,
        None => file.schedules.push(schedule),
    }
    schedule::save(data_dir, &file)
}

#[tauri::command]
fn delete_schedule(state: State<AppState>, id: String) -> Result<(), String> {
    let data_dir = state.data_dir()?;
    let mut file = schedule::load(data_dir)?;
    let before = file.schedules.len();
    file.schedules.retain(|s| s.id != id);
    if file.schedules.len() == before {
        return Err(format!("スケジュールが見つかりません: {id}"));
    }
    schedule::save(data_dir, &file)
}

/// 実行ビュー付近の「待機中のスケジュール実行: n 件」表示用(docs/roadmap.md v0.4)。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct QueueStatusDto {
    queued: usize,
}

#[tauri::command]
fn get_queue_status(state: State<AppState>) -> QueueStatusDto {
    QueueStatusDto { queued: state.queue.lock().unwrap().len() }
}

#[tauri::command]
fn cancel_task(state: State<AppState>, session_id: String) -> Result<(), String> {
    let mut running = state.running.lock().unwrap();
    let run_id = running.values().find(|t| t.session_id == session_id).map(|t| t.run_id.clone());
    match run_id {
        Some(run_id) => {
            let task = running.remove(&run_id).expect("run_id はこの直前の検索で確認済み");
            // 受信側(run_task)が既に終了していれば送信は失敗するが、握りつぶしてよい
            // (二重中断や完了直後の中断は正常なタイミング差であり、エラーではない)。
            let _ = task.cancel.send(());
            Ok(())
        }
        None => Err(format!("指定されたセッション({session_id})は実行中ではありません")),
    }
}

/// decision は "approveOnce" / "approveAlways" / "deny"(承認ダイアログの3択。
/// docs/architecture.md §7.1 拡張)。SDK の型をフロントに流さないのと同様、フロントには
/// 文字列だけ持たせ、Rust 側で PermissionReply に変換する。
#[tauri::command]
fn respond_permission(state: State<AppState>, request_id: String, decision: String) -> Result<(), String> {
    let reply = match decision.as_str() {
        "approveOnce" => copilot::PermissionReply::ApproveOnce,
        "approveAlways" => copilot::PermissionReply::ApproveAlways,
        "deny" => copilot::PermissionReply::Deny,
        other => return Err(format!("不明な決定です: {other}")),
    };
    state.bridge.respond(&request_id, reply)
}

/// ask_user への回答(v1.0 経路A)。answer=None は「回答しない」(SDK へは「回答なし」を返す)。
#[tauri::command]
fn respond_user_input(state: State<AppState>, request_id: String, answer: Option<String>) -> Result<(), String> {
    state.user_input_bridge.respond(&request_id, answer)
}

fn main() {
    tauri::Builder::default()
        // フォルダ選択ダイアログ用(docs/requirements.md §3.4)。
        .plugin(tauri_plugin_dialog::init())
        // 完了・失敗のトースト通知用(docs/roadmap.md v0.4)。
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            data_dir: config::data_dir(),
            running: Mutex::new(HashMap::new()),
            next_run_id: AtomicU64::new(1),
            bridge: copilot::PermissionBridge::new(),
            user_input_bridge: copilot::UserInputBridge::new(),
            queue: Mutex::new(VecDeque::new()),
        })
        .setup(|app| {
            // スケジューラはアプリ(ウィンドウ)起動中のみ動作する(docs/roadmap.md v0.4: 常駐は未決のため実装しない)。
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(scheduler_loop(handle.clone()));

            // 起動時にログ保持期間(docs/open-questions.md #5、既定90日)を過ぎた監査ログ・
            // 来歴を削除する。失敗してもアプリ起動は妨げない(eprintln のみ)。
            let state = handle.state::<AppState>();
            if let Ok(data_dir) = state.data_dir() {
                match config::load_app_config(data_dir) {
                    Ok(cfg) => {
                        let logs_dir = data_dir.join("logs");
                        match audit::cleanup_old_logs(&logs_dir, cfg.log_retention_days) {
                            Ok(0) => {}
                            Ok(n) => eprintln!("保持期間を過ぎたログを {n} 件削除しました"),
                            Err(e) => eprintln!("古いログの削除に失敗しました: {e}"),
                        }
                    }
                    Err(e) => eprintln!("設定の読み込みに失敗しました(ログ保持期間の適用をスキップ): {e}"),
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_agents,
            get_agent_config,
            save_agent_config,
            get_agent_definition,
            save_agent_definition,
            create_agent_definition,
            draft_agent_definition,
            list_models,
            duplicate_agent,
            rename_agent_definition,
            delete_agent_definition,
            sync_shared_agents_cmd,
            get_app_config,
            save_shared_agents_source,
            check_for_updates,
            save_update_source,
            open_update_folder,
            list_history,
            open_output_folder,
            open_work_folder,
            open_logs_folder,
            start_task,
            reply_task,
            cancel_task,
            respond_permission,
            respond_user_input,
            list_schedules,
            save_schedule,
            delete_schedule,
            get_queue_status,
        ])
        .run(tauri::generate_context!())
        .expect("Tauri の起動に失敗しました");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// save/create_agent_definition が書き出す形式(render_agent_md)を
    /// scan_definitions で読み戻せることを確認する(定義の書き出し→読み戻し round-trip)。
    #[test]
    fn render_agent_md_round_trips_through_scan_definitions() {
        let base = std::env::temp_dir().join("agent_deck_test_main_roundtrip");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let shared = base.join("shared"); // 存在しない → scan_definitions は空扱い

        let tools = Some(vec!["read".to_string(), "write".to_string()]);
        let model = Some("claude-sonnet-4".to_string());
        let content = render_agent_md(
            "集計くん",
            "アンケート集計を行う",
            &model,
            &tools,
            "# 役割\n数字はスクリプトで出す",
        );
        std::fs::write(base.join("survey-analyst.agent.md"), content).unwrap();

        let defs = agents::scan_definitions(&[base.clone()], &shared).unwrap();
        assert_eq!(defs.len(), 1);
        let d = &defs[0];
        assert_eq!(d.id, "survey-analyst");
        assert_eq!(d.name, "集計くん");
        assert_eq!(d.description, "アンケート集計を行う");
        assert_eq!(d.model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(d.tools, Some(vec!["read".to_string(), "write".to_string()]));
        assert_eq!(d.body, "# 役割\n数字はスクリプトで出す");

        std::fs::remove_dir_all(&base).ok();
    }

    /// ID 変更時、入出力設定・スケジュール・既定の作業フォルダが新 ID へ付いてくる
    /// (付いてこないと設定が黙って消えたように見える)。
    #[test]
    fn rename_agent_data_moves_settings_schedules_and_workspace() {
        let dir = std::env::temp_dir().join("agent_deck_test_main_rename");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("workspace").join("old-id")).unwrap();
        std::fs::write(dir.join("workspace").join("old-id").join("memo.txt"), "作業中").unwrap();

        let mut agents_cfg = config::AgentsConfig::default();
        agents_cfg.agents.insert(
            "old-id".into(),
            AgentSettings { output_dir: Some(PathBuf::from("C:/out")), ..Default::default() },
        );
        config::save_agents_config(&dir, &agents_cfg).unwrap();
        schedule::save(
            &dir,
            &schedule::SchedulesFile {
                version: 1,
                schedules: vec![schedule::Schedule {
                    id: "s1".into(),
                    agent_id: "old-id".into(),
                    prompt: "p".into(),
                    recurrence: schedule::Recurrence::Daily { time: "09:00".into() },
                    enabled: true,
                    last_run_at: None,
                }],
            },
        )
        .unwrap();

        rename_agent_data(&dir, "old-id", "new-id").unwrap();

        let agents_cfg = config::load_agents_config(&dir).unwrap();
        assert!(!agents_cfg.agents.contains_key("old-id"));
        assert_eq!(agents_cfg.agents["new-id"].output_dir.as_deref(), Some(Path::new("C:/out")));
        assert_eq!(schedule::load(&dir).unwrap().schedules[0].agent_id, "new-id");
        assert!(dir.join("workspace").join("new-id").join("memo.txt").is_file());
        assert!(!dir.join("workspace").join("old-id").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_agent_id_rejects_path_separators_and_dotdot() {
        assert!(validate_agent_id("survey-analyst").is_ok());
        assert!(validate_agent_id("../escape").is_err());
        assert!(validate_agent_id("a/b").is_err());
        assert!(validate_agent_id("a\\b").is_err());
        assert!(validate_agent_id("").is_err());
    }

    /// docs/roadmap.md v0.5: 同じ outputDir を使うタスクは同時実行できない。
    #[test]
    fn output_dir_conflict_detects_same_normalized_path() {
        let dir = std::env::temp_dir().join("agent_deck_test_main_outputdir_conflict");
        std::fs::create_dir_all(&dir).unwrap();
        let running = vec![("agent-a".to_string(), Some(dir.clone()))];
        // 素朴な文字列表現は違う("./" 混入)が、正規化すれば同一パス。
        let candidate = Some(dir.parent().unwrap().join(".").join(dir.file_name().unwrap()));
        let reason = output_dir_conflict(&running, &candidate);
        assert_eq!(reason, Some("同じ出力フォルダを使うタスクが実行中です(agent-a)".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn output_dir_conflict_none_when_dirs_differ() {
        let base = std::env::temp_dir().join("agent_deck_test_main_outputdir_no_conflict");
        let a = base.join("a");
        let b = base.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let running = vec![("agent-a".to_string(), Some(a))];
        assert_eq!(output_dir_conflict(&running, &Some(b)), None);
        std::fs::remove_dir_all(&base).ok();
    }

    /// outputDir 未設定同士(どちらも None)は制限しない。
    #[test]
    fn output_dir_conflict_none_when_candidate_unset() {
        let dir = std::env::temp_dir().join("agent_deck_test_main_outputdir_unset");
        std::fs::create_dir_all(&dir).unwrap();
        let running = vec![("agent-a".to_string(), None), ("agent-b".to_string(), Some(dir.clone()))];
        assert_eq!(output_dir_conflict(&running, &None), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn personal_scan_dirs_falls_back_to_auto_created_data_agents_when_empty() {
        let base = std::env::temp_dir().join("agent_deck_test_main_personal_dirs");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = AppConfig::default();
        let dirs = personal_scan_dirs(&cfg, &base).unwrap();
        assert_eq!(dirs, vec![base.join("agents")]);
        assert!(base.join("agents").is_dir());
        std::fs::remove_dir_all(&base).ok();
    }
}
