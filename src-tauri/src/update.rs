// アプリ本体の更新検知(docs/roadmap.md v0.3、段階案 v0.3.0: 検知と通知のみ・適用はしない)。
// 配布元は「共有フォルダ + マニフェスト」方式のみ実装する(GitHub Releases は配布元が
// 未決のため実装しない。docs/roadmap.md v0.3 未決事項)。

use crate::agents::sha256_hex;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// 配布フォルダ直下の manifest.json(docs/roadmap.md v0.3 設計)。
#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    version: String,
    file: String,
    sha256: String,
    notes: String,
}

/// check_updates の戻り値。file_path はハッシュ検証対象のフルパス(UI には出さない。
/// 「配布フォルダを開く」は updateSource そのものを開くため main.rs 側では不要)。
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: String,
    // v0.3.0 の UI(main.rs UpdateInfoDto)では使わない(「配布フォルダを開く」は
    // updateSource そのものを開くため)。v0.3.x の半自動適用(roadmap.md)で
    // 検証済みファイルへの参照として使う想定のため残す。
    #[allow(dead_code)]
    pub file_path: PathBuf,
    pub hash_ok: bool,
}

/// update_source/manifest.json を読み、現行バージョンより新しければ Some を返す。
/// 配布ファイルの sha256 をここで照合し hash_ok に反映する(ファイル欠落やハッシュ不一致は
/// hash_ok=false として結果に含める。UI 側で警告表示する)。
/// マニフェスト自体が読めない/壊れている場合はエラー(握りつぶさない。docs/development.md §3)。
pub fn check_updates(update_source: &Path, current_version: &str) -> Result<Option<UpdateInfo>, String> {
    let manifest_path = update_source.join("manifest.json");
    if !manifest_path.exists() {
        return Err(format!("マニフェストが見つかりません: {}", manifest_path.display()));
    }
    let text = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{} を読めません: {e}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&text)
        .map_err(|e| format!("{} の形式が不正です: {e}", manifest_path.display()))?;

    if !is_newer(&manifest.version, current_version)? {
        return Ok(None);
    }

    let file_path = update_source.join(&manifest.file);
    let hash_ok = fs::read(&file_path)
        .map(|bytes| sha256_hex(&bytes) == manifest.sha256)
        .unwrap_or(false);

    Ok(Some(UpdateInfo { version: manifest.version, notes: manifest.notes, file_path, hash_ok }))
}

/// "x.y.z" の数値比較(セマンティックバージョン)。プレリリース表記(-beta 等)は非対応
/// (ponytail: 必要になったら semver クレートではなく `-` 以降を切り捨てる程度で足りるはず)。
fn is_newer(candidate: &str, current: &str) -> Result<bool, String> {
    Ok(parse_version(candidate)? > parse_version(current)?)
}

fn parse_version(v: &str) -> Result<(u32, u32, u32), String> {
    let parts: Vec<&str> = v.split('.').collect();
    let [maj, min, patch] = parts[..] else {
        return Err(format!("バージョン形式が不正です(x.y.z 形式のみ対応): {v}"));
    };
    let parse = |s: &str| s.parse::<u32>().map_err(|_| format!("バージョン形式が不正です(x.y.z 形式のみ対応): {v}"));
    Ok((parse(maj)?, parse(min)?, parse(patch)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_true_for_higher_version() {
        assert!(is_newer("0.4.0", "0.3.0").unwrap());
        assert!(is_newer("1.0.0", "0.9.9").unwrap());
    }

    #[test]
    fn is_newer_false_for_same_version() {
        assert!(!is_newer("0.3.0", "0.3.0").unwrap());
    }

    #[test]
    fn is_newer_false_for_older_version() {
        assert!(!is_newer("0.2.0", "0.3.0").unwrap());
    }

    #[test]
    fn is_newer_rejects_malformed_versions() {
        assert!(is_newer("0.4", "0.3.0").is_err());
        assert!(is_newer("0.4.0-beta", "0.3.0").is_err());
        assert!(is_newer("0.3.0", "not-a-version").is_err());
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent_deck_test_update_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_manifest(dir: &Path, version: &str, file: &str, sha256: &str, notes: &str) {
        let json = format!(r#"{{"version":"{version}","file":"{file}","sha256":"{sha256}","notes":"{notes}"}}"#);
        fs::write(dir.join("manifest.json"), json).unwrap();
    }

    /// 新しいバージョン + ハッシュ一致 → Some(hash_ok=true)。
    #[test]
    fn newer_version_with_matching_hash_is_reported_ok() {
        let dir = temp_dir("newer_ok");
        let bytes = b"dummy exe contents";
        fs::write(dir.join("agent-deck-0.4.0.exe"), bytes).unwrap();
        write_manifest(&dir, "0.4.0", "agent-deck-0.4.0.exe", &sha256_hex(bytes), "変更点の要約");
        let info = check_updates(&dir, "0.3.0").unwrap().expect("update expected");
        assert_eq!(info.version, "0.4.0");
        assert_eq!(info.notes, "変更点の要約");
        assert!(info.hash_ok);
        fs::remove_dir_all(&dir).ok();
    }

    /// 同じバージョン → None。
    #[test]
    fn same_version_returns_none() {
        let dir = temp_dir("same");
        write_manifest(&dir, "0.3.0", "agent-deck-0.3.0.exe", "deadbeef", "");
        assert!(check_updates(&dir, "0.3.0").unwrap().is_none());
        fs::remove_dir_all(&dir).ok();
    }

    /// 古いバージョン(配布フォルダにロールバック用の旧版が残っている等)→ None。
    #[test]
    fn older_version_returns_none() {
        let dir = temp_dir("older");
        write_manifest(&dir, "0.2.0", "agent-deck-0.2.0.exe", "deadbeef", "");
        assert!(check_updates(&dir, "0.3.0").unwrap().is_none());
        fs::remove_dir_all(&dir).ok();
    }

    /// マニフェストのバージョン形式が不正 → エラー(握りつぶさない)。
    #[test]
    fn malformed_manifest_version_is_an_error() {
        let dir = temp_dir("malformed");
        write_manifest(&dir, "not-a-version", "agent-deck.exe", "deadbeef", "");
        assert!(check_updates(&dir, "0.3.0").is_err());
        fs::remove_dir_all(&dir).ok();
    }

    /// ハッシュ不一致 → エラーにはせず hash_ok=false で返す(UI が警告を出す)。
    #[test]
    fn hash_mismatch_is_reported_not_ok() {
        let dir = temp_dir("hash_mismatch");
        fs::write(dir.join("agent-deck-0.4.0.exe"), b"dummy contents").unwrap();
        write_manifest(&dir, "0.4.0", "agent-deck-0.4.0.exe", &"0".repeat(64), "");
        let info = check_updates(&dir, "0.3.0").unwrap().unwrap();
        assert!(!info.hash_ok);
        fs::remove_dir_all(&dir).ok();
    }

    /// 配布ファイル自体が無い → エラーにはせず hash_ok=false で返す。
    #[test]
    fn missing_distribution_file_is_reported_not_ok() {
        let dir = temp_dir("missing_file");
        write_manifest(&dir, "0.4.0", "does-not-exist.exe", "deadbeef", "");
        let info = check_updates(&dir, "0.3.0").unwrap().unwrap();
        assert!(!info.hash_ok);
        fs::remove_dir_all(&dir).ok();
    }

    /// manifest.json 自体が無い → エラー。
    #[test]
    fn missing_manifest_is_an_error() {
        let dir = temp_dir("missing_manifest");
        assert!(check_updates(&dir, "0.3.0").is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
