// ステップ6(履歴)の実機検証バイナリ(docs/development.md §4)。
// 一時ディレクトリ構成: <tmp>/ws(作業ディレクトリ)、<tmp>/ws/out(outputDir)、
// <tmp>/data(history.jsonl 用の一時 data ディレクトリ。exe 横の data は使わない)。
//
//   ラウンドA(自動承認 + 履歴): writer エージェントで out/hello.txt を書かせ、
//     copilot::run_task の RunOutcome(status/output_files/duration_ms/total_tokens)と、
//     TaskCompleted イベント自体の output_files を検証したのち、
//     history::entry_from_outcome → history::append → history::list で読み戻して
//     status=="completed" 等を確認する。
//   ラウンドB(中断 + 履歴): TaskStarted から3秒後に cancel し、RunOutcome.status ==
//     Cancelled と、履歴に "cancelled" で追記されることを確認する。
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で events.rs / config.rs / permissions.rs / copilot.rs / history.rs を共有する
// (step2_check.rs 以降と同じ手法)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --manifest-path src-tauri/Cargo.toml --bin step6_check

#[path = "../events.rs"]
mod events;
#[path = "../config.rs"]
#[allow(dead_code)]
mod config;
#[path = "../permissions.rs"]
mod permissions;
#[path = "../copilot.rs"]
mod copilot;
#[path = "../history.rs"]
mod history;

use events::AppEvent;
use std::path::PathBuf;
use std::time::Duration;

const WRITER_PROMPT: &str = "あなたはファイル作成担当です。指示されたパスに指示された内容のファイルを作成してください。余計なことはしないでください。";

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

    let tmp = std::env::temp_dir().join(format!("agent-deck-step6-{}", std::process::id()));
    let ws = tmp.join("ws");
    let out = ws.join("out");
    let temp_data = tmp.join("data");
    for dir in [&out, &temp_data] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("フォルダを作成できません({}): {e}", dir.display());
            std::process::exit(1);
        }
    }
    println!("作業ディレクトリ: {}", ws.display());
    println!("出力フォルダ: {}", out.display());
    println!("一時 data ディレクトリ: {}", temp_data.display());

    let ok_a = run_round_a(&cli_path, &ws, &out, &temp_data).await;
    let ok_b = run_round_b(cli_path, ws, temp_data).await;

    if ok_a && ok_b {
        println!("\nすべてのラウンドが成功しました");
        std::process::exit(0);
    }
    eprintln!("失敗したラウンドがあります: A(自動承認+履歴)={ok_a} B(中断+履歴)={ok_b}");
    std::process::exit(1);
}

/// ラウンド1回分の実行結果(AppEvent チャネル経由で集計したもの + run_task の RunOutcome)。
#[derive(Default)]
struct RoundOutcome {
    permission_requests: Vec<(String, String)>,
    completed: bool,
    failed: bool,
    error: Option<String>,
    task_completed_output_files: Vec<String>,
    run_outcome: Option<copilot::RunOutcome>,
}

/// run_task を実行し、受信イベントを標準エラーへ列挙しつつ結果を集計する。
/// 発生した PermissionRequested は常に承認する(ラウンドAは自動承認のみを想定しているが、
/// 想定外に Ask が来てもハングしないよう安全側で応答する。step4_check.rs と同じ理由)。
async fn run_task_and_collect(
    cli_path: &PathBuf,
    ws: &PathBuf,
    agent: copilot::AgentSpec,
    rules: config::AgentSettings,
    prompt: &str,
) -> RoundOutcome {
    let bridge = copilot::PermissionBridge::new();
    let spec = copilot::TaskSpec {
        prompt: prompt.to_string(),
        agent_id: agent.name.clone(),
        agents: vec![agent.clone()],
        selected_agent_name: agent.name.clone(),
        working_directory: ws.clone(),
        session_model: None,
        rules,
        bridge: bridge.clone(),
        unattended: false,
    };
    let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let sink = move |ev: AppEvent| {
        let _ = tx.send(ev);
    };
    let run = tokio::spawn(copilot::run_task(cli_path.clone(), spec, cancel_rx, sink));

    let mut outcome = RoundOutcome::default();
    while let Some(ev) = rx.recv().await {
        print_event(&ev);
        match &ev {
            AppEvent::PermissionRequested { request_id, permission_kind, detail, .. } => {
                outcome.permission_requests.push((permission_kind.clone(), detail.clone()));
                if let Err(e) = bridge.respond(request_id, true) {
                    eprintln!("bridge.respond に失敗しました: {e}");
                }
            }
            AppEvent::TaskCompleted { output_files, .. } => {
                outcome.completed = true;
                outcome.task_completed_output_files = output_files.clone();
            }
            AppEvent::TaskFailed { error, .. } => {
                outcome.failed = true;
                outcome.error = Some(error.clone());
            }
            _ => {}
        }
    }

    match run.await {
        Ok(Ok(run_outcome)) => outcome.run_outcome = Some(run_outcome),
        Ok(Err(e)) => eprintln!("run_task がエラーを返しました(開始前の失敗): {e}"),
        Err(e) => eprintln!("run_task の join に失敗しました: {e}"),
    }
    outcome
}

fn contains_hello_txt(files: &[String]) -> bool {
    files.iter().any(|f| f.to_lowercase().contains("hello.txt"))
}

/// ラウンドA: 出力フォルダ配下への自動承認書き込み → RunOutcome の各フィールドと、
/// history::entry_from_outcome / append / list による読み戻しを検証する。
async fn run_round_a(cli_path: &PathBuf, ws: &PathBuf, out: &PathBuf, temp_data: &PathBuf) -> bool {
    println!("\n=== ラウンドA: 自動承認 + 履歴書き込み ===");
    let agent = copilot::AgentSpec {
        name: "writer".to_string(),
        display_name: None,
        description: "ファイル作成担当".to_string(),
        tools: None,
        model: None,
        prompt: WRITER_PROMPT.to_string(),
    };
    let rules = config::AgentSettings {
        input_dir: None,
        output_dir: Some(out.clone()),
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        auto_approve_write_in_output_dir: true,
    };
    let target = out.join("hello.txt");
    let prompt = format!("{} に こんにちは と書いてください", target.display());
    let outcome = run_task_and_collect(cli_path, ws, agent, rules, &prompt).await;

    let Some(run_outcome) = &outcome.run_outcome else {
        eprintln!(
            "ラウンドA失敗: RunOutcome を取得できませんでした(completed={} failed={} error={:?})",
            outcome.completed, outcome.failed, outcome.error
        );
        return false;
    };

    let mut ok = true;
    if run_outcome.status != copilot::TaskStatus::Completed {
        eprintln!("ラウンドA失敗: RunOutcome.status が Completed ではありません: {:?}", run_outcome.status);
        ok = false;
    }
    if !contains_hello_txt(&run_outcome.output_files) {
        eprintln!("ラウンドA失敗: RunOutcome.output_files に hello.txt がありません: {:?}", run_outcome.output_files);
        ok = false;
    }
    if !contains_hello_txt(&outcome.task_completed_output_files) {
        eprintln!(
            "ラウンドA失敗: TaskCompleted.output_files に hello.txt がありません: {:?}",
            outcome.task_completed_output_files
        );
        ok = false;
    }
    if run_outcome.duration_ms == 0 {
        eprintln!("ラウンドA失敗: RunOutcome.duration_ms が 0 です");
        ok = false;
    }
    if run_outcome.total_tokens.is_none() {
        eprintln!("ラウンドA失敗: RunOutcome.total_tokens が None です");
        ok = false;
    }
    if !target.is_file() {
        eprintln!("ラウンドA失敗: {} が存在しません", target.display());
        ok = false;
    }
    if !ok {
        return false;
    }

    let entry = history::entry_from_outcome("writer", &prompt, run_outcome, "manual");
    if let Err(e) = history::append(temp_data, &entry) {
        eprintln!("ラウンドA失敗: history::append に失敗しました: {e}");
        return false;
    }
    let list = match history::list(temp_data, 10) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ラウンドA失敗: history::list に失敗しました: {e}");
            return false;
        }
    };
    let Some(first) = list.first() else {
        eprintln!("ラウンドA失敗: history::list が空です");
        return false;
    };
    if first.status != "completed" {
        eprintln!("ラウンドA失敗: 履歴の status が completed ではありません: {}", first.status);
        ok = false;
    }
    if !contains_hello_txt(&first.output_files) {
        eprintln!("ラウンドA失敗: 履歴の outputFiles に hello.txt がありません: {:?}", first.output_files);
        ok = false;
    }
    if first.duration_ms == 0 {
        eprintln!("ラウンドA失敗: 履歴の durationMs が 0 です");
        ok = false;
    }
    if first.total_tokens.is_none() {
        eprintln!("ラウンドA失敗: 履歴の totalTokens が None です");
        ok = false;
    }

    if ok {
        println!("ラウンドA OK: summary={:?}", run_outcome.summary);
        println!("ラウンドA OK: history={}", serde_json::to_string(first).unwrap_or_default());
    }
    ok
}

/// ラウンドB: TaskStarted から3秒後に中断 → RunOutcome.status == Cancelled と、
/// 履歴に "cancelled" で追記されることを確認する(step3_check.rs のラウンド2と同じ中断手順)。
async fn run_round_b(cli_path: PathBuf, ws: PathBuf, temp_data: PathBuf) -> bool {
    println!("\n=== ラウンドB: 3秒で中断 + 履歴書き込み ===");
    let agent = copilot::AgentSpec {
        name: "counter".to_string(),
        display_name: None,
        description: "数を数える担当".to_string(),
        tools: Some(Vec::new()),
        model: None,
        prompt: "あなたは数を数える担当です。依頼された範囲の数字を一つずつ、ゆっくり時間をかけて数えてください。".to_string(),
    };
    let prompt = "1から100までゆっくり数えてください".to_string();
    let spec = copilot::TaskSpec {
        prompt: prompt.clone(),
        agent_id: "counter".to_string(),
        agents: vec![agent.clone()],
        selected_agent_name: agent.name.clone(),
        working_directory: ws,
        session_model: None,
        rules: config::AgentSettings::default(),
        bridge: copilot::PermissionBridge::new(),
        unattended: false,
    };
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let sink = move |ev: AppEvent| {
        let _ = tx.send(ev);
    };
    let run = tokio::spawn(copilot::run_task(cli_path, spec, cancel_rx, sink));

    let mut cancel_tx = Some(cancel_tx);
    // TaskStarted 受信までは発火させない、遠い未来にセットしておいて受信時に 3 秒へリセットする
    // (step3_check.rs のラウンド2と同じ手法)。
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

    let run_outcome = match run.await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            eprintln!("ラウンドB失敗: run_task がエラーを返しました(開始前の失敗): {e}");
            return false;
        }
        Err(e) => {
            eprintln!("ラウンドB失敗: run_task の join に失敗しました: {e}");
            return false;
        }
    };

    let mut ok = true;
    if !saw_cancelled {
        eprintln!("ラウンドB失敗: TaskCancelled を受信できませんでした(TaskCompleted受信={saw_completed})");
        ok = false;
    }
    if run_outcome.status != copilot::TaskStatus::Cancelled {
        eprintln!("ラウンドB失敗: RunOutcome.status が Cancelled ではありません: {:?}", run_outcome.status);
        ok = false;
    }
    if !ok {
        return false;
    }

    let entry = history::entry_from_outcome("counter", &prompt, &run_outcome, "manual");
    if let Err(e) = history::append(&temp_data, &entry) {
        eprintln!("ラウンドB失敗: history::append に失敗しました: {e}");
        return false;
    }
    let list = match history::list(&temp_data, 10) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ラウンドB失敗: history::list に失敗しました: {e}");
            return false;
        }
    };
    let Some(first) = list.first() else {
        eprintln!("ラウンドB失敗: history::list が空です");
        return false;
    };
    if first.status != "cancelled" {
        eprintln!("ラウンドB失敗: 履歴の status が cancelled ではありません: {}", first.status);
        ok = false;
    }

    if ok {
        println!("ラウンドB OK: summary={:?}", run_outcome.summary);
        println!("ラウンドB OK: history={}", serde_json::to_string(first).unwrap_or_default());
    }
    ok
}

fn print_event(ev: &AppEvent) {
    match serde_json::to_string(ev) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("イベントの JSON 化に失敗しました: {e}"),
    }
}
