use serde::Serialize;
use std::path::PathBuf;

/// エージェント定義の由来スコープ。
/// v0.1 は Personal のみだが、v0.2 の共有/個人スコープ分離のために
/// 読み込み時点から保持しておく(docs/roadmap.md)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentScope {
    Personal,
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
}

/// agent_dirs を走査して .agent.md をフルパースする(docs/requirements.md §3.2)。
/// 読めないファイルは握りつぶさず、エラーとして呼び出し側に返す。
pub fn scan_definitions(agent_dirs: &[PathBuf]) -> Result<Vec<AgentDefinition>, String> {
    let mut agents = Vec::new();
    for dir in agent_dirs {
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
            agents.push(AgentDefinition {
                id: id.to_string(),
                name: doc.name.unwrap_or_else(|| id.to_string()),
                description: doc.description.unwrap_or_default(),
                tools: doc.tools,
                model: doc.model,
                body: doc.body,
                source_path: path,
                scope: AgentScope::Personal,
            });
        }
    }
    agents.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(agents)
}

/// 一覧表示用に AgentDefinition から必要な項目だけ取り出す(重複パース排除)。
pub fn scan(agent_dirs: &[PathBuf]) -> Result<Vec<AgentSummary>, String> {
    Ok(scan_definitions(agent_dirs)?
        .into_iter()
        .map(|d| AgentSummary {
            id: d.id,
            name: d.name,
            description: d.description,
            source_path: d.source_path,
            scope: d.scope,
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
}
