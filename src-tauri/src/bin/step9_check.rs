// ステップ9(監査とガバナンス)の実機検証バイナリ(docs/roadmap.md v0.6)。
//
//   ラウンドA: writer エージェント + 自動承認 write(step6_check.rs ラウンドA相当の構成)で
//              1 タスクを実行し、以下を assert する:
//    - data/logs/session-<id>.jsonl が存在し、taskStarted / permission(autoApproved の
//      write) / taskCompleted の行を含む
//    - provenance-<id>.json が存在し、outputFiles に書いたファイル・agentVersion が
//      8 桁 hex・appVersion が入っている(provenance の組み立ては main.rs の spawn_task と
//      同じ手順をこのバイナリ側で再現する。main.rs は Tauri AppHandle に結びついており
//      ヘッドレスに #[path] 共有できないため、step2_check.rs 以降と同じ理由で再現する)
//
//   ラウンドB: data/policy.json に forcedDeniedTools: ["shell(rm)"] を置いた状態で
//              config::load_policy → config::merge_forced_denied_tools の呼び出しを確認する
//              (実行はしない。マージ関数の呼び出し確認のみでよい、という指示のとおり)。
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で agents.rs / events.rs / config.rs / permissions.rs / copilot.rs / audit.rs
// を共有する(step2_check.rs 以降と同じ手法)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --manifest-path src-tauri/Cargo.toml --bin step9_check

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
// このバイナリは承認ダイアログの3択(PermissionReply)のうち ApproveOnce しか使わないため
// allow(dead_code)(docs/architecture.md §7.1 拡張。config/audit と同じ理由)。
#[path = "../copilot.rs"]
#[allow(dead_code)]
mod copilot;
// cleanup_old_logs はこのバイナリでは検証しない(main.rs の起動時処理でのみ呼ばれる)ため
// allow(dead_code)。
#[path = "../audit.rs"]
#[allow(dead_code)]
mod audit;

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

    let tmp = std::env::temp_dir().join(format!("agent-deck-step9-{}", std::process::id()));
    let ws = tmp.join("ws");
    let out = ws.join("out");
    let data_dir = tmp.join("data");
    let logs_dir = data_dir.join("logs");
    for dir in [&out, &logs_dir] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("フォルダを作成できません({}): {e}", dir.display());
            std::process::exit(1);
        }
    }
    println!("data ディレクトリ: {}", data_dir.display());

    let ok_a = run_round_a(&cli_path, &ws, &out, &logs_dir).await;
    let ok_b = run_round_b(&data_dir);

    if ok_a && ok_b {
        println!("\nすべてのラウンドが成功しました");
        std::process::exit(0);
    }
    eprintln!("失敗したラウンドがあります: A(監査ログ+来歴)={ok_a} B(管理者ポリシーのマージ)={ok_b}");
    std::process::exit(1);
}

/// ラウンドA: 自動承認 write を1回実行し、監査ログと来歴を検証する。
async fn run_round_a(cli_path: &PathBuf, ws: &PathBuf, out: &PathBuf, logs_dir: &PathBuf) -> bool {
    println!("\n=== ラウンドA: 監査ログ(session-*.jsonl)+ 来歴(provenance-*.json) ===");
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
        work_dir: None,
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        auto_approve_write_in_output_dir: true,
    };
    let target = out.join("hello.txt");
    let prompt = format!("{} に こんにちは と書いてください", target.display());

    let bridge = copilot::PermissionBridge::new();
    let spec = copilot::TaskSpec {
        prompt: prompt.clone(),
        agent_id: "writer".to_string(),
        agents: vec![agent.clone()],
        selected_agent_name: agent.name.clone(),
        working_directory: ws.clone(),
        session_model: None,
        rules,
        bridge: bridge.clone(),
        user_input_bridge: copilot::UserInputBridge::new(),
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

    while let Some(ev) = rx.recv().await {
        print_event(&ev);
        if let AppEvent::PermissionRequested { request_id, .. } = &ev {
            // outputDir 配下への write 自体は自動承認されるが、モデルが書き込み前に
            // 出力フォルダの一覧(read)を確認する等、write 以外の Ask が挟まることがある
            // (docs/architecture.md §7.1: 自動承認の対象は write のみ。step6_check.rs の
            // ラウンドAも同様に許容している)。常に承認して応答しないと send_and_wait が
            // タイムアウトまでハングするため、種別を問わず承認する。
            if let Err(e) = bridge.respond(request_id, copilot::PermissionReply::ApproveOnce) {
                eprintln!("bridge.respond に失敗しました: {e}");
            }
        }
    }

    let run_outcome = match run.await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            eprintln!("ラウンドA失敗: run_task がエラーを返しました(開始前の失敗): {e}");
            return false;
        }
        Err(e) => {
            eprintln!("ラウンドA失敗: run_task の join に失敗しました: {e}");
            return false;
        }
    };
    if run_outcome.status != copilot::TaskStatus::Completed {
        eprintln!("ラウンドA失敗: RunOutcome.status が Completed ではありません: {:?}", run_outcome.status);
        return false;
    }
    println!(
        "RunOutcome: summary={:?} total_tokens={:?} subagents={}",
        run_outcome.summary,
        run_outcome.total_tokens,
        run_outcome.subagents.len()
    );
    for sub in &run_outcome.subagents {
        println!("subagent: {} ({}ms)", sub.name, sub.duration_ms);
    }

    let mut ok = true;

    // 監査ログ(data/logs/session-<id>.jsonl)の検証。
    let session_log_path = logs_dir.join(format!("session-{}.jsonl", run_outcome.session_id));
    let session_log = match std::fs::read_to_string(&session_log_path) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("ラウンドA失敗: {} を読めません: {e}", session_log_path.display());
            return false;
        }
    };
    if !session_log.contains("\"taskStarted\"") {
        eprintln!("ラウンドA失敗: 監査ログに taskStarted の行がありません");
        ok = false;
    }
    if !session_log.contains("\"autoApproved\"") {
        eprintln!("ラウンドA失敗: 監査ログに permission(autoApproved)の行がありません");
        ok = false;
    }
    if !session_log.contains("\"taskCompleted\"") {
        eprintln!("ラウンドA失敗: 監査ログに taskCompleted の行がありません");
        ok = false;
    }
    println!("監査ログ OK: {}", session_log_path.display());

    // 来歴(data/logs/provenance-<id>.json)の組み立て。main.rs の spawn_task の
    // Ok(outcome) 分岐と同じ手順をここで再現する(writer は個人スコープ相当の定義なので
    // agents::sha256_hex を直接使う。main.rs の agent_version_for_provenance の
    // Personal 分岐と同じ計算)。
    let agent_version = agents::sha256_hex(WRITER_PROMPT.as_bytes())[..8].to_string();
    let prov = audit::Provenance {
        session_id: run_outcome.session_id.clone(),
        agent_id: "writer".to_string(),
        agent_name: agent.name.clone(),
        agent_version,
        agent_source_path: "(step9_check: インラインエージェント定義)".to_string(),
        model: "(SDK 既定)".to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        prompt,
        started_at: run_outcome.started_at.clone(),
        duration_ms: run_outcome.duration_ms,
        status: run_outcome.status.as_str().to_string(),
        input_files: run_outcome.input_files.clone(),
        output_files: run_outcome.output_files.clone(),
    };
    if let Err(e) = audit::write_provenance(logs_dir, &prov) {
        eprintln!("ラウンドA失敗: write_provenance に失敗しました: {e}");
        return false;
    }

    let provenance_path = logs_dir.join(format!("provenance-{}.json", run_outcome.session_id));
    let provenance_text = match std::fs::read_to_string(&provenance_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ラウンドA失敗: {} を読めません: {e}", provenance_path.display());
            return false;
        }
    };
    let parsed: serde_json::Value = match serde_json::from_str(&provenance_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ラウンドA失敗: provenance の JSON が壊れています: {e}");
            return false;
        }
    };
    let output_files = parsed["outputFiles"].as_array().cloned().unwrap_or_default();
    if !output_files.iter().any(|f| f.as_str().unwrap_or_default().to_lowercase().contains("hello.txt")) {
        eprintln!("ラウンドA失敗: provenance.outputFiles に hello.txt がありません: {output_files:?}");
        ok = false;
    }
    let agent_version_field = parsed["agentVersion"].as_str().unwrap_or_default();
    let is_8_hex = agent_version_field.len() == 8 && agent_version_field.chars().all(|c| c.is_ascii_hexdigit());
    if !is_8_hex {
        eprintln!("ラウンドA失敗: provenance.agentVersion が 8 桁 hex ではありません: {agent_version_field:?}");
        ok = false;
    }
    let app_version_field = parsed["appVersion"].as_str().unwrap_or_default();
    if app_version_field.is_empty() {
        eprintln!("ラウンドA失敗: provenance.appVersion が空です");
        ok = false;
    }
    if !target.is_file() {
        eprintln!("ラウンドA失敗: {} が存在しません", target.display());
        ok = false;
    }

    if ok {
        println!("来歴 OK: {}", provenance_path.display());
        println!("ラウンドA OK");
    }
    ok
}

/// ラウンドB: policy.json の forcedDeniedTools が config::merge_forced_denied_tools で
/// agents.json 側の denied_tools にマージされることを確認する(実行はしない)。
fn run_round_b(data_dir: &PathBuf) -> bool {
    println!("\n=== ラウンドB: 管理者ポリシー(policy.json)のマージ ===");
    let policy_json = r#"{"version":1,"forcedDeniedTools":["shell(rm)"]}"#;
    if let Err(e) = std::fs::write(data_dir.join("policy.json"), policy_json) {
        eprintln!("ラウンドB失敗: policy.json を書き込めません: {e}");
        return false;
    }

    let policy = match config::load_policy(data_dir) {
        Ok(Some(p)) => p,
        Ok(None) => {
            eprintln!("ラウンドB失敗: load_policy が None を返しました(policy.json を書いたはず)");
            return false;
        }
        Err(e) => {
            eprintln!("ラウンドB失敗: load_policy がエラーを返しました: {e}");
            return false;
        }
    };

    let existing_denied = vec!["custom-tool".to_string()];
    let merged = config::merge_forced_denied_tools(existing_denied.clone(), &policy.forced_denied_tools);

    let mut ok = true;
    if !merged.contains(&"shell(rm)".to_string()) {
        eprintln!("ラウンドB失敗: マージ結果に shell(rm) が含まれません: {merged:?}");
        ok = false;
    }
    if !merged.contains(&"custom-tool".to_string()) {
        eprintln!("ラウンドB失敗: マージ結果が既存の denied_tools を保持していません: {merged:?}");
        ok = false;
    }
    if ok {
        println!("ラウンドB OK: merged denied_tools = {merged:?}");
    }
    ok
}

fn print_event(ev: &AppEvent) {
    match serde_json::to_string(ev) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("イベントの JSON 化に失敗しました: {e}"),
    }
}
