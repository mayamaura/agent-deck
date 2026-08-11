// ステップ7(定期実行と通知)の実機検証バイナリ(docs/roadmap.md v0.4)。
// main.rs のスケジューラ(scheduler_tick/spawn_task)は tauri::AppHandle/AppState に
// 結びついているため、ここではそのコア(is_due → 無人実行 → 履歴記録)を
// Tauri 抜きで直接再現する(main.rs 側と同じ呼び出し順序: schedule::load →
// schedule::is_due → copilot::run_task(unattended=true) → history::entry_from_outcome）。
//
//   ラウンドA: 一時 data ディレクトリに schedules.json(daily、time=現在時刻の1分前、
//              last_run_at なし)を作成 → is_due が true になることを確認 →
//              greeter エージェント(tools:[] のため権限確認は発生しない)で
//              unattended=true の run_task を実行 → 完了後 history.jsonl に
//              trigger="scheduled" のエントリが追記されることを assert。
//   ラウンドB: outputDir 未設定のエージェント設定で、作業ディレクトリ内のファイルへの
//              書き込みを依頼(通常なら Ask になる状況)を unattended=true で実行 →
//              PermissionRequested が1件も emit されず、無人実行の拒否文言つきで
//              即座に TaskFailed になり、ファイルが作成されないことを assert。
//
// このパッケージには lib クレートが無く、main.rs のモジュールを bin から直接 use できないため、
// #[path] で agents.rs / events.rs / config.rs / permissions.rs / copilot.rs / history.rs /
// schedule.rs を共有する(step2_check.rs 以降と同じ手法)。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --manifest-path src-tauri/Cargo.toml --bin step7_check

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
// copilot.rs が use crate::audit(監査ログ。docs/roadmap.md v0.6)を要求するため共有する。
// このバイナリでは監査ログの中身までは検証しないため allow(dead_code)。
#[path = "../audit.rs"]
#[allow(dead_code)]
mod audit;
#[path = "../history.rs"]
mod history;
#[path = "../schedule.rs"]
mod schedule;

use chrono::{Duration, Local, Timelike};
use events::AppEvent;
use std::path::PathBuf;

const GREETER_MD: &str = "---\nname: greeter\ndescription: 挨拶担当\ntools: []\n---\nあなたは挨拶担当です。ツールを使わず、依頼に一言で答えてください。\n";
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

    let tmp = std::env::temp_dir().join(format!("agent-deck-step7-{}", std::process::id()));
    let agent_dir = tmp.join("agents");
    let data_dir = tmp.join("data");
    for dir in [&agent_dir, &data_dir] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("フォルダを作成できません({}): {e}", dir.display());
            std::process::exit(1);
        }
    }
    if let Err(e) = std::fs::write(agent_dir.join("greeter.agent.md"), GREETER_MD) {
        eprintln!("greeter.agent.md を書き込めません: {e}");
        std::process::exit(1);
    }
    let no_shared_dir = agent_dir.join("__no_shared__");
    let definitions = match agents::scan_definitions(&[agent_dir.clone()], &no_shared_dir) {
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
    let greeter_spec = copilot::AgentSpec {
        name: greeter.name.clone(),
        display_name: None,
        description: greeter.description.clone(),
        tools: greeter.tools.clone(),
        model: greeter.model.clone(),
        prompt: greeter.body.clone(),
    };

    let ok_a = run_round_a(&cli_path, &greeter_spec, &data_dir).await;
    let ok_b = run_round_b(&cli_path, &data_dir).await;

    if ok_a && ok_b {
        println!("\nすべてのラウンドが成功しました");
        std::process::exit(0);
    }
    eprintln!("失敗したラウンドがあります: A(スケジュール発火+無人実行+履歴)={ok_a} B(Ask相当を即拒否)={ok_b}");
    std::process::exit(1);
}

/// ラウンド1回分の実行結果(AppEvent チャネル経由で集計したもの + run_task の RunOutcome)。
#[derive(Default)]
struct RoundOutcome {
    permission_requests: Vec<(String, String)>,
    completed: bool,
    failed: bool,
    error: Option<String>,
    run_outcome: Option<copilot::RunOutcome>,
}

/// run_task を実行し、受信イベントを標準エラーへ列挙しつつ結果を集計する。
/// unattended=true では PermissionRequested は本来 emit されないはずだが、想定外に
/// 届いた場合でもハングしないよう安全側(拒否)で bridge.respond する
/// (step4/step6_check.rs と同じ理由)。
async fn run_task_and_collect(
    cli_path: &PathBuf,
    ws: &PathBuf,
    agent: copilot::AgentSpec,
    rules: config::AgentSettings,
    prompt: &str,
    unattended: bool,
) -> RoundOutcome {
    let bridge = copilot::PermissionBridge::new();
    let logs_dir = std::env::temp_dir().join(format!("agent-deck-step7-logs-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&logs_dir);
    let spec = copilot::TaskSpec {
        prompt: prompt.to_string(),
        agent_id: agent.name.clone(),
        agents: vec![agent.clone()],
        selected_agent_name: agent.name.clone(),
        working_directory: ws.clone(),
        session_model: None,
        rules,
        bridge: bridge.clone(),
        unattended,
        logs_dir,
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
                if let Err(e) = bridge.respond(request_id, false) {
                    eprintln!("bridge.respond に失敗しました: {e}");
                }
            }
            AppEvent::TaskCompleted { .. } => outcome.completed = true,
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

/// ラウンドA: schedules.json への発火判定(is_due)→ 無人実行 → 履歴(trigger=scheduled)。
async fn run_round_a(cli_path: &PathBuf, greeter_spec: &copilot::AgentSpec, data_dir: &PathBuf) -> bool {
    println!("\n=== ラウンドA: スケジュール発火 → 無人実行 → 履歴(trigger=scheduled) ===");

    let now = Local::now();
    let due_time = now - Duration::minutes(1);
    let sch = schedule::Schedule {
        id: "sched-1".to_string(),
        agent_id: "greeter".to_string(),
        prompt: "こんにちはとだけ返してください".to_string(),
        recurrence: schedule::Recurrence::Daily { time: format!("{:02}:{:02}", due_time.hour(), due_time.minute()) },
        enabled: true,
        last_run_at: None,
    };
    if let Err(e) = schedule::save(data_dir, &schedule::SchedulesFile { version: 1, schedules: vec![sch] }) {
        eprintln!("ラウンドA失敗: schedule::save に失敗しました: {e}");
        return false;
    }

    let loaded = match schedule::load(data_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ラウンドA失敗: schedule::load に失敗しました: {e}");
            return false;
        }
    };
    let Some(loaded_sch) = loaded.schedules.first() else {
        eprintln!("ラウンドA失敗: schedules.json から読み戻せませんでした");
        return false;
    };
    match schedule::is_due(loaded_sch, now) {
        Ok(true) => println!("is_due OK: {loaded_sch:?}"),
        Ok(false) => {
            eprintln!("ラウンドA失敗: is_due が false でした(due になっているはず): {loaded_sch:?}");
            return false;
        }
        Err(e) => {
            eprintln!("ラウンドA失敗: is_due がエラーを返しました: {e}");
            return false;
        }
    }

    let prompt = loaded_sch.prompt.clone();

    let ws = data_dir.join("ws-a");
    if let Err(e) = std::fs::create_dir_all(&ws) {
        eprintln!("ラウンドA失敗: 作業フォルダを作成できません: {e}");
        return false;
    }

    let outcome =
        run_task_and_collect(cli_path, &ws, greeter_spec.clone(), config::AgentSettings::default(), &prompt, true)
            .await;

    let Some(run_outcome) = &outcome.run_outcome else {
        eprintln!(
            "ラウンドA失敗: RunOutcome を取得できませんでした(completed={} failed={} error={:?})",
            outcome.completed, outcome.failed, outcome.error
        );
        return false;
    };
    if run_outcome.status != copilot::TaskStatus::Completed {
        eprintln!("ラウンドA失敗: RunOutcome.status が Completed ではありません: {:?}", run_outcome.status);
        return false;
    }
    if !outcome.permission_requests.is_empty() {
        eprintln!(
            "ラウンドA失敗: PermissionRequested が emit されました(greeter は tools:[] のはず): {:?}",
            outcome.permission_requests
        );
        return false;
    }

    // main.rs のスケジューラと同じ順序: 発火時刻を last_run_at として書き戻す。
    let mut sch_file = loaded;
    sch_file.schedules[0].last_run_at = Some(now.to_rfc3339());
    if let Err(e) = schedule::save(data_dir, &sch_file) {
        eprintln!("ラウンドA失敗: last_run_at の書き戻しに失敗しました: {e}");
        return false;
    }

    let entry = history::entry_from_outcome("greeter", &prompt, run_outcome, "scheduled");
    if let Err(e) = history::append(data_dir, &entry) {
        eprintln!("ラウンドA失敗: history::append に失敗しました: {e}");
        return false;
    }
    let list = match history::list(data_dir, 10) {
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
    let mut ok = true;
    if first.trigger != "scheduled" {
        eprintln!("ラウンドA失敗: 履歴の trigger が scheduled ではありません: {}", first.trigger);
        ok = false;
    }
    if first.status != "completed" {
        eprintln!("ラウンドA失敗: 履歴の status が completed ではありません: {}", first.status);
        ok = false;
    }
    if ok {
        println!("ラウンドA OK: summary={:?}", run_outcome.summary);
        println!(
            "ラウンドA OK: status.as_str()={} input_files={:?}",
            run_outcome.status.as_str(),
            run_outcome.input_files
        );
        println!("ラウンドA OK: history={}", serde_json::to_string(first).unwrap_or_default());
    }
    ok
}

/// ラウンドB: outputDir 未設定(通常なら Ask になる状況)を unattended=true で実行 →
/// PermissionRequested を出さずに即座に拒否文言つきの TaskFailed になり、
/// ファイルは作成されないこと(docs/roadmap.md v0.4: 無人実行と承認ダイアログは両立しない)。
async fn run_round_b(cli_path: &PathBuf, data_dir: &PathBuf) -> bool {
    println!("\n=== ラウンドB: 無人実行 + outputDir 未設定(Ask相当) → 即座に拒否 ===");
    let agent = copilot::AgentSpec {
        name: "writer".to_string(),
        display_name: None,
        description: "ファイル作成担当".to_string(),
        tools: None,
        model: None,
        prompt: WRITER_PROMPT.to_string(),
    };
    let ws = data_dir.join("ws-b");
    if let Err(e) = std::fs::create_dir_all(&ws) {
        eprintln!("ラウンドB失敗: 作業フォルダを作成できません: {e}");
        return false;
    }
    let target = ws.join("x.txt");
    let prompt = format!("{} に こんにちは と書いてください", target.display());
    // rules は既定値(output_dir 無し)。architecture.md §7.1 の判定では出力フォルダ未設定の
    // ため自動承認されず、通常は Ask になる状況。
    let outcome =
        run_task_and_collect(cli_path, &ws, agent, config::AgentSettings::default(), &prompt, true).await;

    let mut ok = true;
    if !outcome.permission_requests.is_empty() {
        eprintln!(
            "ラウンドB失敗: PermissionRequested が {} 件 emit されました(無人実行なので0件のはず): {:?}",
            outcome.permission_requests.len(),
            outcome.permission_requests
        );
        ok = false;
    }
    if !outcome.failed {
        eprintln!(
            "ラウンドB失敗: TaskFailed を受信できませんでした(completed={})",
            outcome.completed
        );
        ok = false;
    }
    let error = outcome.error.clone().unwrap_or_default();
    if !error.contains("無人実行のため") {
        eprintln!("ラウンドB失敗: TaskFailed.error に無人実行拒否の文言がありません: {error:?}");
        ok = false;
    }
    if target.is_file() {
        eprintln!("ラウンドB失敗: 拒否されたはずなのに {} が作成されました", target.display());
        ok = false;
    }
    match &outcome.run_outcome {
        Some(run_outcome) if run_outcome.status != copilot::TaskStatus::Failed => {
            eprintln!("ラウンドB失敗: RunOutcome.status が Failed ではありません: {:?}", run_outcome.status);
            ok = false;
        }
        None => {
            eprintln!("ラウンドB失敗: RunOutcome を取得できませんでした");
            ok = false;
        }
        _ => {}
    }

    if ok {
        println!("ラウンドB OK(error={error:?})");
    }
    ok
}

fn print_event(ev: &AppEvent) {
    match serde_json::to_string(ev) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("イベントの JSON 化に失敗しました: {e}"),
    }
}
