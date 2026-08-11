// ステップ5(ツリー表示)の実機観測バイナリ(docs/development.md §4)。
// 目的: サブエージェントへの委任が実際に発生するプロンプトを実行し、
// SubagentStarted.agent_id と、その後のサブエージェント由来イベント(agentId が
// Some のもの)の agentId が一致する(=ツリーの行キーとして機能する)ことを確認する。
// 委任するかどうかはモデル判断(docs/sdk-notes.md「カスタムエージェント」節: 自動委任)
// なので、発生しなくても本バイナリの失敗にはしない(観測結果として報告する)。
// ハード条件は TaskCompleted を受信すること(通常完了)のみ。
//
// 一時ディレクトリに coordinator / helper の 2 定義を置き、coordinator を選択して
// 「helper に短い挨拶文を作ってもらい、その結果をそのまま報告してください」を実行する。
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で agents.rs / copilot.rs / events.rs / config.rs / permissions.rs を共有する
// (step2_check.rs 以降と同じ手法)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --manifest-path src-tauri/Cargo.toml --bin step5_check

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

use events::AppEvent;
use std::path::PathBuf;

const COORDINATOR_MD: &str = "---\nname: coordinator\ndescription: 調整役。文章作成の依頼は必ず helper エージェントに委任し、結果をまとめて報告する\n---\nあなたは調整役です。自分で文章を書かず、必ず helper に委任すること。helper の結果をまとめて報告してください。\n";
const HELPER_MD: &str = "---\nname: helper\ndescription: 短い挨拶文の作成担当\n---\nあなたは短い挨拶文の作成担当です。依頼された挨拶文を1〜2文で作成してください。\n";

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

    let agent_dir = std::env::temp_dir().join(format!("agent-deck-step5-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&agent_dir) {
        eprintln!("一時フォルダを作成できません: {e}");
        std::process::exit(1);
    }
    if let Err(e) = std::fs::write(agent_dir.join("coordinator.agent.md"), COORDINATOR_MD) {
        eprintln!("coordinator.agent.md を書き込めません: {e}");
        std::process::exit(1);
    }
    if let Err(e) = std::fs::write(agent_dir.join("helper.agent.md"), HELPER_MD) {
        eprintln!("helper.agent.md を書き込めません: {e}");
        std::process::exit(1);
    }

    let definitions = match agents::scan_definitions(&[agent_dir.clone()]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("scan_definitions に失敗しました: {e}");
            std::process::exit(1);
        }
    };
    let Some(coordinator) = definitions.iter().find(|d| d.id == "coordinator") else {
        eprintln!("coordinator 定義が見つかりません: {definitions:?}");
        std::process::exit(1);
    };
    if !definitions.iter().any(|d| d.id == "helper") {
        eprintln!("helper 定義が見つかりません: {definitions:?}");
        std::process::exit(1);
    }
    println!("scan_definitions OK: {} 件", definitions.len());

    // 選択外(helper)も委任候補としてセッションに渡す(main.rs の start_task と同じ運用。
    // docs/sdk-notes.md「カスタムエージェント」節: サブエージェント委任は自動)。
    let agent_specs: Vec<copilot::AgentSpec> = definitions
        .iter()
        .map(|d| copilot::AgentSpec {
            name: d.name.clone(),
            display_name: None,
            description: d.description.clone(),
            tools: d.tools.clone(),
            model: d.model.clone(),
            prompt: d.body.clone(),
        })
        .collect();

    let spec = copilot::TaskSpec {
        prompt: "helper に短い挨拶文を作ってもらい、その結果をそのまま報告してください".to_string(),
        agent_id: "coordinator".to_string(),
        agents: agent_specs,
        selected_agent_name: coordinator.name.clone(),
        working_directory: agent_dir,
        session_model: None,
        rules: config::AgentSettings::default(),
        bridge: copilot::PermissionBridge::new(),
    };

    let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let sink = move |ev: AppEvent| {
        let _ = tx.send(ev);
    };
    let run = tokio::spawn(copilot::run_task(cli_path, spec, cancel_rx, sink));

    let mut log: Vec<AppEvent> = Vec::new();
    let mut saw_completed = false;
    let mut saw_failed = false;
    let mut saw_cancelled = false;
    while let Some(ev) = rx.recv().await {
        print_event(&ev);
        match &ev {
            AppEvent::TaskCompleted { .. } => saw_completed = true,
            AppEvent::TaskFailed { .. } => saw_failed = true,
            AppEvent::TaskCancelled { .. } => saw_cancelled = true,
            _ => {}
        }
        log.push(ev);
    }

    match run.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("run_task がエラーを返しました: {e}"),
        Err(e) => eprintln!("run_task の join に失敗しました: {e}"),
    }

    print_delegation_summary(&log);

    if saw_completed {
        println!("\nTaskCompleted を受信しました(ハード条件クリア)");
        std::process::exit(0);
    }
    eprintln!("\nTaskCompleted を受信できませんでした(failed={saw_failed} cancelled={saw_cancelled})");
    std::process::exit(1);
}

fn print_event(ev: &AppEvent) {
    match serde_json::to_string(ev) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("イベントの JSON 化に失敗しました: {e}"),
    }
}

/// AppEvent からエンベロープ相当の agentId を取り出す(観測専用。tree.ts と同じルール)。
fn event_agent_id(ev: &AppEvent) -> Option<String> {
    match ev {
        AppEvent::AgentIntent { agent_id, .. } => agent_id.clone(),
        AppEvent::ToolStarted { agent_id, .. } => agent_id.clone(),
        AppEvent::ToolCompleted { agent_id, .. } => agent_id.clone(),
        AppEvent::SubagentStarted { agent_id, .. } => Some(agent_id.clone()),
        AppEvent::SubagentCompleted { agent_id, .. } => Some(agent_id.clone()),
        AppEvent::SubagentFailed { agent_id, .. } => Some(agent_id.clone()),
        _ => None,
    }
}

/// SubagentStarted を観測したら、その agent_id が後続のサブエージェント由来イベント
/// (同じ tool_call_id の SubagentCompleted/Failed までの区間)でも一貫して使われて
/// いるかを要約する(docs/development.md ステップ5「サブエージェント相関の確定」)。
fn print_delegation_summary(log: &[AppEvent]) {
    let started_indices: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, ev)| matches!(ev, AppEvent::SubagentStarted { .. }))
        .map(|(i, _)| i)
        .collect();

    if started_indices.is_empty() {
        println!("\nDELEGATION OBSERVED: no(モデルが委任しなかった可能性があります。委任はモデル判断のため再実行を検討してください)");
        return;
    }
    println!("\nDELEGATION OBSERVED: yes({} 件)", started_indices.len());

    for &i in &started_indices {
        let AppEvent::SubagentStarted { agent_id, tool_call_id, display_name, .. } = &log[i] else {
            unreachable!()
        };
        // 同じ tool_call_id を持つ SubagentCompleted/Failed を終端として探す。
        let end = log
            .iter()
            .enumerate()
            .skip(i + 1)
            .find(|(_, ev)| match ev {
                AppEvent::SubagentCompleted { tool_call_id: tid, .. } => tid == tool_call_id,
                AppEvent::SubagentFailed { tool_call_id: tid, .. } => tid == tool_call_id,
                _ => false,
            })
            .map(|(j, _)| j)
            .unwrap_or(log.len().saturating_sub(1));

        let mut matched = 0usize;
        let mut mismatched: Vec<String> = Vec::new();
        for ev in &log[(i + 1)..=end.max(i)] {
            if let Some(other_id) = event_agent_id(ev) {
                if &other_id == agent_id {
                    matched += 1;
                } else {
                    mismatched.push(other_id);
                }
            }
        }
        println!(
            "CORRELATION: subagent={display_name} agent_id={agent_id} tool_call_id={tool_call_id} \
             同一agent_idの後続イベント={matched}件 不一致={mismatched:?}"
        );
    }
}
