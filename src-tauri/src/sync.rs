// 共有エージェント定義の同期(docs/roadmap.md v0.2、決定ログ 2026-08-12: 共有フォルダ方式のみ実装)。
// source フォルダ直下の *.agent.md を data/shared-agents/ へ全置換でコピーし、
// マニフェスト(data/shared-agents.meta.json)にファイルごとの sha256/size を記録する。

use crate::agents::sha256_hex;
use crate::copilot::format_rfc3339_now;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestFileEntry {
    pub sha256: String,
    pub size: u64,
}

/// data/shared-agents.meta.json(docs/roadmap.md v0.2 の決定ログ)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: u32,
    pub synced_at: String,
    pub source_path: String,
    pub files: BTreeMap<String, ManifestFileEntry>,
}

/// sync_shared_agents の結果(UI の同期結果表示に使う)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub synced_at: String,
}

/// マニフェストを読む。存在しない場合は None(初回同期前はまだファイルが無いため、
/// エラーにはしない)。壊れている場合はエラー(docs/development.md §3: 握りつぶさない)。
pub fn load_manifest(meta_path: &Path) -> Result<Option<Manifest>, String> {
    if !meta_path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(meta_path).map_err(|e| format!("{} を読めません: {e}", meta_path.display()))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{} の形式が不正です: {e}", meta_path.display()))
}

fn save_manifest(meta_path: &Path, manifest: &Manifest) -> Result<(), String> {
    let json = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    fs::write(meta_path, json).map_err(|e| format!("{} に保存できません: {e}", meta_path.display()))
}

/// source フォルダ直下の *.agent.md を shared_dir へ全置換で同期する
/// (source に無いファイルは shared_dir から削除する)。source が存在しなければエラー。
pub fn sync_shared_agents(source: &Path, shared_dir: &Path, meta_path: &Path) -> Result<SyncSummary, String> {
    if !source.is_dir() {
        return Err(format!("共有元フォルダがありません: {}", source.display()));
    }

    let old_files = load_manifest(meta_path)?.map(|m| m.files).unwrap_or_default();

    fs::create_dir_all(shared_dir)
        .map_err(|e| format!("{} を作成できません: {e}", shared_dir.display()))?;

    // source 側を読み、ハッシュを計算する。
    let mut new_files: BTreeMap<String, ManifestFileEntry> = BTreeMap::new();
    let mut contents: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for entry in fs::read_dir(source).map_err(|e| format!("{} を走査できません: {e}", source.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !file_name.ends_with(".agent.md") {
            continue;
        }
        let bytes = fs::read(&path).map_err(|e| format!("{} を読めません: {e}", path.display()))?;
        let hash = sha256_hex(&bytes);
        new_files.insert(file_name.to_string(), ManifestFileEntry { sha256: hash, size: bytes.len() as u64 });
        contents.insert(file_name.to_string(), bytes);
    }

    // 全置換: shared_dir 側で source に無くなった *.agent.md を削除する。
    let mut removed = 0usize;
    for entry in fs::read_dir(shared_dir).map_err(|e| format!("{} を走査できません: {e}", shared_dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !file_name.ends_with(".agent.md") {
            continue;
        }
        if !new_files.contains_key(file_name) {
            fs::remove_file(&path).map_err(|e| format!("{} を削除できません: {e}", path.display()))?;
            removed += 1;
        }
    }

    // source 側の内容を shared_dir へ書き出し、追加/更新を数える。
    let mut added = 0usize;
    let mut updated = 0usize;
    for (file_name, bytes) in &contents {
        let dest = shared_dir.join(file_name);
        match old_files.get(file_name) {
            None => added += 1,
            Some(old) if old.sha256 != new_files[file_name].sha256 => updated += 1,
            Some(_) => {}
        }
        fs::write(&dest, bytes).map_err(|e| format!("{} に書き込めません: {e}", dest.display()))?;
    }

    let synced_at = format_rfc3339_now();
    save_manifest(
        meta_path,
        &Manifest {
            version: 1,
            synced_at: synced_at.clone(),
            source_path: source.display().to_string(),
            files: new_files,
        },
    )?;

    Ok(SyncSummary { added, updated, removed, synced_at })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("agent_deck_test_sync_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_source_is_an_error() {
        let base = temp_dir("missing_source");
        let source = base.join("does-not-exist");
        let shared = base.join("shared");
        let meta = base.join("meta.json");
        assert!(sync_shared_agents(&source, &shared, &meta).is_err());
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn add_update_remove_reflected_and_manifest_hash_matches_content() {
        let base = temp_dir("add_update_remove");
        let source = base.join("source");
        let shared = base.join("shared");
        let meta = base.join("meta.json");
        fs::create_dir_all(&source).unwrap();

        // 1回目: a, b を追加。
        fs::write(source.join("a.agent.md"), "---\nname: a\n---\nbody-a").unwrap();
        fs::write(source.join("b.agent.md"), "---\nname: b\n---\nbody-b").unwrap();
        let summary1 = sync_shared_agents(&source, &shared, &meta).unwrap();
        assert_eq!((summary1.added, summary1.updated, summary1.removed), (2, 0, 0));
        assert!(shared.join("a.agent.md").exists());
        assert!(shared.join("b.agent.md").exists());

        // マニフェストのハッシュが実内容と一致することを確認する。
        let manifest = load_manifest(&meta).unwrap().expect("manifest should exist after sync");
        let a_bytes = fs::read(source.join("a.agent.md")).unwrap();
        assert_eq!(manifest.files["a.agent.md"].sha256, sha256_hex(&a_bytes));
        assert_eq!(manifest.files["a.agent.md"].size, a_bytes.len() as u64);

        // 2回目: a を更新、b を削除、c を追加。
        fs::write(source.join("a.agent.md"), "---\nname: a\n---\nbody-a-changed").unwrap();
        fs::remove_file(source.join("b.agent.md")).unwrap();
        fs::write(source.join("c.agent.md"), "---\nname: c\n---\nbody-c").unwrap();
        let summary2 = sync_shared_agents(&source, &shared, &meta).unwrap();
        assert_eq!((summary2.added, summary2.updated, summary2.removed), (1, 1, 1));
        assert!(!shared.join("b.agent.md").exists());
        assert_eq!(fs::read_to_string(shared.join("a.agent.md")).unwrap(), "---\nname: a\n---\nbody-a-changed");
        assert!(shared.join("c.agent.md").exists());

        // 3回目: 変更なしなら added/updated/removed は全て0。
        let summary3 = sync_shared_agents(&source, &shared, &meta).unwrap();
        assert_eq!((summary3.added, summary3.updated, summary3.removed), (0, 0, 0));

        fs::remove_dir_all(&base).ok();
    }
}
