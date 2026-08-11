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

/// agents.json の運用設定を権限判定に使う際の呼び名(docs/architecture.md §6.3)。
/// AgentSettings と同一の型で足りるため、別構造体は起こさずエイリアスにする。
pub type PermissionRules = AgentSettings;

/// SDK の権限要求をアプリ独自に要約したもの。SDK の型をここに持ち込まない。
#[derive(Debug, Clone)]
pub struct PermissionInput {
    /// ツール種別名(例: "shell", "write")。CLI の --allow-tool/--deny-tool の NAME 部分に対応。
    pub tool_name: String,
    /// shell ならコマンド文字列、他ツールなら引数やパスなどの要約。
    pub detail: Option<String>,
    /// 書き込み系ツールの場合の書き込み先
    pub write_path: Option<PathBuf>,
    /// read ツールの場合の読み込み対象(docs/roadmap.md v0.6: 来歴 provenance.inputFiles 用)。
    /// write_path と同様、kind が read のときだけ埋める(ディレクトリ一覧の read も混ざるが、
    /// そのまま列挙してよい仕様)。
    pub read_path: Option<PathBuf>,
}

/// 判定ロジック(docs/architecture.md §7.1 の 1→4 の順)。
pub fn decide(settings: &AgentSettings, input: &PermissionInput) -> Decision {
    if settings.denied_tools.iter().any(|p| tool_matches(p, input)) {
        return Decision::Deny;
    }
    if settings.auto_approve_write_in_output_dir {
        if let (Some(output_dir), Some(write_path)) = (&settings.output_dir, &input.write_path) {
            if is_within(output_dir, write_path) {
                return Decision::Approve;
            }
        }
    }
    if settings.allowed_tools.iter().any(|p| tool_matches(p, input)) {
        return Decision::Approve;
    }
    Decision::Ask
}

/// ツールパターンの照合(CLI の `--allow-tool` / `--deny-tool` 記法。docs/sdk-notes.md CLI 節)。
///
/// パターンは `名前` または `名前(フィルタ)`。
/// - `名前` のみ(例 `write`, `shell`)→ input.tool_name と一致すれば detail 不問でマッチ
/// - `名前(フィルタ)` で フィルタ が `接頭辞:*` の形(例 `shell(python:*)`)→
///   detail が `接頭辞` で始まるか(ワイルドカードは CLI の仕様上 shell 専用なので、
///   名前が "shell" でなければ常に不一致)
/// - `名前(フィルタ)` でそれ以外(例 `shell(rm)`)→ detail の最初のトークン(空白区切り)が
///   フィルタと完全一致するか
fn tool_matches(pattern: &str, input: &PermissionInput) -> bool {
    let (name, filter) = match pattern.split_once('(') {
        Some((name, rest)) => (name, Some(rest.strip_suffix(')').unwrap_or(rest))),
        None => (pattern, None),
    };
    if name != input.tool_name {
        return false;
    }
    let Some(filter) = filter else {
        return true;
    };
    let detail = input.detail.as_deref().unwrap_or("");
    if let Some(prefix) = filter.strip_suffix(":*") {
        // CLI 仕様: "Wildcards are only supported for shell"
        return name == "shell" && detail.starts_with(prefix);
    }
    detail.split_whitespace().next() == Some(filter)
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
/// pub(crate): main.rs の output_dir_conflict(docs/roadmap.md v0.5、同一 outputDir 検出)が
/// 「配下判定」ではなく「同一パス判定」に同じ正規化ロジックを再利用する。
pub(crate) fn normalize_possibly_missing(path: &Path) -> Option<PathBuf> {
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
            detail: None,
            write_path: Some(dir.join("report.md")),
            read_path: None,
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
            detail: None,
            write_path: Some(dir.join("..").join("escape.md")),
            read_path: None,
        };
        assert_eq!(decide(&s, &input), Decision::Ask);
    }

    #[test]
    fn denied_tool_wins_even_inside_output_dir() {
        let dir = std::env::temp_dir().join("agent_deck_test_out");
        std::fs::create_dir_all(&dir).unwrap();
        let s = settings(&dir);
        let input = PermissionInput {
            tool_name: "shell".into(),
            detail: Some("rm -rf report.md".into()),
            write_path: Some(dir.join("x")),
            read_path: None,
        };
        assert_eq!(decide(&s, &input), Decision::Deny);
    }

    #[test]
    fn unknown_tool_outside_output_dir_asks() {
        let dir = std::env::temp_dir().join("agent_deck_test_out");
        std::fs::create_dir_all(&dir).unwrap();
        let s = settings(&dir);
        let input = PermissionInput {
            tool_name: "shell".into(),
            detail: Some("python script.py".into()),
            write_path: None,
            read_path: None,
        };
        assert_eq!(decide(&s, &input), Decision::Ask);
    }

    #[test]
    fn pattern_name_only_matches_regardless_of_detail() {
        let input = PermissionInput { tool_name: "write".into(), detail: Some("何でも".into()), write_path: None, read_path: None };
        assert!(tool_matches("write", &input));
        let input_no_detail = PermissionInput { tool_name: "write".into(), detail: None, write_path: None, read_path: None };
        assert!(tool_matches("write", &input_no_detail));
    }

    #[test]
    fn pattern_name_only_does_not_match_different_kind() {
        let input = PermissionInput { tool_name: "read".into(), detail: None, write_path: None, read_path: None };
        assert!(!tool_matches("write", &input));
    }

    #[test]
    fn shell_filter_without_wildcard_matches_first_token_exactly() {
        let input = PermissionInput { tool_name: "shell".into(), detail: Some("rm -rf /tmp".into()), write_path: None, read_path: None };
        assert!(tool_matches("shell(rm)", &input));

        let not_exact = PermissionInput { tool_name: "shell".into(), detail: Some("rmdir /tmp".into()), write_path: None, read_path: None };
        assert!(!tool_matches("shell(rm)", &not_exact), "先頭トークンの完全一致でなければマッチしない");
    }

    #[test]
    fn shell_wildcard_filter_matches_command_prefix() {
        let input = PermissionInput { tool_name: "shell".into(), detail: Some("python script.py".into()), write_path: None, read_path: None };
        assert!(tool_matches("shell(python:*)", &input));

        let other = PermissionInput { tool_name: "shell".into(), detail: Some("node x.js".into()), write_path: None, read_path: None };
        assert!(!tool_matches("shell(python:*)", &other));
    }

    #[test]
    fn wildcard_filter_is_rejected_for_non_shell_names() {
        // CLI 仕様上ワイルドカードは shell 専用。write(python:*) のような組み合わせは常に不一致。
        let input = PermissionInput { tool_name: "write".into(), detail: Some("python:* script.py".into()), write_path: None, read_path: None };
        assert!(!tool_matches("write(python:*)", &input));
    }

    #[test]
    fn different_tool_kind_never_matches_even_with_matching_filter_text() {
        let input = PermissionInput { tool_name: "read".into(), detail: Some("rm".into()), write_path: None, read_path: None };
        assert!(!tool_matches("shell(rm)", &input));
    }
}
