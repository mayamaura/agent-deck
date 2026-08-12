// ステップ4(入出力設定と権限制御)の境界ケース実機検証バイナリ(docs/development.md §4)。
// 一時ディレクトリ構成: <tmp>/ws(作業ディレクトリ)、<tmp>/ws/out(outputDir)。
// エージェント writer(tools フィールドなし、本文は「あなたはファイル作成担当です。指示された
// パスに指示された内容のファイルを作成してください。余計なことはしないでください。」)を使い、
// 3 ラウンドで docs/architecture.md §7 の判定ロジックと受け入れ条件6・7を実機で確認する。
//
//   ラウンドA(自動承認): outputDir 配下への書き込み → PermissionRequested が 0 件で完了する
//   ラウンドB(Ask→拒否): outputDir 外への書き込み → Ask を bridge.respond(false) で拒否
//                         → TaskFailed(権限拒否)で終了し、ファイルは作成されない
//   ラウンドC(Ask→承認): B と同じ状況を bridge.respond(true) で承認 → TaskCompleted し、
//                         ファイルが作成される
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で events.rs / config.rs / permissions.rs / copilot.rs を共有する
// (main.rs の lib 化はこのステップのスコープ外。step2_check.rs / step3_check.rs と同じ手法)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --manifest-path src-tauri/Cargo.toml --bin step4_check

#[path = "../events.rs"]
mod events;
// config.rs のうち AgentSettings 以外(data_dir 等)はこのバイナリでは未使用になるため
// allow(dead_code)(step2_check.rs / step3_check.rs と同じ理由)。
#[path = "../config.rs"]
#[allow(dead_code)]
mod config;
#[path = "../permissions.rs"]
mod permissions;
// このバイナリは run_task の RunOutcome を素通し(Ok(Ok(_)))するだけで各フィールドは
// 読まないため allow(dead_code)(config と同じ理由。docs/development.md ステップ6で
// RunOutcome を追加した際に判明)。
#[path = "../copilot.rs"]
#[allow(dead_code)]
mod copilot;
// copilot.rs が use crate::audit(監査ログ。docs/roadmap.md v0.6)を要求するため共有する。
// このバイナリでは監査ログの中身までは検証しないため allow(dead_code)。
#[path = "../audit.rs"]
#[allow(dead_code)]
mod audit;

use config::AgentSettings;
use events::AppEvent;
use std::path::PathBuf;

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

    let tmp = std::env::temp_dir().join(format!("agent-deck-step4-{}", std::process::id()));
    let ws = tmp.join("ws");
    let out = ws.join("out");
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("作業フォルダを作成できません({}): {e}", out.display());
        std::process::exit(1);
    }
    println!("作業ディレクトリ: {}", ws.display());
    println!("出力フォルダ: {}", out.display());

    let agent = copilot::AgentSpec {
        name: "writer".to_string(),
        display_name: None,
        description: "ファイル作成担当".to_string(),
        tools: None,
        model: None,
        prompt: WRITER_PROMPT.to_string(),
    };
    let rules = AgentSettings {
        input_dir: None,
        output_dir: Some(out.clone()),
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        auto_approve_write_in_output_dir: true,
    };

    let ok_a = run_round_a(&cli_path, &ws, &out, agent.clone(), rules.clone()).await;
    let ok_b = run_round_b(&cli_path, &ws, agent.clone(), rules.clone()).await;
    let ok_c = run_round_c(&cli_path, &ws, agent, rules).await;

    if ok_a && ok_b && ok_c {
        println!("すべてのラウンドが成功しました");
        std::process::exit(0);
    }
    eprintln!("失敗したラウンドがあります: A(自動承認)={ok_a} B(拒否)={ok_b} C(承認)={ok_c}");
    std::process::exit(1);
}

/// ラウンド1回分の実行結果。
#[derive(Debug, Default)]
struct RoundOutcome {
    /// (permission_kind, detail) の列。
    permission_requests: Vec<(String, String)>,
    completed: bool,
    failed: bool,
    error: Option<String>,
    cancelled: bool,
}

/// run_task を実行し、受信イベントを標準エラーへ列挙しつつ結果を集計する。
/// PermissionRequested を受信するたびに `auto_approve` の可否で bridge.respond する
/// (Ask のまま応答しないと send_and_wait のタイムアウトまで停止してしまうため、
/// ラウンドAで想定外に Ask が来た場合もハングしないよう常に応答する)。
async fn run_task_and_collect(
    cli_path: &PathBuf,
    ws: &PathBuf,
    agent: copilot::AgentSpec,
    rules: AgentSettings,
    prompt: &str,
    auto_approve: bool,
) -> RoundOutcome {
    let bridge = copilot::PermissionBridge::new();
    let logs_dir = std::env::temp_dir().join(format!("agent-deck-step4-logs-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&logs_dir);
    let spec = copilot::TaskSpec {
        prompt: prompt.to_string(),
        agent_id: "writer".to_string(),
        agents: vec![agent.clone()],
        selected_agent_name: agent.name.clone(),
        working_directory: ws.clone(),
        session_model: None,
        rules,
        bridge: bridge.clone(),
        user_input_bridge: copilot::UserInputBridge::new(),
        unattended: false,
        logs_dir,
        resume_session_id: None,
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
                let reply =
                    if auto_approve { copilot::PermissionReply::ApproveOnce } else { copilot::PermissionReply::Deny };
                if let Err(e) = bridge.respond(request_id, reply) {
                    eprintln!("bridge.respond に失敗しました: {e}");
                }
            }
            AppEvent::TaskCompleted { .. } => outcome.completed = true,
            AppEvent::TaskFailed { error, .. } => {
                outcome.failed = true;
                outcome.error = Some(error.clone());
            }
            AppEvent::TaskCancelled { .. } => outcome.cancelled = true,
            _ => {}
        }
    }

    match run.await {
        // 拒否ラウンドも TaskStarted 後の終端は Ok(RunOutcome{status: Failed}) になる
        // (docs/development.md ステップ6: 開始後は Err を返さない設計に変更)。
        Ok(Ok(_)) => {}
        Ok(Err(e)) => eprintln!("run_task がエラーを返しました(開始前の失敗): {e}"),
        Err(e) => eprintln!("run_task の join に失敗しました: {e}"),
    }
    outcome
}

/// ラウンドA: outputDir 配下への書き込みは自動承認され、Ask が 1 件も出ないこと。
async fn run_round_a(cli_path: &PathBuf, ws: &PathBuf, out: &PathBuf, agent: copilot::AgentSpec, rules: AgentSettings) -> bool {
    println!("\n=== ラウンドA: 出力フォルダ配下 → 自動承認 ===");
    let target = out.join("hello.txt");
    let prompt = format!("{} に こんにちは と書いてください", target.display());
    let outcome = run_task_and_collect(cli_path, ws, agent, rules, &prompt, true).await;

    let mut ok = true;
    if !outcome.permission_requests.is_empty() {
        eprintln!(
            "ラウンドA失敗: PermissionRequested が {} 件 emit されました(0 件のはず): {:?}",
            outcome.permission_requests.len(),
            outcome.permission_requests
        );
        ok = false;
    }
    if !outcome.completed {
        eprintln!(
            "ラウンドA失敗: TaskCompleted を受信できませんでした(failed={} error={:?} cancelled={})",
            outcome.failed, outcome.error, outcome.cancelled
        );
        ok = false;
    }
    if !target.is_file() {
        eprintln!("ラウンドA失敗: {} が存在しません", target.display());
        ok = false;
    }
    if ok {
        println!("ラウンドA OK");
    }
    ok
}

/// ラウンドB: outputDir 外への書き込みは Ask になる。bridge.respond(false) で拒否すると
/// TaskFailed(権限拒否)で終了し、ファイルは作成されないこと(受け入れ条件7)。
async fn run_round_b(cli_path: &PathBuf, ws: &PathBuf, agent: copilot::AgentSpec, rules: AgentSettings) -> bool {
    println!("\n=== ラウンドB: 出力フォルダ外 → Ask → 拒否 ===");
    let target = ws.join("escape.txt");
    let prompt = format!("{} に こんにちは と書いてください", target.display());
    let outcome = run_task_and_collect(cli_path, ws, agent, rules, &prompt, false).await;

    let mut ok = true;
    if outcome.permission_requests.is_empty() {
        eprintln!("ラウンドB失敗: PermissionRequested を受信できませんでした");
        ok = false;
    }
    if !outcome.failed {
        eprintln!(
            "ラウンドB失敗: TaskFailed を受信できませんでした(completed={} cancelled={})",
            outcome.completed, outcome.cancelled
        );
        ok = false;
    }
    if target.is_file() {
        eprintln!("ラウンドB失敗: 拒否したのに {} が作成されました", target.display());
        ok = false;
    }
    if ok {
        println!("ラウンドB OK(error={:?})", outcome.error);
    }
    ok
}

/// ラウンドC: B と同じ状況で bridge.respond(true) を返すと TaskCompleted し、
/// ファイルが作成されること。
async fn run_round_c(cli_path: &PathBuf, ws: &PathBuf, agent: copilot::AgentSpec, rules: AgentSettings) -> bool {
    println!("\n=== ラウンドC: 出力フォルダ外 → Ask → 承認 ===");
    let target = ws.join("escape.txt");
    let prompt = format!("{} に こんにちは と書いてください", target.display());
    let outcome = run_task_and_collect(cli_path, ws, agent, rules, &prompt, true).await;

    let mut ok = true;
    if outcome.permission_requests.is_empty() {
        eprintln!("ラウンドC失敗: PermissionRequested を受信できませんでした");
        ok = false;
    }
    if !outcome.completed {
        eprintln!(
            "ラウンドC失敗: TaskCompleted を受信できませんでした(failed={} error={:?} cancelled={})",
            outcome.failed, outcome.error, outcome.cancelled
        );
        ok = false;
    }
    if !target.is_file() {
        eprintln!("ラウンドC失敗: 承認したのに {} が存在しません", target.display());
        ok = false;
    }
    if ok {
        println!("ラウンドC OK");
    }
    ok
}

fn print_event(ev: &AppEvent) {
    match serde_json::to_string(ev) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("イベントの JSON 化に失敗しました: {e}"),
    }
}
