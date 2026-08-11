// SDK 疎通確認バイナリ(docs/development.md §4 ステップ1)。UI 不要。
// CLI パスはハードコードせず、環境変数 COPILOT_CLI_PATH を ClientOptions::default() に読ませる。
// 実行例(PowerShell):
//   $env:COPILOT_CLI_PATH = "...\copilot.exe"; cargo run --bin smoke

use std::sync::Arc;
use std::time::Duration;

use github_copilot_sdk::handler::ApproveAllHandler;
use github_copilot_sdk::types::{MessageOptions, SessionConfig, SessionEvent};
use github_copilot_sdk::{Client, ClientOptions};

fn print_event(event: &SessionEvent) {
    println!("[{}] agent_id={:?}", event.event_type, event.agent_id);
    // README 記載は delta、実サンプル(SDK examples/chat.rs)は deltaContent。
    // 実行して確認した結果 deltaContent が正しいフィールド名(実サンプルが正)。
    // assistant.message_delta が届かない実行もあったため、確定版の本文である
    // assistant.message の content も合わせて出す。
    match event.event_type.as_str() {
        "assistant.message_delta" => {
            if let Some(text) = event.data.get("deltaContent").and_then(|c| c.as_str()) {
                println!("  delta: {text}");
            }
        }
        "assistant.message" => {
            if let Some(text) = event.data.get("content").and_then(|c| c.as_str()) {
                println!("  content: {text}");
            }
        }
        _ => {}
    }
}

// StopErrors (client.stop() の失敗型) は github_copilot_sdk::Error と別型で
// From 変換がないため、Box<dyn Error> で両方まとめて ? 伝播する。
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::start(ClientOptions::default()).await?;

    let session = client
        .create_session(SessionConfig::default().with_permission_handler(Arc::new(ApproveAllHandler)))
        .await?;

    let mut events = session.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            print_event(&event);
        }
    });

    session
        .send_and_wait(
            MessageOptions::new("1たす1は?計算せず算数の知識で一言で答えてください。")
                .with_wait_timeout(Duration::from_secs(120)),
        )
        .await?;

    session.disconnect().await?;
    client.stop().await?;
    Ok(())
}
