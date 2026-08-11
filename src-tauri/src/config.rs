use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// data/config.json — アプリ全体の設定(docs/architecture.md §6.2)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub version: u32,
    pub agent_dirs: Vec<PathBuf>,
    pub copilot_cli_path: Option<PathBuf>,
    pub default_model: Option<String>,
    pub log_level: String,
    /// 共有エージェント定義の同期元フォルダ(docs/roadmap.md v0.2、決定ログ 2026-08-12: 共有フォルダ方式)。
    /// 既存の config.json に無くても読めるよう serde default(欠落時は None)。
    #[serde(default)]
    pub shared_agents_source: Option<PathBuf>,
    /// アプリ本体の更新配布フォルダ(docs/roadmap.md v0.3.0: 共有フォルダ+マニフェスト方式のみ)。
    /// 直下に manifest.json を置く想定(update::check_updates)。既存 config.json に無くても
    /// 読めるよう serde default(欠落時は None)。
    #[serde(default)]
    pub update_source: Option<PathBuf>,
    /// 同時実行数の上限(docs/roadmap.md v0.5、docs/open-questions.md #4: 暫定、既定 2)。
    /// 正式な上限値は運用を見て決定するため、決め打ちの定数ではなく設定値にしてある。
    /// 既存 config.json に無くても読めるよう serde default。
    #[serde(default = "default_max_concurrent_tasks")]
    pub max_concurrent_tasks: usize,
}

fn default_max_concurrent_tasks() -> usize {
    2
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            agent_dirs: Vec::new(),
            copilot_cli_path: None,
            default_model: None,
            log_level: "info".into(),
            shared_agents_source: None,
            update_source: None,
            max_concurrent_tasks: default_max_concurrent_tasks(),
        }
    }
}

/// data/agents.json — エージェントごとの運用設定(docs/architecture.md §6.3)。
/// .agent.md 本体は Copilot CLI の管轄なので、アプリは書き換えない。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsConfig {
    pub version: u32,
    pub agents: BTreeMap<String, AgentSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettings {
    pub input_dir: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_tools: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_approve_write_in_output_dir: bool,
}

fn default_true() -> bool {
    true
}

/// エージェント未設定時の既定値(docs/architecture.md §7.1: allowed/denied 空、
/// output_dir 無し、auto_approve true)。start_task が rules 構築に使う。
impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            input_dir: None,
            output_dir: None,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            auto_approve_write_in_output_dir: true,
        }
    }
}

/// exe と同階層の data/ を返す(docs/architecture.md §6.1、ポータブル運用)。
/// 書き込み不可の場所なら Err — 呼び出し側で起動エラーとして UI に出すこと。
pub fn data_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("実行ファイルの場所を取得できません: {e}"))?;
    let dir = exe
        .parent()
        .ok_or("実行ファイルの親フォルダがありません")?
        .join("data");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("データフォルダを作成できません({}): {e}。書き込み可能な場所に配置してください", dir.display()))?;
    // 実際に書けるか確認(Program Files 等の検出)
    let probe = dir.join(".write_test");
    fs::write(&probe, b"").map_err(|e| {
        format!("データフォルダに書き込めません({}): {e}。書き込み可能な場所に配置してください", dir.display())
    })?;
    let _ = fs::remove_file(&probe);
    Ok(dir)
}

pub fn load_app_config(data_dir: &Path) -> Result<AppConfig, String> {
    load_or_default(&data_dir.join("config.json"))
}

pub fn save_app_config(data_dir: &Path, config: &AppConfig) -> Result<(), String> {
    let path = data_dir.join("config.json");
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| format!("{} に保存できません: {e}", path.display()))
}

pub fn load_agents_config(data_dir: &Path) -> Result<AgentsConfig, String> {
    load_or_default(&data_dir.join("agents.json"))
}

/// 共有エージェント定義の同期先(docs/roadmap.md v0.2、アプリ内では読み取り専用)。
pub fn shared_agents_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("shared-agents")
}

/// 共有同期のマニフェスト置き場(sync::sync_shared_agents / load_manifest が使う)。
pub fn shared_agents_meta_path(data_dir: &Path) -> PathBuf {
    data_dir.join("shared-agents.meta.json")
}

/// 個人スコープの書き込み先(決定ログ 2026-08-12: 個人=agentDirs 編集可)。
/// agentDirs の先頭を使う。agentDirs が空なら data/agents/ を自動作成して使う
/// (初回起動でも個人スコープの作成・編集が成立するようにするため)。
pub fn personal_agent_dir(config: &AppConfig, data_dir: &Path) -> Result<PathBuf, String> {
    if let Some(first) = config.agent_dirs.first() {
        return Ok(first.clone());
    }
    let dir = data_dir.join("agents");
    fs::create_dir_all(&dir).map_err(|e| format!("{} を作成できません: {e}", dir.display()))?;
    Ok(dir)
}

pub fn save_agents_config(data_dir: &Path, config: &AgentsConfig) -> Result<(), String> {
    let path = data_dir.join("agents.json");
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| format!("{} に保存できません: {e}", path.display()))
}

/// ファイルが無ければ既定値。あれば読む。壊れていたら握りつぶさずエラーを返す(docs/development.md §3)。
fn load_or_default<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    if !path.exists() {
        return Ok(T::default());
    }
    let text = fs::read_to_string(path).map_err(|e| format!("{} を読めません: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{} の形式が不正です: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 受け入れ条件 9(再起動しても設定が保持される)の中核: 保存 → 読み戻しの round-trip。
    #[test]
    fn agents_config_roundtrip() {
        let dir = std::env::temp_dir().join("agent_deck_test_config");
        fs::create_dir_all(&dir).unwrap();
        let mut cfg = AgentsConfig { version: 1, agents: BTreeMap::new() };
        cfg.agents.insert(
            "survey-analyst".into(),
            AgentSettings {
                input_dir: Some(PathBuf::from("C:/work/in")),
                output_dir: Some(PathBuf::from("C:/work/out")),
                allowed_tools: vec!["write".into(), "shell(python:*)".into()],
                denied_tools: vec!["shell(rm)".into()],
                auto_approve_write_in_output_dir: true,
            },
        );
        save_agents_config(&dir, &cfg).unwrap();
        let loaded = load_agents_config(&dir).unwrap();
        let s = &loaded.agents["survey-analyst"];
        assert_eq!(s.input_dir.as_deref(), Some(Path::new("C:/work/in")));
        assert_eq!(s.allowed_tools, vec!["write", "shell(python:*)"]);
        assert!(s.auto_approve_write_in_output_dir);
        fs::remove_dir_all(&dir).ok();
    }

    /// 壊れた JSON は握りつぶさずエラーになる(docs/development.md §3)。
    #[test]
    fn broken_json_is_an_error() {
        let dir = std::env::temp_dir().join("agent_deck_test_config_broken");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("agents.json"), "{ こわれてる").unwrap();
        assert!(load_agents_config(&dir).is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
