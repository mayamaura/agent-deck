use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// ファイル内容の sha256 を16進文字列で返す。共有定義のバージョン(先頭8桁)と、
/// sync.rs のマニフェスト(files[id].sha256)の両方で使う共通ヘルパー。
/// ここに置く理由: `src-tauri/src/bin/*.rs` の検証バイナリは `#[path = "../agents.rs"]`
/// でこのファイルだけを個別コンパイルするため(step3/5 等の実行確認バイナリには lib クレートが
/// 無く main.rs のモジュールを直接 use できない、各バイナリ冒頭のコメント参照)、他モジュール
/// (sync.rs)への依存を持ち込むと検証バイナリ側のビルドが壊れる。依存を持たない sha2 直呼びに留める。
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// エージェント定義の由来スコープ(決定ログ 2026-08-12: 個人=agentDirs 編集可、
/// 共有=data/shared-agents 読み取り専用)。Ord は Personal < Shared とし、
/// 同名 id が並ぶ際に個人側を先頭にできるようにする(scan_definitions のソート)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentScope {
    Personal,
    Shared,
}

/// 一覧表示用のエージェント情報(docs/architecture.md §3 list_agents)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    /// ファイル名から拡張子を除いたもの(例: survey-analyst.agent.md → survey-analyst)
    pub id: String,
    pub name: String,
    pub description: String,
    pub source_path: PathBuf,
    pub scope: AgentScope,
    /// 共有定義のバージョン(sha256 先頭8桁)。個人定義は None(docs/roadmap.md v0.2)。
    pub version: Option<String>,
    /// true = 同名の個人定義に隠されている(実行には使われないが、一覧には残す)。
    pub shadowed: bool,
}

/// 実行時に SDK へ渡すための完全な定義(docs/sdk-notes.md「カスタムエージェント」節)。
/// `CustomAgentConfig` への詰め替えは copilot.rs の責務(SDK 型をこのモジュールに出さないため)。
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    /// ファイル名から拡張子を除いたもの(例: survey-analyst.agent.md → survey-analyst)
    pub id: String,
    pub name: String,
    pub description: String,
    /// None = 全ツール(CustomAgentConfig.tools の意味に合わせる)
    pub tools: Option<Vec<String>>,
    pub model: Option<String>,
    /// frontmatter 以降の本文全部(= SDK の CustomAgentConfig.prompt)
    pub body: String,
    pub source_path: PathBuf,
    pub scope: AgentScope,
    pub version: Option<String>,
    pub shadowed: bool,
}

/// personal_dirs(複数可・見つからなければエラー)+ shared_dir(単一・無ければ空扱い)を
/// 走査して .agent.md をフルパースし、個人優先で dedup する(決定ログ 2026-08-12)。
/// 同名 id が両方にある場合、共有側は shadowed=true として一覧には残すが、実行候補
/// (main.rs が組み立てる SDK 委任リスト)には含めない(呼び出し側が shadowed で除外する)。
/// 読めないファイルは握りつぶさず、エラーとして呼び出し側に返す。
pub fn scan_definitions(personal_dirs: &[PathBuf], shared_dir: &Path) -> Result<Vec<AgentDefinition>, String> {
    let mut agents = scan_dir(personal_dirs, AgentScope::Personal)?;
    let personal_ids: HashSet<&str> = agents.iter().map(|d| d.id.as_str()).collect();

    if shared_dir.is_dir() {
        let shared_path = shared_dir.to_path_buf();
        let mut shared = scan_dir(std::slice::from_ref(&shared_path), AgentScope::Shared)?;
        for d in &mut shared {
            d.shadowed = personal_ids.contains(d.id.as_str());
        }
        agents.extend(shared);
    }

    agents.sort_by(|a, b| a.id.cmp(&b.id).then(a.scope.cmp(&b.scope)));
    Ok(agents)
}

/// dirs 内の *.agent.md を読んでパースする(scan_definitions の内部ヘルパー)。
/// version は Shared スコープのみ計算する(sha256 先頭8桁。docs/roadmap.md v0.2)。
fn scan_dir(dirs: &[PathBuf], scope: AgentScope) -> Result<Vec<AgentDefinition>, String> {
    let mut agents = Vec::new();
    for dir in dirs {
        if !dir.is_dir() {
            return Err(format!("エージェント定義フォルダがありません: {}", dir.display()));
        }
        let entries = std::fs::read_dir(dir).map_err(|e| format!("{} を走査できません: {e}", dir.display()))?;
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(id) = file_name.strip_suffix(".agent.md") else {
                continue;
            };
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("{} を読めません: {e}", path.display()))?;
            let doc = parse_agent_md(&text);
            let version = match scope {
                AgentScope::Shared => Some(sha256_hex(text.as_bytes())[..8].to_string()),
                AgentScope::Personal => None,
            };
            agents.push(AgentDefinition {
                id: id.to_string(),
                name: doc.name.unwrap_or_else(|| id.to_string()),
                description: doc.description.unwrap_or_default(),
                tools: doc.tools,
                model: doc.model,
                body: doc.body,
                source_path: path,
                scope,
                version,
                shadowed: false,
            });
        }
    }
    Ok(agents)
}

/// 一覧表示用に AgentDefinition から必要な項目だけ取り出す(重複パース排除)。
pub fn scan(personal_dirs: &[PathBuf], shared_dir: &Path) -> Result<Vec<AgentSummary>, String> {
    Ok(scan_definitions(personal_dirs, shared_dir)?
        .into_iter()
        .map(|d| AgentSummary {
            id: d.id,
            name: d.name,
            description: d.description,
            source_path: d.source_path,
            scope: d.scope,
            version: d.version,
            shadowed: d.shadowed,
        })
        .collect())
}

#[derive(Default)]
struct ParsedAgentMd {
    name: Option<String>,
    description: Option<String>,
    tools: Option<Vec<String>>,
    model: Option<String>,
    body: String,
}

/// `---` で囲まれたフロントマターと、それ以降の本文をまとめてパースする。
/// ponytail: 単純な `key: value` 行のみ対応。tools はインライン YAML 配列
/// `["read", "edit"]` とカンマ区切り `read, edit` の 2 形式のみ扱う(CLI 仕様準拠、
/// sdk-notes.md の CLI 節)。複数行 YAML リストは非対応。v0.1 の .agent.md で使うのは
/// この形だけなので YAML クレートは追加しない。必要になったら serde_yaml を検討。
fn parse_agent_md(text: &str) -> ParsedAgentMd {
    let mut doc = ParsedAgentMd::default();
    let mut lines = text.lines().enumerate();
    let Some((_, first)) = lines.next() else {
        return doc;
    };
    if first.trim() != "---" {
        doc.body = text.trim().to_string();
        return doc;
    }
    let mut end_line = None;
    for (i, line) in lines.by_ref() {
        if line.trim() == "---" {
            end_line = Some(i);
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "name" => doc.name = Some(unquote(value)),
            "description" => doc.description = Some(unquote(value)),
            "model" => doc.model = Some(unquote(value)),
            "tools" => doc.tools = parse_tools(value),
            _ => {}
        }
    }
    let all_lines: Vec<&str> = text.lines().collect();
    doc.body = match end_line {
        Some(i) if i + 1 < all_lines.len() => all_lines[i + 1..].join("\n").trim().to_string(),
        _ => String::new(),
    };
    doc
}

/// tools フィールドの値をパースする。`None` は「フィールドなし」、
/// `Some(vec![])` は `tools: []`(ツールなし)を表す。
fn parse_tools(value: &str) -> Option<Vec<String>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let inner = value.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(value);
    let inner = inner.trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }
    Some(inner.split(',').map(unquote).collect())
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches('"').trim_matches('\'').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_description() {
        let text = "---\nname: survey-analyst\ndescription: アンケート集計\n---\n本文";
        let doc = parse_agent_md(text);
        assert_eq!(doc.name.as_deref(), Some("survey-analyst"));
        assert_eq!(doc.description.as_deref(), Some("アンケート集計"));
    }

    #[test]
    fn no_frontmatter_returns_empty_and_body_is_whole_text() {
        let doc = parse_agent_md("# ただの Markdown");
        assert!(doc.name.is_none());
        assert_eq!(doc.body, "# ただの Markdown");
    }

    #[test]
    fn extracts_body_after_closing_frontmatter_delimiter() {
        let text = "---\nname: a\n---\n\n# 役割\n本文1行目\n本文2行目\n";
        let doc = parse_agent_md(text);
        assert_eq!(doc.body, "# 役割\n本文1行目\n本文2行目");
    }

    #[test]
    fn parses_model_field() {
        let text = "---\nname: a\nmodel: claude-sonnet-4\n---\n本文";
        let doc = parse_agent_md(text);
        assert_eq!(doc.model.as_deref(), Some("claude-sonnet-4"));
    }

    #[test]
    fn parses_tools_inline_yaml_array() {
        let text = "---\nname: a\ntools: [\"read\", \"edit\"]\n---\n本文";
        let doc = parse_agent_md(text);
        assert_eq!(doc.tools, Some(vec!["read".to_string(), "edit".to_string()]));
    }

    #[test]
    fn parses_tools_comma_separated_string() {
        let text = "---\nname: a\ntools: read, edit\n---\n本文";
        let doc = parse_agent_md(text);
        assert_eq!(doc.tools, Some(vec!["read".to_string(), "edit".to_string()]));
    }

    #[test]
    fn parses_tools_empty_array_as_no_tools() {
        let text = "---\nname: a\ntools: []\n---\n本文";
        let doc = parse_agent_md(text);
        assert_eq!(doc.tools, Some(Vec::new()));
    }

    #[test]
    fn missing_tools_field_is_none() {
        let text = "---\nname: a\n---\n本文";
        let doc = parse_agent_md(text);
        assert!(doc.tools.is_none());
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent_deck_test_agents_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 同名 id が個人・共有の両方にある場合: 個人優先(実行候補は個人)、
    /// 共有側は shadowed=true として一覧には残る(決定ログ 2026-08-12)。
    #[test]
    fn personal_wins_over_shared_with_same_id_and_shared_is_shadowed() {
        let base = temp_dir("dedup");
        let personal = base.join("personal");
        let shared = base.join("shared");
        std::fs::create_dir_all(&personal).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(personal.join("survey.agent.md"), "---\nname: 個人版\n---\n個人の本文").unwrap();
        std::fs::write(shared.join("survey.agent.md"), "---\nname: 共有版\n---\n共有の本文").unwrap();
        std::fs::write(shared.join("only-shared.agent.md"), "---\nname: 共有のみ\n---\n本文").unwrap();

        let result = scan_definitions(&[personal], &shared).unwrap();
        let survey_entries: Vec<&AgentDefinition> = result.iter().filter(|d| d.id == "survey").collect();
        assert_eq!(survey_entries.len(), 2, "個人・共有の両方が一覧に残ること");

        let personal_entry = survey_entries.iter().find(|d| d.scope == AgentScope::Personal).unwrap();
        assert!(!personal_entry.shadowed);
        assert_eq!(personal_entry.name, "個人版");

        let shared_entry = survey_entries.iter().find(|d| d.scope == AgentScope::Shared).unwrap();
        assert!(shared_entry.shadowed, "共有側は個人版に隠されている");
        assert_eq!(shared_entry.name, "共有版");

        let only_shared = result.iter().find(|d| d.id == "only-shared").unwrap();
        assert!(!only_shared.shadowed, "個人版が無ければ shadowed にならない");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn shared_version_is_sha256_prefix_and_personal_has_no_version() {
        let base = temp_dir("version");
        let personal = base.join("personal");
        let shared = base.join("shared");
        std::fs::create_dir_all(&personal).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        let text = "---\nname: a\n---\n本文";
        std::fs::write(personal.join("a.agent.md"), text).unwrap();
        std::fs::write(shared.join("b.agent.md"), text).unwrap();

        let result = scan_definitions(&[personal], &shared).unwrap();
        let personal_entry = result.iter().find(|d| d.id == "a").unwrap();
        assert!(personal_entry.version.is_none());

        let shared_entry = result.iter().find(|d| d.id == "b").unwrap();
        let expected = &sha256_hex(text.as_bytes())[..8];
        assert_eq!(shared_entry.version.as_deref(), Some(expected));

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn missing_shared_dir_is_empty_not_error() {
        let base = temp_dir("no_shared");
        let personal = base.join("personal");
        std::fs::create_dir_all(&personal).unwrap();
        std::fs::write(personal.join("a.agent.md"), "---\nname: a\n---\n本文").unwrap();
        let shared = base.join("does-not-exist");

        let result = scan_definitions(&[personal], &shared).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].scope, AgentScope::Personal);

        std::fs::remove_dir_all(&base).ok();
    }
}
