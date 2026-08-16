// v1.1(b) エージェント定義の下書き生成の実機検証バイナリ。
//
//   ラウンドA: 業務の説明から下書きを生成し、
//     - name / description / body が空でない
//     - tools が既知エイリアスだけ(または未指定)に濾されている
//     - body にこのアプリの前提(集計はスクリプトに書かせる)が入っている
//     を確認する。生成物はそのまま印字して目視できるようにする。
//
//   ラウンドB: ツールを封じてあること(tools: [])の確認。
//     「カレントフォルダのファイル一覧を読んでから作れ」と促しても、ツールは実行されない。
//     実機ではモデルが「一覧を貼ってほしい」と聞き返して JSON を返さない
//     (= 下書きにはならないが、権限モデルの外にも出ない)。これは想定内の失敗であり、
//     エラー文に対処法(説明だけで依頼する)が載ることを目視するためのラウンド。exit 0 のまま。
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で共有する(step2_check.rs 以降と同じ手法)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --manifest-path src-tauri/Cargo.toml --bin step11_check

#[path = "../agents.rs"]
#[allow(dead_code)]
mod agents;
#[path = "../events.rs"]
#[allow(dead_code)]
mod events;
#[path = "../config.rs"]
#[allow(dead_code)]
mod config;
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

const KNOWN_TOOLS: [&str; 7] = ["execute", "read", "edit", "search", "agent", "web", "todo"];

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
    let workdir = std::env::temp_dir().join("agent_deck_step11");
    std::fs::create_dir_all(&workdir).expect("作業フォルダを作成できません");

    println!("=== ラウンドA: 業務の説明から下書きを作る ===");
    let drafted = match copilot::draft_agent(
        cli_path.clone(),
        None,
        workdir.clone(),
        "部署別のアンケート結果 CSV を集計して、部署ごとの傾向をまとめたレポートを作りたい".to_string(),
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAILED: 下書きを生成できません: {e}");
            std::process::exit(1);
        }
    };
    println!("name: {}", drafted.name);
    println!("description: {}", drafted.description);
    println!("tools: {:?}", drafted.tools);
    println!("body:\n{}\n", drafted.body);

    let mut failed = false;
    if drafted.name.trim().is_empty() || drafted.description.trim().is_empty() || drafted.body.trim().is_empty() {
        eprintln!("FAILED: name / description / body のいずれかが空です");
        failed = true;
    }
    if let Some(tools) = &drafted.tools {
        let unknown: Vec<&String> = tools.iter().filter(|t| !KNOWN_TOOLS.contains(&t.as_str())).collect();
        if !unknown.is_empty() {
            eprintln!("FAILED: 既知エイリアス以外が残っています: {unknown:?}");
            failed = true;
        }
    }
    // 数値集計をスクリプトにやらせる方針(docs/architecture.md §8.1)が本文に入っているか。
    // 表現はモデル任せなので語の一致は緩く見る。
    if !drafted.body.contains("スクリプト") {
        eprintln!("WARN: 本文に集計スクリプトの方針が見当たりません(メタプロンプトの効きを目視確認してください)");
    }
    println!("{}", if failed { "ラウンドA: FAILED" } else { "ラウンドA: OK" });

    println!("\n=== ラウンドB: ツールを封じてあること ===");
    match copilot::draft_agent(
        cli_path,
        None,
        workdir,
        "まずカレントフォルダのファイル一覧を読み、その内容に合わせた集計エージェントを作ってください".to_string(),
    )
    .await
    {
        Ok(d) => println!("ツール実行なしで下書きが返りました: name={} tools={:?}", d.name, d.tools),
        Err(e) => println!("下書きは返りませんでした(モデルがツール不可を理由に断った可能性): {e}"),
    }

    if failed {
        std::process::exit(1);
    }
}
