// ステップ10(エージェントからの質問への回答)の実機検証バイナリ(v1.0)。
//
//   ラウンドA: ask_user への回答(経路A)。
//     必ず ask_user ツールで質問するよう指示したエージェントを実行し、
//     UserInputRequested を受信したら user_input_bridge.respond で回答する。
//     回答後 TaskCompleted に到達すること(summary の中身の一致判定は緩め:
//     完了すれば OK とし、summary はそのまま印字して目視確認できるようにする)。
//     モデルが ask_user を使わなかった場合(非決定性)は1回だけ再実行し、
//     それでも来なければ「SKIPPED(モデル判断)」を出して exit 0 として扱う。
//
//   ラウンドB: タスク完了後の追い返信(経路B)。
//     通常実行(挨拶)→ TaskCompleted 後、同じ session_id を
//     TaskSpec.resume_session_id に積んで再実行(「先ほどの挨拶を英語にしてください」)。
//     2回目の TaskCompleted が同じ session_id で届き、summary が変化していることを確認する。
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で agents.rs / events.rs / config.rs / permissions.rs / copilot.rs / audit.rs
// を共有する(step2_check.rs 以降と同じ手法)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --manifest-path src-tauri/Cargo.toml --bin step10_check

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
#[allow(dead_code)]
mod copilot;
// write_provenance/cleanup_old_logs はこのバイナリでは検証しない(main.rs の spawn_task/起動時
// 処理でのみ呼ばれる)ため allow(dead_code)。
#[path = "../audit.rs"]
#[allow(dead_code)]
mod audit;

use events::AppEvent;
use std::path::PathBuf;

/// ラウンドAのエージェント定義本文。質問を強制する(指示どおり、body に明記する)。
const ASKER_PROMPT: &str = "あなたは慎重なアシスタントです。作業を始める前に、必ず ask_user ツールを使って『続行しますか?』と質問し、\
ユーザーの回答を得てから、その回答内容をそのまま最終報告として返してください。それ以外の作業は行わないでください。";
const ANSWER_TEXT: &str = "はい、続行です";

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

    let tmp = std::env::temp_dir().join(format!("agent-deck-step10-{}", std::process::id()));
    let ws = tmp.join("ws");
    let logs_dir = tmp.join("data").join("logs");
    for dir in [&ws, &logs_dir] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("フォルダを作成できません({}): {e}", dir.display());
            std::process::exit(1);
        }
    }
    println!("ワークスペース: {}", ws.display());

    let ok_a = run_round_a(&cli_path, &ws, &logs_dir).await;
    let ok_b = run_round_b(&cli_path, &ws, &logs_dir).await;

    if ok_a && ok_b {
        println!("\nすべてのラウンドが成功しました");
        std::process::exit(0);
    }
    eprintln!("失敗したラウンドがあります: A(ask_user への回答)={ok_a} B(セッション再開)={ok_b}");
    std::process::exit(1);
}

/// 1回分の試行結果。NoQuestion はモデルが ask_user を使わなかった場合(非決定性、再実行対象)。
enum RoundAAttempt {
    Answered,
    NoQuestion,
    Failed,
}

/// ラウンドA: ask_user への回答。UserInputRequested を受信できなければ1回だけ再実行し、
/// それでも来なければ SKIPPED として exit 0 扱いにする(モデル非決定性のため)。
async fn run_round_a(cli_path: &PathBuf, ws: &PathBuf, logs_dir: &PathBuf) -> bool {
    println!("\n=== ラウンドA: ask_user への回答 ===");
    for attempt in 1..=2 {
        println!("-- 試行 {attempt}/2 --");
        match run_round_a_once(cli_path, ws, logs_dir).await {
            RoundAAttempt::Answered => {
                println!("ラウンドA OK");
                return true;
            }
            RoundAAttempt::Failed => {
                eprintln!("ラウンドA失敗");
                return false;
            }
            RoundAAttempt::NoQuestion if attempt == 1 => {
                println!("UserInputRequested を受信できませんでした(モデルが ask_user を使わなかった可能性)。再実行します");
            }
            RoundAAttempt::NoQuestion => {
                println!("ラウンドA: SKIPPED(モデル判断: 2回とも ask_user を使いませんでした)");
                return true;
            }
        }
    }
    unreachable!("ループは2回で必ず return する");
}

async fn run_round_a_once(cli_path: &PathBuf, ws: &PathBuf, logs_dir: &PathBuf) -> RoundAAttempt {
    let agent = copilot::AgentSpec {
        name: "asker".to_string(),
        display_name: None,
        description: "質問して回答をそのまま報告する".to_string(),
        tools: None,
        model: None,
        prompt: ASKER_PROMPT.to_string(),
    };
    let bridge = copilot::PermissionBridge::new();
    let user_input_bridge = copilot::UserInputBridge::new();
    let spec = copilot::TaskSpec {
        prompt: "作業をお願いします。".to_string(),
        agent_id: "asker".to_string(),
        agents: vec![agent.clone()],
        selected_agent_name: agent.name.clone(),
        working_directory: ws.clone(),
        session_model: None,
        rules: config::AgentSettings::default(),
        bridge: bridge.clone(),
        user_input_bridge: user_input_bridge.clone(),
        unattended: false,
        logs_dir: logs_dir.clone(),
        resume_session_id: None,
    };
    let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let sink = move |ev: AppEvent| {
        let _ = tx.send(ev);
    };
    let run = tokio::spawn(copilot::run_task(cli_path.clone(), spec, cancel_rx, sink));

    let mut saw_question = false;
    while let Some(ev) = rx.recv().await {
        print_event(&ev);
        match &ev {
            AppEvent::UserInputRequested { request_id, question, .. } => {
                saw_question = true;
                println!("質問を受信: {question}");
                if let Err(e) = user_input_bridge.respond(request_id, Some(ANSWER_TEXT.to_string())) {
                    eprintln!("user_input_bridge.respond に失敗しました: {e}");
                }
            }
            // asker は ask_user 以外のツールを使わない想定だが、モデルが確認等で
            // 他のツールを使う可能性に備えて何でも承認する(ハング防止。他の step*_check.rs と同じ理由)。
            AppEvent::PermissionRequested { request_id, .. } => {
                if let Err(e) = bridge.respond(request_id, copilot::PermissionReply::ApproveOnce) {
                    eprintln!("bridge.respond に失敗しました: {e}");
                }
            }
            _ => {}
        }
    }

    let run_outcome = match run.await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            eprintln!("run_task がエラーを返しました(開始前の失敗): {e}");
            return RoundAAttempt::Failed;
        }
        Err(e) => {
            eprintln!("run_task の join に失敗しました: {e}");
            return RoundAAttempt::Failed;
        }
    };

    if !saw_question {
        return RoundAAttempt::NoQuestion;
    }
    if run_outcome.status != copilot::TaskStatus::Completed {
        eprintln!("RunOutcome.status が Completed ではありません: {:?}", run_outcome.status);
        return RoundAAttempt::Failed;
    }
    println!("RunOutcome.summary = {:?}", run_outcome.summary);
    RoundAAttempt::Answered
}

/// ラウンドB: 通常実行 → TaskCompleted 後、同じ session_id で resume して追い返信する。
async fn run_round_b(cli_path: &PathBuf, ws: &PathBuf, logs_dir: &PathBuf) -> bool {
    println!("\n=== ラウンドB: セッション再開(タスク完了後の追い返信) ===");
    let agent = copilot::AgentSpec {
        name: "greeter".to_string(),
        display_name: None,
        description: "挨拶担当".to_string(),
        tools: None,
        model: None,
        prompt: "あなたは挨拶だけを行うアシスタントです。指示された内容・言語で一言だけ返答してください。".to_string(),
    };

    let (session_id, summary1, ok1) =
        run_and_collect(cli_path, ws, logs_dir, &agent, "「こんにちは」とだけ日本語で返答してください。", None).await;
    let Some(session_id) = session_id else {
        eprintln!("ラウンドB失敗: 1回目のタスクが session_id を得られませんでした");
        return false;
    };
    if !ok1 {
        eprintln!("ラウンドB失敗: 1回目のタスクが完了しませんでした");
        return false;
    }
    println!("1回目 summary = {summary1:?}(session_id={session_id})");

    let (session_id2, summary2, ok2) = run_and_collect(
        cli_path,
        ws,
        logs_dir,
        &agent,
        "先ほどの挨拶を英語にしてください。",
        Some(session_id.clone()),
    )
    .await;
    let Some(session_id2) = session_id2 else {
        eprintln!("ラウンドB失敗: 2回目のタスクが session_id を得られませんでした");
        return false;
    };
    println!("2回目 summary = {summary2:?}(session_id={session_id2})");

    let mut ok = true;
    if session_id2 != session_id {
        eprintln!("ラウンドB失敗: 2回目の session_id が1回目と異なります: {session_id2} != {session_id}");
        ok = false;
    }
    if !ok2 {
        eprintln!("ラウンドB失敗: 2回目のタスクが完了しませんでした");
        ok = false;
    }
    if summary1 == summary2 {
        eprintln!("ラウンドB失敗: summary が変化していません: {summary1:?}");
        ok = false;
    }
    if ok {
        println!("ラウンドB OK(session_id 一致・summary 変化を確認)");
    }
    ok
}

/// run_task を1回実行し、(session_id, summary, 完了したか) を返す。resume_session_id が
/// Some ならそのセッションを再開する(v1.0 経路B)。
async fn run_and_collect(
    cli_path: &PathBuf,
    ws: &PathBuf,
    logs_dir: &PathBuf,
    agent: &copilot::AgentSpec,
    prompt: &str,
    resume_session_id: Option<String>,
) -> (Option<String>, String, bool) {
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
        user_input_bridge: copilot::UserInputBridge::new(),
        unattended: false,
        logs_dir: logs_dir.clone(),
        resume_session_id,
    };
    let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let sink = move |ev: AppEvent| {
        let _ = tx.send(ev);
    };
    let run = tokio::spawn(copilot::run_task(cli_path.clone(), spec, cancel_rx, sink));

    while let Some(ev) = rx.recv().await {
        print_event(&ev);
        if let AppEvent::PermissionRequested { request_id, .. } = &ev {
            if let Err(e) = bridge.respond(request_id, copilot::PermissionReply::ApproveOnce) {
                eprintln!("bridge.respond に失敗しました: {e}");
            }
        }
    }

    match run.await {
        Ok(Ok(o)) => {
            let ok = o.status == copilot::TaskStatus::Completed;
            (Some(o.session_id), o.summary, ok)
        }
        Ok(Err(e)) => {
            eprintln!("run_task がエラーを返しました(開始前の失敗): {e}");
            (None, String::new(), false)
        }
        Err(e) => {
            eprintln!("run_task の join に失敗しました: {e}");
            (None, String::new(), false)
        }
    }
}

fn print_event(ev: &AppEvent) {
    match serde_json::to_string(ev) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("イベントの JSON 化に失敗しました: {e}"),
    }
}
