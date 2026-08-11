// ステップ8(並行実行とダッシュボード)の実機検証バイナリ(docs/roadmap.md v0.5)。
// main.rs の AppState(HashMap<String, RunningTask> + config.json の maxConcurrentTasks)は
// Tauri の AppHandle に結びついているため、ここでは並行実行の核心である
// 「copilot::run_task を2本同時に実行してもイベント・session_id が混線しない」ことを、
// Tauri 抜きで直接検証する。
// 同一 outputDir の排他判定(output_dir_conflict)は純関数として main.rs 側の
// #[cfg(test)] でユニットテスト済み(実機検証はここでは行わない)。
//
//   一時 data ディレクトリに greeter-a / greeter-b の2定義(ツール不要の挨拶エージェント)を
//   用意し、tokio::join! で copilot::run_task を2本同時に await する
//   (maxConcurrentTasks=2 相当の状況の再現)。それぞれ別の作業ディレクトリ・別のチャネルで
//   イベントを収集し、以下を assert する:
//     - 両方 RunOutcome.status == Completed
//     - 2つの RunOutcome.session_id が異なる(空でもない)
//     - 各チャネルで受信した全イベントの session_id が、そのチャネル自身の
//       RunOutcome.session_id とだけ一致する(他方の session_id が混ざっていない)
//     - TaskStarted.agent_id がそれぞれ "greeter-a" / "greeter-b" と一致する
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で agents.rs / events.rs / config.rs / permissions.rs / copilot.rs を共有する
// (step2_check.rs 以降と同じ手法)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --manifest-path src-tauri/Cargo.toml --bin step8_check

#[path = "../agents.rs"]
#[allow(dead_code)]
mod agents;
#[path = "../events.rs"]
mod events;
#[path = "../config.rs"]
#[allow(dead_code)]
mod config;
#[path = "../permissions.rs"]
mod permissions;
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

    let tmp = std::env::temp_dir().join(format!("agent-deck-step8-{}", std::process::id()));
    let agent_dir = tmp.join("agents");
    if let Err(e) = std::fs::create_dir_all(&agent_dir) {
        eprintln!("フォルダを作成できません({}): {e}", agent_dir.display());
        std::process::exit(1);
    }
    for name in ["greeter-a", "greeter-b"] {
        let content = format!(
            "---\nname: {name}\ndescription: 挨拶担当\ntools: []\n---\nあなたは挨拶担当です。ツールを使わず、依頼に一言で答えてください。\n"
        );
        if let Err(e) = std::fs::write(agent_dir.join(format!("{name}.agent.md")), content) {
            eprintln!("{name}.agent.md を書き込めません: {e}");
            std::process::exit(1);
        }
    }
    let no_shared_dir = agent_dir.join("__no_shared__");
    let definitions = match agents::scan_definitions(&[agent_dir.clone()], &no_shared_dir) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("scan_definitions に失敗しました: {e}");
            std::process::exit(1);
        }
    };
    let spec_for = |id: &str| -> copilot::AgentSpec {
        let d = definitions
            .iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("定義が見つかりません: {id}"));
        copilot::AgentSpec {
            name: d.name.clone(),
            display_name: None,
            description: d.description.clone(),
            tools: d.tools.clone(),
            model: d.model.clone(),
            prompt: d.body.clone(),
        }
    };

    let ws_a = tmp.join("ws-a");
    let ws_b = tmp.join("ws-b");
    for dir in [&ws_a, &ws_b] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("作業フォルダを作成できません({}): {e}", dir.display());
            std::process::exit(1);
        }
    }

    println!("=== 2セッション並行実行(maxConcurrentTasks=2 相当。tokio::join! で同時 await) ===");
    let (result_a, result_b) = tokio::join!(
        run_and_collect(&cli_path, &ws_a, spec_for("greeter-a"), "こんにちはとだけ返してください"),
        run_and_collect(&cli_path, &ws_b, spec_for("greeter-b"), "やあとだけ返してください"),
    );

    if check_results("greeter-a", result_a, "greeter-b", result_b) {
        println!("\nすべてのラウンドが成功しました");
        std::process::exit(0);
    }
    eprintln!("失敗しました");
    std::process::exit(1);
}

struct RunResult {
    events: Vec<AppEvent>,
    run_outcome: Option<copilot::RunOutcome>,
}

/// run_task を1本実行し、受信イベントを標準出力へ列挙しつつ結果を集計する
/// (step6/7_check.rs と同じ手法)。greeter は tools:[] のため PermissionRequested は
/// 想定外だが、想定外に届いてもハングしないよう安全側(拒否)で応答する。
async fn run_and_collect(cli_path: &PathBuf, ws: &PathBuf, agent: copilot::AgentSpec, prompt: &str) -> RunResult {
    let bridge = copilot::PermissionBridge::new();
    let spec = copilot::TaskSpec {
        prompt: prompt.to_string(),
        agent_id: agent.name.clone(),
        agents: vec![agent.clone()],
        selected_agent_name: agent.name.clone(),
        working_directory: ws.clone(),
        session_model: None,
        rules: config::AgentSettings::default(),
        bridge: bridge.clone(),
        unattended: false,
    };
    let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let sink = move |ev: AppEvent| {
        let _ = tx.send(ev);
    };
    let run = tokio::spawn(copilot::run_task(cli_path.clone(), spec, cancel_rx, sink));

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        print_event(&agent.name, &ev);
        if let AppEvent::PermissionRequested { request_id, .. } = &ev {
            if let Err(e) = bridge.respond(request_id, false) {
                eprintln!("bridge.respond に失敗しました: {e}");
            }
        }
        events.push(ev);
    }

    let run_outcome = match run.await {
        Ok(Ok(o)) => Some(o),
        Ok(Err(e)) => {
            eprintln!("run_task がエラーを返しました(開始前の失敗): {e}");
            None
        }
        Err(e) => {
            eprintln!("run_task の join に失敗しました: {e}");
            None
        }
    };
    RunResult { events, run_outcome }
}

fn check_results(name_a: &str, a: RunResult, name_b: &str, b: RunResult) -> bool {
    let Some(outcome_a) = &a.run_outcome else {
        eprintln!("失敗: {name_a} の RunOutcome を取得できませんでした");
        return false;
    };
    let Some(outcome_b) = &b.run_outcome else {
        eprintln!("失敗: {name_b} の RunOutcome を取得できませんでした");
        return false;
    };

    let mut ok = true;
    if outcome_a.status != copilot::TaskStatus::Completed {
        eprintln!("失敗: {name_a} の status が Completed ではありません: {:?}", outcome_a.status);
        ok = false;
    }
    if outcome_b.status != copilot::TaskStatus::Completed {
        eprintln!("失敗: {name_b} の status が Completed ではありません: {:?}", outcome_b.status);
        ok = false;
    }
    if outcome_a.session_id.is_empty() || outcome_b.session_id.is_empty() {
        eprintln!("失敗: session_id が空です(a={:?} b={:?})", outcome_a.session_id, outcome_b.session_id);
        ok = false;
    }
    if outcome_a.session_id == outcome_b.session_id {
        eprintln!("失敗: 2セッションの session_id が同じです: {}", outcome_a.session_id);
        ok = false;
    }
    // 並行実行下でも RunOutcome の各フィールド(所要時間・開始時刻・トークン数・サブエージェント)が
    // 2セッション独立に埋まっていることの確認(混線していれば片方が0のまま、といった形で壊れうる)。
    for (label, outcome) in [(name_a, outcome_a), (name_b, outcome_b)] {
        if outcome.duration_ms == 0 {
            eprintln!("失敗: {label} の duration_ms が 0 です");
            ok = false;
        }
        if outcome.started_at.is_empty() {
            eprintln!("失敗: {label} の started_at が空です");
            ok = false;
        }
        println!(
            "{label}: summary={:?} total_tokens={:?} output_files={:?}",
            outcome.summary, outcome.total_tokens, outcome.output_files
        );
        for sub in &outcome.subagents {
            println!("{label} subagent: {} ({}ms)", sub.name, sub.duration_ms);
        }
    }

    if !events_belong_only_to(&a.events, &outcome_a.session_id, name_a) {
        ok = false;
    }
    if !events_belong_only_to(&b.events, &outcome_b.session_id, name_b) {
        ok = false;
    }
    if !task_started_agent_id_matches(&a.events, name_a) {
        ok = false;
    }
    if !task_started_agent_id_matches(&b.events, name_b) {
        ok = false;
    }

    if ok {
        println!("\nOK: session_a={} session_b={}", outcome_a.session_id, outcome_b.session_id);
    }
    ok
}

/// あるチャネルで受信した全イベントの session_id が own_session_id とだけ一致すること
/// (=イベントが混線していないこと)、かつ TaskCompleted を受信できたことを確認する。
fn events_belong_only_to(events: &[AppEvent], own_session_id: &str, label: &str) -> bool {
    let mut ok = true;
    let mut saw_completed = false;
    for ev in events {
        let sid = event_session_id(ev);
        if sid != own_session_id {
            eprintln!("失敗: {label} のチャネルに他セッションのイベントが混入しました(sid={sid}, own={own_session_id})");
            ok = false;
        }
        if matches!(ev, AppEvent::TaskCompleted { .. }) {
            saw_completed = true;
        }
    }
    if !saw_completed {
        eprintln!("失敗: {label} で TaskCompleted を受信できませんでした");
        ok = false;
    }
    ok
}

fn event_session_id(ev: &AppEvent) -> &str {
    match ev {
        AppEvent::TaskStarted { session_id, .. }
        | AppEvent::AgentIntent { session_id, .. }
        | AppEvent::SubagentStarted { session_id, .. }
        | AppEvent::SubagentCompleted { session_id, .. }
        | AppEvent::SubagentFailed { session_id, .. }
        | AppEvent::ToolStarted { session_id, .. }
        | AppEvent::ToolCompleted { session_id, .. }
        | AppEvent::PermissionRequested { session_id, .. }
        | AppEvent::UsageUpdated { session_id, .. }
        | AppEvent::TaskCompleted { session_id, .. }
        | AppEvent::TaskFailed { session_id, .. }
        | AppEvent::TaskCancelled { session_id } => session_id.as_str(),
    }
}

fn task_started_agent_id_matches(events: &[AppEvent], expected_agent_id: &str) -> bool {
    for ev in events {
        if let AppEvent::TaskStarted { agent_id, .. } = ev {
            if agent_id != expected_agent_id {
                eprintln!("失敗: TaskStarted.agent_id が一致しません: expected={expected_agent_id} actual={agent_id}");
                return false;
            }
            return true;
        }
    }
    eprintln!("失敗: TaskStarted を受信できませんでした(expected agent_id={expected_agent_id})");
    false
}

fn print_event(label: &str, ev: &AppEvent) {
    match serde_json::to_string(ev) {
        Ok(json) => println!("[{label}] {json}"),
        Err(e) => eprintln!("イベントの JSON 化に失敗しました: {e}"),
    }
}
