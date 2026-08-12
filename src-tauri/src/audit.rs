// 監査ログ・来歴(docs/roadmap.md v0.6: 監査とガバナンス)。
//
// SDK/CLI の hooks 機構は使わない: 全 SessionEvent は既に EventContext::convert 経由で
// 購読しており、権限判定も UiPermissionHandler が全量を握っている(copilot.rs)。
// 自前記録で全量が取れるため、hooks を別途配線する必要はない(この判断はここに残す)。
//
// data/logs/ 配下に 2 種類のファイルを置く:
//   - session-<sessionId>.jsonl : 1 行 1 JSON、追記専用の監査ログ(全 AppEvent + 権限判定)
//   - provenance-<sessionId>.json : タスク完了時に 1 回だけ書く来歴(成果物の由来)

use crate::events::AppEvent;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// セッション単位の追記専用ログライタ。
/// ファイル(session-<sessionId>.jsonl)は sessionId 判明後に record_* が初めて呼ばれた
/// 時点で遅延生成される(コンストラクタの時点ではまだセッションが存在しないため)。
pub struct AuditWriter {
    logs_dir: PathBuf,
}

impl AuditWriter {
    pub fn new(logs_dir: &Path) -> Self {
        Self { logs_dir: logs_dir.to_path_buf() }
    }

    /// AppEvent をそのまま1行追記する(kind タグ込みで JSON 化される)。
    pub fn record_event(&self, session_id: &str, ev: &AppEvent) {
        self.append_line(session_id, ev);
    }

    /// 権限判定の結果を1行追記する。自動承認/自動拒否は AppEvent に出ないため、
    /// UiPermissionHandler の全判定経路(copilot.rs)から呼ばれて初めて記録される。
    pub fn record_permission(&self, session_id: &str, rec: &PermissionAudit) {
        self.append_line(session_id, rec);
    }

    /// ask_user への応答結果を1行追記する(v1.0: 経路A)。UserInputRequested の AppEvent 自体は
    /// record_event で記録されるため、これは最終判定(回答した/しなかった)のみを記す別記録
    /// (record_permission と同じ設計)。
    pub fn record_user_input(&self, session_id: &str, rec: &UserInputAudit) {
        self.append_line(session_id, rec);
    }

    /// 書き込み失敗は eprintln のみに留め、タスクの実行は止めない
    /// (監査ログの欠落自体はタスクの成否とは無関係。呼び出し側の指示どおり)。
    /// ponytail: 1呼び出しごとに open/close する簡易実装(history::append と同じ方式)。
    /// 同一セッションへの並行書き込み(権限判定が複数同時に走るケース)で行の交差が
    /// 起き得るが、1行=1 write() 呼び出しであり実運用の行長では実害が出ていない。
    /// 問題になれば単一ファイルハンドル+Mutex に切り替える。
    fn append_line<T: Serialize>(&self, session_id: &str, value: &T) {
        let path = self.logs_dir.join(format!("session-{session_id}.jsonl"));
        let line = match serde_json::to_string(value) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("監査ログの JSON 化に失敗しました: {e}");
                return;
            }
        };
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = result {
            eprintln!("監査ログを書き込めません({}): {e}", path.display());
        }
    }
}

/// 権限判定 1 件分の監査記録(docs/roadmap.md v0.6)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionAudit {
    pub timestamp: String,
    /// autoApproved / autoDenied / userApproved / userApprovedAlways / userDenied / unattendedDenied
    /// (userApprovedAlways は「常に許可」経路。docs/architecture.md §7.1 拡張)
    pub decision: String,
    pub tool_name: String,
    pub detail: Option<String>,
    pub write_path: Option<String>,
}

/// ask_user への応答 1 件分の監査記録(docs/sdk-notes.md「ユーザー入力」節、v1.0)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAudit {
    pub timestamp: String,
    /// userAnswered / userDeclined / unattendedNoAnswer
    pub decision: String,
    pub question: String,
    pub answer: Option<String>,
}

/// 成果物の来歴(docs/roadmap.md v0.6)。タスク完了時(RunOutcome 確定後)に main.rs が
/// 一度だけ data/logs/provenance-<sessionId>.json へ書く。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    /// 共有定義: 同期マニフェストの sha256 先頭8桁。個人定義: ファイル内容の sha256 先頭8桁
    /// (呼び出し側がその都度計算する。読めない/見つからない場合は "(unknown)")。
    pub agent_version: String,
    pub agent_source_path: String,
    /// 定義の model > config.defaultModel > "(SDK 既定)"(docs/open-questions.md #3 の暫定運用)。
    pub model: String,
    pub app_version: String,
    pub prompt: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub status: String,
    /// 権限承認された read のパス(ディレクトリ一覧の read も混ざるが、そのまま列挙する)。
    pub input_files: Vec<String>,
    pub output_files: Vec<String>,
}

/// provenance-<sessionId>.json へ書き出す。書き込み失敗はエラーを返す(呼び出し側の main.rs が
/// eprintln に留めるか判断する。history 追記の失敗と同じ扱いでタスク結果には影響させない)。
pub fn write_provenance(logs_dir: &Path, prov: &Provenance) -> Result<(), String> {
    std::fs::create_dir_all(logs_dir)
        .map_err(|e| format!("{} を作成できません: {e}", logs_dir.display()))?;
    let path = logs_dir.join(format!("provenance-{}.json", prov.session_id));
    let json = serde_json::to_string_pretty(prov).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("{} に保存できません: {e}", path.display()))
}

/// data/logs/ 配下の古いファイル(session-*.jsonl / provenance-*.json とも)を削除する
/// (docs/open-questions.md #5: 保持期間の暫定既定90日。0 は「無制限」で何もしない)。
/// history.jsonl はこの関数の対象外(data/logs/ の外にあるため、そもそも走査されない)。
pub fn cleanup_old_logs(logs_dir: &Path, retention_days: u32) -> Result<usize, String> {
    if retention_days == 0 || !logs_dir.is_dir() {
        return Ok(0);
    }
    let max_age = std::time::Duration::from_secs(retention_days as u64 * 86400);
    let cutoff = std::time::SystemTime::now()
        .checked_sub(max_age)
        .ok_or("保持期間の計算に失敗しました(値が大きすぎます)")?;

    let mut removed = 0usize;
    for entry in
        std::fs::read_dir(logs_dir).map_err(|e| format!("{} を走査できません: {e}", logs_dir.display()))?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .map_err(|e| format!("{} の更新日時を取得できません: {e}", path.display()))?;
        if modified < cutoff {
            std::fs::remove_file(&path).map_err(|e| format!("{} を削除できません: {e}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent_deck_test_audit_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn set_mtime(path: &Path, time: std::time::SystemTime) {
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(time).unwrap();
    }

    #[test]
    fn cleanup_removes_only_files_older_than_retention() {
        let dir = temp_dir("removes_old");
        let old = dir.join("session-old.jsonl");
        let recent = dir.join("session-recent.jsonl");
        std::fs::write(&old, "{}").unwrap();
        std::fs::write(&recent, "{}").unwrap();
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(200 * 86400);
        set_mtime(&old, old_time);

        let removed = cleanup_old_logs(&dir, 90).unwrap();
        assert_eq!(removed, 1);
        assert!(!old.exists(), "古いファイルは削除される");
        assert!(recent.exists(), "新しいファイルは残る");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cleanup_with_zero_retention_does_nothing() {
        let dir = temp_dir("zero_retention");
        let old = dir.join("provenance-old.json");
        std::fs::write(&old, "{}").unwrap();
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(400 * 86400);
        set_mtime(&old, old_time);

        let removed = cleanup_old_logs(&dir, 0).unwrap();
        assert_eq!(removed, 0);
        assert!(old.exists(), "0(無制限)では何も削除しない");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_event_and_record_permission_append_to_same_session_file() {
        let dir = temp_dir("append");
        let writer = AuditWriter::new(&dir);
        writer.record_event(
            "sess-1",
            &AppEvent::TaskStarted {
                session_id: "sess-1".to_string(),
                agent_id: "writer".to_string(),
                started_at: "2026-08-12T00:00:00Z".to_string(),
            },
        );
        writer.record_permission(
            "sess-1",
            &PermissionAudit {
                timestamp: "2026-08-12T00:00:01Z".to_string(),
                decision: "autoApproved".to_string(),
                tool_name: "write".to_string(),
                detail: None,
                write_path: Some("C:/out/report.md".to_string()),
            },
        );
        let text = std::fs::read_to_string(dir.join("session-sess-1.jsonl")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"taskStarted\""));
        assert!(lines[1].contains("\"autoApproved\""));

        std::fs::remove_dir_all(&dir).ok();
    }
}
