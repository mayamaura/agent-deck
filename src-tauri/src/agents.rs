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

/// agent_dirs を走査して .agent.md を読む(docs/requirements.md §3.2)。
/// 読めないファイルは握りつぶさず、エラーとして呼び出し側に返す。
pub fn scan(agent_dirs: &[PathBuf]) -> Result<Vec<AgentSummary>, String> {
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
            let fm = parse_frontmatter(&text);
            agents.push(AgentSummary {
                id: id.to_string(),
                name: fm.name.unwrap_or_else(|| id.to_string()),
                description: fm.description.unwrap_or_default(),
                source_path: path,
                scope: AgentScope::Personal,
            });
        }
    }
    agents.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(agents)
}

#[derive(Default)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
}

/// `---` で囲まれたフロントマターから name / description を取り出す。
/// ponytail: 単純な `key: value` 行のみ対応。v0.1 で使うのはこの2キーだけなので
/// YAML クレートは追加しない。ネストや複数行値が必要になったら serde_yaml を検討。
fn parse_frontmatter(text: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return fm;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches('"').trim_matches('\'').to_string();
            match key.trim() {
                "name" => fm.name = Some(value),
                "description" => fm.description = Some(value),
                _ => {}
            }
        }
    }
    fm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_description() {
        let text = "---\nname: survey-analyst\ndescription: アンケート集計\n---\n本文";
        let fm = parse_frontmatter(text);
        assert_eq!(fm.name.as_deref(), Some("survey-analyst"));
        assert_eq!(fm.description.as_deref(), Some("アンケート集計"));
    }

    #[test]
    fn no_frontmatter_returns_empty() {
        let fm = parse_frontmatter("# ただの Markdown");
        assert!(fm.name.is_none());
    }
}
