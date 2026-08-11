use crate::copilot::{RunOutcome, TaskStatus};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// data/history.jsonl の 1 行(docs/architecture.md §6.4)。追記のみ。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub session_id: String,
    pub agent_id: String,
    pub prompt: String,
    pub started_at: String,
    pub duration_ms: u64,
    /// completed / failed / cancelled — 出力フォルダの中途半端なファイルの
    /// 完成/未完の判別に使う(docs/architecture.md §8.3)
    pub status: String,
    pub output_files: Vec<String>,
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub subagents: Vec<SubagentRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRecord {
    pub name: String,
    pub duration_ms: u64,
}

/// RunOutcome(copilot::run_task の戻り値)から履歴 1 行分を組み立てる。
/// copilot::RunOutcome を引数に取るが、SDK の型ではなくアプリ独自の型なのでモジュール境界として許容する
/// (docs/architecture.md §4: SDK の型を直接流さない、の対象は SDK 型そのもの)。
pub fn entry_from_outcome(agent_id: &str, prompt: &str, outcome: &RunOutcome) -> HistoryEntry {
    let status = match outcome.status {
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    };
    HistoryEntry {
        session_id: outcome.session_id.clone(),
        agent_id: agent_id.to_string(),
        prompt: prompt.to_string(),
        started_at: outcome.started_at.clone(),
        duration_ms: outcome.duration_ms,
        status: status.to_string(),
        output_files: outcome.output_files.clone(),
        total_tokens: outcome.total_tokens,
        subagents: outcome
            .subagents
            .iter()
            .map(|s| SubagentRecord { name: s.name.clone(), duration_ms: s.duration_ms })
            .collect(),
    }
}

pub fn append(data_dir: &Path, entry: &HistoryEntry) -> Result<(), String> {
    let path = data_dir.join("history.jsonl");
    let line = serde_json::to_string(entry).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{} を開けません: {e}", path.display()))?;
    writeln!(file, "{line}").map_err(|e| format!("履歴を書き込めません: {e}"))
}

/// 新しい順に最大 limit 件。壊れた行は無視せずエラーにする(docs/development.md §3)。
pub fn list(data_dir: &Path, limit: usize) -> Result<Vec<HistoryEntry>, String> {
    let path = data_dir.join("history.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{} を読めません: {e}", path.display()))?;
    let mut entries = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| format!("履歴の行が不正です: {e}: {l}")))
        .collect::<Result<Vec<HistoryEntry>, String>>()?;
    entries.reverse();
    entries.truncate(limit);
    Ok(entries)
}
