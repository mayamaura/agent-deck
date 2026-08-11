// ステップ3(エージェント定義の読み込みと実行)のヘッドレス動作確認バイナリ(docs/development.md §4)。
// 一時フォルダに greeter.agent.md を作り、
//   ラウンド1: scan_definitions で読める(tools が空配列)ことと、TaskStarted.agent_id ==
//              "greeter" / TaskCompleted を受信して通常完了することを確認する。
//   ラウンド2: TaskStarted 受信の 3 秒後に cancel_task 相当(cancel チャネル送信)を発火し、
//              Session::abort() 経由で TaskCancelled を受信できることを確認する
//              (TaskCompleted で終わった場合は失敗として exit 1)。
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で agents.rs / copilot.rs / events.rs を共有する(main.rs の lib 化はこのステップのスコープ外)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --bin step3_check

#[path = "../agents.rs"]
// このバイナリでは scan_definitions / AgentDefinition だけを使う。list_agents 用の
// scan / AgentSummary はここでは未使用になるため allow(dead_code)
// (events.rs の EVENT_CHANNEL と同じ理由 — 本体の main.rs では両方とも使用済み)。
#[allow(dead_code)]
mod agents;
#[path = "../events.rs"]
mod events;
// copilot.rs が use crate::permissions(内部で crate::config::AgentSettings を使う)を
// 要求するため、このバイナリでも両方を共有する必要がある。config.rs のうち
// AgentSettings 以外(data_dir 等)はこのバイナリでは未使用になるため allow(dead_code)。
#[path = "../config.rs"]
#[allow(dead_code)]
mod config;
#[path = "../permissions.rs"]
mod permissions;
// このバイナリは権限確認フローを試験しないため PermissionBridge::respond は未使用
// (config と同じ理由で allow)。
#[path = "../copilot.rs"]
#[allow(dead_code)]
mod copilot;

use events::AppEvent;
use std::path::PathBuf;
use std::time::Duration;

const GREETER_MD: &str = "---\nname: greeter\ndescription: 挨拶担当\ntools: []\n---\nあなたは挨拶担当です。ツールを使わず、依頼に一言で答えてください。\n";

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

    let agent_dir = std::env::temp_dir().join(format!("agent-deck-step3-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&agent_dir) {
        eprintln!("一時フォルダを作成できません: {e}");
        std::process::exit(1);
    }
    if let Err(e) = std::fs::write(agent_dir.join("greeter.agent.md"), GREETER_MD) {
        eprintln!("greeter.agent.md を書き込めません: {e}");
        std::process::exit(1);
    }

    let definitions = match agents::scan_definitions(&[agent_dir.clone()]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("scan_definitions に失敗しました: {e}");
            std::process::exit(1);
        }
    };
    let Some(greeter) = definitions.iter().find(|d| d.id == "greeter") else {
        eprintln!("greeter 定義が見つかりません: {definitions:?}");
        std::process::exit(1);
    };
    if greeter.tools != Some(Vec::new()) {
        eprintln!("tools が空配列になっていません: {:?}", greeter.tools);
        std::process::exit(1);
    }
    println!("scan_definitions OK: id={} name={} tools={:?}", greeter.id, greeter.name, greeter.tools);

    let agent_specs = vec![copilot::AgentSpec {
        name: greeter.name.clone(),
        display_name: None,
        description: greeter.description.clone(),
        tools: greeter.tools.clone(),
        model: greeter.model.clone(),
        prompt: greeter.body.clone(),
    }];

    if !run_round1(&cli_path, greeter, &agent_specs).await {
        std::process::exit(1);
    }
    if !run_round2(cli_path, greeter, agent_specs).await {
        std::process::exit(1);
    }

    std::process::exit(0);
}

/// ラウンド1: 通常完了(TaskStarted.agent_id == "greeter" / TaskCompleted を受信)。
async fn run_round1(cli_path: &PathBuf, greeter: &agents::AgentDefinition, agent_specs: &[copilot::AgentSpec]) -> bool {
    println!("=== ラウンド1: 通常完了 ===");
    let spec = copilot::TaskSpec {
        prompt: "こんにちはとだけ返してください".to_string(),
        agent_id: "greeter".to_string(),
        agents: agent_specs.to_vec(),
        selected_agent_name: greeter.name.clone(),
        working_directory: greeter.source_path.parent().unwrap_or(&greeter.source_path).to_path_buf(),
        session_model: None,
        rules: config::AgentSettings::default(),
        bridge: copilot::PermissionBridge::new(),
    };
    let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let sink = move |ev: AppEvent| {
        let _ = tx.send(ev);
    };
    let run = tokio::spawn(copilot::run_task(cli_path.clone(), spec, cancel_rx, sink));

    let mut saw_started_with_correct_agent_id = false;
    let mut saw_completed = false;
    while let Some(ev) = rx.recv().await {
        print_event(&ev);
        match &ev {
            AppEvent::TaskStarted { agent_id, .. } => saw_started_with_correct_agent_id = agent_id == "greeter",
            AppEvent::TaskCompleted { .. } => saw_completed = true,
            _ => {}
        }
    }
    match run.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("run_task が失敗を返しました: {e}");
            return false;
        }
        Err(e) => {
            eprintln!("run_task の join に失敗しました: {e}");
            return false;
        }
    }
    if !saw_started_with_correct_agent_id || !saw_completed {
        eprintln!(
            "ラウンド1失敗: TaskStarted.agent_id==\"greeter\"={saw_started_with_correct_agent_id} TaskCompleted受信={saw_completed}"
        );
        return false;
    }
    println!("ラウンド1 OK");
    true
}

/// ラウンド2: TaskStarted 受信の 3 秒後に中断(cancel チャネル送信 → Session::abort())。
/// TaskCancelled を受信できれば OK。TaskCompleted で終わった場合は中断が機能していないので失敗。
async fn run_round2(cli_path: PathBuf, greeter: &agents::AgentDefinition, agent_specs: Vec<copilot::AgentSpec>) -> bool {
    println!("=== ラウンド2: 中断 ===");
    let spec = copilot::TaskSpec {
        prompt: "1から100までゆっくり数えて".to_string(),
        agent_id: "greeter".to_string(),
        agents: agent_specs,
        selected_agent_name: greeter.name.clone(),
        working_directory: greeter.source_path.parent().unwrap_or(&greeter.source_path).to_path_buf(),
        session_model: None,
        rules: config::AgentSettings::default(),
        bridge: copilot::PermissionBridge::new(),
    };
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let sink = move |ev: AppEvent| {
        let _ = tx.send(ev);
    };
    let run = tokio::spawn(copilot::run_task(cli_path, spec, cancel_rx, sink));

    let mut cancel_tx = Some(cancel_tx);
    // TaskStarted 受信までは発火させない、遠い未来にセットしておいて受信時に 3 秒へリセットする。
    let timer = tokio::time::sleep(Duration::from_secs(24 * 60 * 60));
    tokio::pin!(timer);
    let mut timer_armed = false;
    let mut saw_cancelled = false;
    let mut saw_completed = false;

    loop {
        tokio::select! {
            ev = rx.recv() => {
                match ev {
                    Some(ev) => {
                        print_event(&ev);
                        match &ev {
                            AppEvent::TaskStarted { .. } => {
                                timer.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(3));
                                timer_armed = true;
                            }
                            AppEvent::TaskCancelled { .. } => saw_cancelled = true,
                            AppEvent::TaskCompleted { .. } => saw_completed = true,
                            _ => {}
                        }
                    }
                    None => break,
                }
            }
            _ = &mut timer, if timer_armed && cancel_tx.is_some() => {
                if let Some(tx) = cancel_tx.take() {
                    println!("(TaskStarted から3秒経過、中断を発火)");
                    let _ = tx.send(());
                }
            }
        }
    }
    match run.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("run_task が失敗を返しました: {e}");
            return false;
        }
        Err(e) => {
            eprintln!("run_task の join に失敗しました: {e}");
            return false;
        }
    }

    if !saw_cancelled {
        eprintln!("ラウンド2失敗: TaskCancelled を受信できませんでした(TaskCompleted受信={saw_completed})");
        return false;
    }
    if saw_completed {
        eprintln!("ラウンド2失敗: TaskCancelled と TaskCompleted の両方を受信しました(排他のはず)");
        return false;
    }
    println!("ラウンド2 OK");
    true
}

fn print_event(ev: &AppEvent) {
    match serde_json::to_string(ev) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("イベントの JSON 化に失敗しました: {e}"),
    }
}
