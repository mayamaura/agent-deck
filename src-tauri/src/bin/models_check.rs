// モデル一覧取得(定義エディタのモデル選択欄)の実機検証バイナリ。
//
// 確認すること:
//   - copilot::list_models が実際に選択肢を返す(SDK の models.list が動く)
//   - 契約プラン名が取れる(取れなければ None のまま = モデル選択は続行できる)
//   - id が `.agent.md` の model に書ける形(空でない)で、倍率も読める
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で共有する(step2_check.rs 以降と同じ手法)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --manifest-path src-tauri/Cargo.toml --bin models_check

#[path = "../config.rs"]
#[allow(dead_code)]
mod config;
#[path = "../events.rs"]
#[allow(dead_code)]
mod events;
#[path = "../permissions.rs"]
#[allow(dead_code)]
mod permissions;
#[path = "../copilot.rs"]
#[allow(dead_code)]
mod copilot;
#[path = "../audit.rs"]
#[allow(dead_code)]
mod audit;

use std::path::PathBuf;

#[tokio::main]
async fn main() {
    // COPILOT_CLI_PATH が無ければ PATH から解決する(開発機での実行を楽にするため)。
    let configured = std::env::var("COPILOT_CLI_PATH").ok().map(PathBuf::from);
    let cli_path = match copilot::resolve_cli_path(configured.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("CLI を解決できません: {e}");
            std::process::exit(1);
        }
    };

    let catalog = match copilot::list_models(cli_path).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("モデル一覧を取得できません: {e}");
            std::process::exit(1);
        }
    };

    println!("契約プラン: {}", catalog.plan.as_deref().unwrap_or("(取得できず)"));
    println!("選択肢 {} 件:", catalog.models.len());
    for m in &catalog.models {
        println!("  {} / {} / 倍率 {:?}", m.id, m.name, m.multiplier);
        assert!(!m.id.is_empty(), "id が空のモデルがある");
        assert!(!m.name.is_empty(), "name が空のモデルがある");
    }
    assert!(!catalog.models.is_empty(), "選択肢が 0 件では選ばせられない");
    println!("OK");
}
