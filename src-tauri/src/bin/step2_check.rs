// ステップ2(イベント可視化)のヘッドレス動作確認バイナリ(docs/development.md §4)。
// COPILOT_CLI_PATH から CLI を解決し、固定プロンプトで run_task を実行、受信した
// AppEvent を 1 行 1 JSON で標準出力に印字する。TaskStarted と TaskCompleted の両方を
// 受信したら exit 0、それ以外(タイムアウト・失敗・中断など)は exit 1。
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で copilot.rs / events.rs を共有する(main.rs の lib 化はこのステップのスコープ外)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --bin step2_check

#[path = "../events.rs"]
mod events;
#[path = "../copilot.rs"]
mod copilot;

use events::AppEvent;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let configured = match std::env::var("COPILOT_CLI_PATH") {
        Ok(v) => PathBuf::from(v),
        Err(_) => {
            eprintln!("環境変数 COPILOT_CLI_PATH が未設定です");
            std::process::exit(1);
        }
    };
    let cli_path = match copilot::resolve_cli_path(Some(&configured)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // このバイナリでは中断しないため、cancel の送信側は使わず保持だけする。
    let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let sink = move |ev: AppEvent| {
        let _ = tx.send(ev);
    };

    // ステップ2はイベント可視化の確認が目的でカスタムエージェントの検証はステップ3の
    // step3_check.rs 側で行うため、ここでは最小構成のダミーエージェント1件だけを渡す。
    let spec = copilot::TaskSpec {
        prompt: "「疎通OK」とだけ返答してください".to_string(),
        agent_id: "default".to_string(),
        agents: vec![copilot::AgentSpec {
            name: "default".to_string(),
            display_name: None,
            description: "疎通確認用".to_string(),
            tools: None,
            model: None,
            prompt: "あなたは疎通確認用のアシスタントです。".to_string(),
        }],
        selected_agent_name: "default".to_string(),
        working_directory: std::env::temp_dir(),
        session_model: None,
    };
    let run = tokio::spawn(copilot::run_task(cli_path, spec, cancel_rx, sink));

    let mut saw_started = false;
    let mut saw_completed = false;
    while let Some(ev) = rx.recv().await {
        match serde_json::to_string(&ev) {
            Ok(json) => println!("{json}"),
            Err(e) => eprintln!("イベントの JSON 化に失敗しました: {e}"),
        }
        match &ev {
            AppEvent::TaskStarted { .. } => saw_started = true,
            AppEvent::TaskCompleted { .. } => saw_completed = true,
            _ => {}
        }
    }

    match run.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("run_task が失敗を返しました: {e}"),
        Err(e) => eprintln!("run_task の join に失敗しました: {e}"),
    }

    if saw_started && saw_completed {
        std::process::exit(0);
    }
    eprintln!("TaskStarted={saw_started} TaskCompleted={saw_completed}");
    std::process::exit(1);
}
