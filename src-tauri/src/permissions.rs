use crate::config::AgentSettings;
use std::path::{Component, Path, PathBuf};

/// 権限判定の結果(docs/architecture.md §7.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 無条件で拒否(deniedTools に該当)
    Deny,
    /// 自動承認
    Approve,
    /// UI で確認(PermissionRequested を emit)
    Ask,
}

/// SDK の権限要求をアプリ独自に要約したもの。SDK の型をここに持ち込まない。
#[derive(Debug, Clone)]
pub struct PermissionInput {
    pub tool_name: String,
    /// 書き込み系ツールの場合の書き込み先
    pub write_path: Option<PathBuf>,
}

/// 判定ロジック(docs/architecture.md §7.1 の 1→4 の順)。
pub fn decide(settings: &AgentSettings, input: &PermissionInput) -> Decision {
    if settings.denied_tools.iter().any(|p| tool_matches(p, &input.tool_name)) {
        return Decision::Deny;
    }
    if settings.auto_approve_write_in_output_dir {
        if let (Some(output_dir), Some(write_path)) = (&settings.output_dir, &input.write_path) {
            if is_within(output_dir, write_path) {
                return Decision::Approve;
            }
        }
    }
    if settings.allowed_tools.iter().any(|p| tool_matches(p, &input.tool_name)) {
        return Decision::Approve;
    }
    Decision::Ask
}

/// ツールパターンの照合。
/// ponytail: 完全一致と末尾 `*` の前方一致のみ。`shell(python:*)` のような CLI 側の
/// 詳細記法は SDK の権限要求の実形が確定してから(ステップ4)拡張する。
fn tool_matches(pattern: &str, tool_name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => tool_name.starts_with(prefix),
        None => pattern == tool_name,
    }
}

/// `target` が `base` 配下かを正規化済み絶対パスで判定する(docs/architecture.md §7.3)。
/// 文字列前方一致は `..` やシンボリックリンクで脱出できるため禁止。
/// `target` は未作成ファイルのことがあるので、存在する最深の祖先を canonicalize し、
/// 残りの成分に `..` が含まれていたら不正として false を返す。
pub fn is_within(base: &Path, target: &Path) -> bool {
    let Ok(base) = base.canonicalize() else {
        // 出力フォルダ自体が存在しないなら自動承認の根拠にできない
        return false;
    };
    let Some(target) = normalize_possibly_missing(target) else {
        return false;
    };
    target.starts_with(&base)
}

/// 存在しない可能性のあるパスを正規化する。
/// 存在する最深の祖先を canonicalize し、残り成分を検査しながら連結する。
fn normalize_possibly_missing(path: &Path) -> Option<PathBuf> {
    if let Ok(p) = path.canonicalize() {
        return Some(p);
    }
    let parent = path.parent()?;
    let file_name = path.file_name()?;
    // 残り成分に `..` や `.` があれば不正(脱出の疑い)
    if matches!(
        Path::new(file_name).components().next(),
        Some(Component::ParentDir) | Some(Component::CurDir)
    ) {
        return None;
    }
    Some(normalize_possibly_missing(parent)?.join(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(output_dir: &Path) -> AgentSettings {
        AgentSettings {
            input_dir: None,
            output_dir: Some(output_dir.to_path_buf()),
            allowed_tools: vec!["read".into()],
            denied_tools: vec!["shell(rm)".into()],
            auto_approve_write_in_output_dir: true,
        }
    }

    #[test]
    fn output_dir_write_is_approved() {
        let dir = std::env::temp_dir().join("agent_deck_test_out");
        std::fs::create_dir_all(&dir).unwrap();
        let s = settings(&dir);
        let input = PermissionInput {
            tool_name: "write".into(),
            write_path: Some(dir.join("report.md")),
        };
        assert_eq!(decide(&s, &input), Decision::Approve);
    }

    #[test]
    fn dotdot_escape_is_not_approved() {
        let dir = std::env::temp_dir().join("agent_deck_test_out");
        std::fs::create_dir_all(&dir).unwrap();
        let s = settings(&dir);
        let input = PermissionInput {
            tool_name: "write".into(),
            write_path: Some(dir.join("..").join("escape.md")),
        };
        assert_eq!(decide(&s, &input), Decision::Ask);
    }

    #[test]
    fn denied_tool_wins_even_inside_output_dir() {
        let dir = std::env::temp_dir().join("agent_deck_test_out");
        std::fs::create_dir_all(&dir).unwrap();
        let s = settings(&dir);
        let input = PermissionInput {
            tool_name: "shell(rm)".into(),
            write_path: Some(dir.join("x")),
        };
        assert_eq!(decide(&s, &input), Decision::Deny);
    }

    #[test]
    fn unknown_tool_outside_output_dir_asks() {
        let dir = std::env::temp_dir().join("agent_deck_test_out");
        std::fs::create_dir_all(&dir).unwrap();
        let s = settings(&dir);
        let input = PermissionInput {
            tool_name: "shell(python)".into(),
            write_path: None,
        };
        assert_eq!(decide(&s, &input), Decision::Ask);
    }

    #[test]
    fn wildcard_pattern_matches_prefix() {
        assert!(tool_matches("shell(python:*", "shell(python:script.py)"));
        assert!(!tool_matches("shell(python:*", "shell(node:x)"));
    }
}
