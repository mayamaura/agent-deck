# SDK / CLI / Tauri 調査メモ(2026-08-12 実施)

初期仕様書([archive/agent-deck-spec.md](archive/agent-deck-spec.md))§0.1 が求めた事前確認の結果。
**プロジェクト文書と食い違う場合はこちら(実機・実ソースで確認した方)が優先。**
出典は各リサーチの一次情報(docs.rs / crates.io / raw.githubusercontent.com / docs.github.com / v2.tauri.app / npm registry)。

## 初期仕様書との食い違い(確認済み・現行文書には反映済み)

以下の § は初期仕様書の節番号。修正内容はすべて [architecture.md](architecture.md) に織り込み済みなので、
現行文書だけを読む場合この節は読み飛ばしてよい(経緯の記録)。

1. **CLI の PATH 解決は SDK がやらない(仕様書 §5.2 と相違)。**
   `default-features = false` の場合、SDK は PATH スキャンを行わないと README に明記。
   `ClientOptions.program = CliProgram::Path(...)` か環境変数 `COPILOT_CLI_PATH` の明示指定が必須。
   → 仕様書の「PATH から解決」はアプリ側で実装する(`where copilot` 相当で探して SDK に明示的に渡す)。
2. **`bundled-cli` 無効時でも build.rs は CLI をダウンロードする。**
   開発機のユーザーキャッシュ(`%LOCALAPPDATA%\github-copilot-sdk\cli\<version>\copilot.exe`)に展開され、
   開発機ではそれで動いてしまうが**配布バイナリには含まれない**。配布時の CLI 解決を必ず別途検証すること。
   ビルド時ダウンロードを止めるには `COPILOT_SKIP_CLI_DOWNLOAD=1`。
3. **`shell(python:*)` のようなツールパターンは `.agent.md` の `tools` の記法ではない。**
   これは CLI の `--allow-tool` / `--deny-tool` フラグの記法(`--deny-tool` が優先)。
   `.agent.md` の `tools` は単純なツール名リスト(`["read", "edit", "search"]`、MCP は `server/tool`)。
4. **仕様書 §4.3 の `PermissionRequested { kind }` は serde タグ名 `kind` と衝突する。**
   実装ではフィールド名を `permission_kind` に変更済み(src-tauri/src/events.rs)。

## 初期仕様書の記述が正しいと確認できたもの

- `agent_id` はサブエージェント由来イベントのみに付与され、メイン/セッションレベルには付かない
  (生成ソース `session_events.rs` の doc コメントで確認。§4.4 のツリー判別ロジックは成立する)
- `permission::approve_all` / `deny_all` / `approve_if(predicate)` は実名のまま実在(`permission.rs` 直読み)
- SDK と CLI はバージョンが対で管理される(crate 同梱の `cli-version.txt` で固定)

## github-copilot-sdk(Rust 公式 SDK)

- 最新安定版 **1.0.9**(2026-08-06)。edition 2024、MIT。リポジトリは github/copilot-sdk の `rust/`
- feature: `default = ["bundled-cli"]`。他に `bundled-in-process` / `derive` / `test-support`
- 主要 API(README + examples/chat.rs で確認):

```rust
use github_copilot_sdk::{Client, ClientOptions, SessionConfig};

let client = Client::start(ClientOptions::default()).await?;
let session = client.create_session(SessionConfig::default()
    .with_permission_handler(Arc::new(handler))).await?;

let mut events = session.subscribe();            // broadcast receiver
while let Ok(event) = events.recv().await {
    // event.event_type: &str, event.agent_id: Option<String>,
    // event.id / timestamp / parent_id / payload
}

session.send("プロンプト").await?;                // message_id を返すだけ
session.send_and_wait(MessageOptions::new("...")  // session.idle / session.error まで待つ
    .with_wait_timeout(Duration::from_secs(120))).await?;  // 既定 60 秒

session.disconnect().await?;
client.stop().await?;
```

- カスタム権限: `PermissionHandler` トレイト
  `async fn handle(&self, session_id, request_id, data: PermissionRequestData) -> PermissionResult`
  を実装して `SessionConfig::with_permission_handler(Arc::new(...))`。
  UI 確認をはさむ本アプリの要件では `approve_if`(同期述語)では足りず、このトレイト実装が本命
- 主要イベント種別(全 101 種、生成ソースで確認): `assistant.intent` / `assistant.usage` /
  `tool.execution_start` / `tool.execution_complete` / `subagent.started` / `subagent.completed` /
  `subagent.failed` / `permission.requested` / `session.idle` / `session.error` など
  - ストリーミング本文の delta は **`deltaContent` が正**(ステップ1で実機確認済み。README の `delta` 表記は誤り)
- tokio 必須(io/sync/process/net)。Tauri v2 は tokio 内蔵なので `#[tauri::command] async fn` から
  そのまま使える見込み(公式の Tauri 統合ガイドは存在しない。ステップ1で実証する)
- Cargo.toml への追加(ステップ1で有効化):
  `github-copilot-sdk = { version = "1.0.9", default-features = false }`

## ステップ1 疎通確認の結果(2026-08-12 実機検証)

`src-tauri/src/bin/smoke.rs` で SDK 経由のセッション作成 → プロンプト送信 → 応答受信 → 正常終了を確認。
実行: `$env:COPILOT_CLI_PATH = "<copilot.exe のパス>"; cargo run --manifest-path src-tauri/Cargo.toml --bin smoke`

実機で確定した API の事実:

- 型の場所: `Client` / `ClientOptions` はクレートルート、`MessageOptions` / `SessionConfig` / `SessionEvent` は
  `github_copilot_sdk::types`、`ApproveAllHandler` は `handler` モジュール
- `SessionEvent` のフィールド: `id / timestamp / parent_id / agent_id / event_type / data`(`data` は `serde_json::Value`)
- 確定本文は `assistant.message` の `content`、ストリーミングは `assistant.message_delta` の `deltaContent`
- `client.stop()` のエラー型は `StopErrors`(`github_copilot_sdk::Error` と別型、`From` 変換なし)
- **ストリーミングの有無は実行ごとに揺れる**: delta 系が一切来ず `assistant.message` だけの回と、
  `assistant.streaming_delta` → `assistant.message_delta` → `assistant.message` と来る回があった。
  ステップ2のイベントマッピングは両方のケースを処理すること
- メインエージェントのイベントはすべて `agent_id=None`(§4.4 の判別ロジックどおり)

観測されたイベント種別(単純プロンプト実行時):

```
session.start, session.managed_settings_resolved, pending_messages.modified,
session.model_change, session.skills_loaded, session.auto_mode_resolved,
system.message, session.tools_updated, user.message, session.title_changed,
assistant.turn_start, session.usage_info, model.call_start,
assistant.streaming_delta, assistant.reasoning_delta,
assistant.message_start, assistant.message_delta, assistant.usage,
assistant.message, assistant.reasoning, assistant.turn_end,
session.usage_checkpoint, assistant.idle, session.idle, session.shutdown,
session.background_tasks_changed
```

(tool.* / subagent.* / permission.* はツールを使わない単純プロンプトのため未観測。ステップ2以降で実プロンプトで確認する)

## SessionConfig / 権限 / 中断の API 実態(2026-08-12 ソース直読み。ステップ2〜4 の設計根拠)

出典はローカル cargo レジストリの `github-copilot-sdk-1.0.9` ソース(types.rs / session.rs / handler.rs / generated/session_events.rs)。

### カスタムエージェント(ステップ3の要)

- **SDK は `.agent.md` を読まない。探索・パースは完全にアプリ側の責務。**
  パース結果を `CustomAgentConfig::new(name, prompt)`(+ `with_tools` / `with_model` /
  `with_display_name` / `with_infer` 等)に詰め替え、
  `SessionConfig::with_custom_agents([...])` で渡し、`with_agent(name)` で初期選択する
- `CustomAgentConfig` の主なフィールド: `name`(必須)/ `prompt`(必須 = .agent.md の本文)/
  `tools: Option<Vec<String>>`(None = 全ツール)/ `model` / `infer: Option<bool>`
- **サブエージェント委任は自動**: ランタイムがプロンプトと各エージェントの name/description を
  照合して委任する。`infer: false` で自動委任の対象外にできる
- `SessionConfig::with_working_directory(dir)` で作業ディレクトリ指定(architecture.md §7.2 用)。
  `with_model(...)` でセッションモデル指定(エージェント側 `model` は親モデルへのフォールバック付き上書き)

### ツール制限

- `SessionConfig` の `available_tools` / `excluded_tools` は**単純なツール名のみ**
  (excluded 優先が wire で固定)。CLI フラグの `shell(python:*)` パターン構文を SDK が
  解釈する証拠は無し → **パターンの適用は自前の PermissionHandler(permissions::decide)で行う**

### 権限ハンドラ(ステップ4の要)

- `PermissionHandler::handle(&self, session_id, request_id, data: PermissionRequestData) -> PermissionResult`(async)
- `PermissionResult::approve_once()` / `reject(feedback)` / `user_not_available()` で応答
- `PermissionRequestData`: 型付きは `kind: Option<PermissionRequestKind>`(shell/write/read/url/mcp/…)と
  `tool_call_id` 程度。**具体的なツール名や書き込み先パスは `extra: Value` の中**:
  `extra["permissionRequest"]["path"]`(SDK のテストで実証)、ツール名は `extra["permissionRequest"]` 配下
  (形は CLI バージョン依存と明記あり)。**防御的にパースし、取れなければ Ask に倒すこと**

### 中断

- **タスク中断の正道は `Session::abort()`**(`session.abort` RPC)。進行中の `send_and_wait` は
  `session.idle`(`SessionIdleData.aborted: Some(true)`)で解決される
  → idle 処理では `aborted` を見て TaskCompleted / TaskCancelled を分岐すること
- `disconnect()` はセッション状態を保持したまま切断(中断とは別用途)

### イベント payload の確定フィールド(変換実装で使うもの)

- `subagent.started`: `agent_name` / `agent_display_name` / `tool_call_id` / `model?`
- `subagent.completed`: `agent_name` / `duration_ms?` / `total_tokens?` / `tool_call_id`
- `subagent.failed`: 上記 + `error`
- `tool.execution_start`: `tool_name` / `tool_call_id` / `arguments?`
- `tool.execution_complete`: `success: bool` / `tool_call_id` / `error?{code?, message}`
- `assistant.intent`: `intent: String`
- `session.usage_info`: `current_tokens: i64` / `token_limit: i64`(両方必須フィールド)
- `session.error`: `message` / `error_type` / `error_code?` / `status_code?`
- 注意: `assistant.usage` の一部フィールド(`quota_snapshots` 等)は `pub(crate)` で外から読めない。
  必要になったら生 JSON(`data: Value`)経由で読む

### ClientOptions 補足

- `extra_args: Vec<String>` で CLI に生フラグを渡せる(パターン構文のツール制御が必要になった場合の退避経路)
- `working_directory`(ビルダーは `with_cwd`)は CLI プロセス自体の cwd。セッションの作業ディレクトリとは別物

## Copilot CLI

- 最新 **1.0.79**(2026-08-10)。インストール: `npm install -g @github/copilot`(Node 22+、
  Windows は PowerShell 6+ 必須)または `winget install GitHub.Copilot`
- **開発機に未インストール(2026-08-12 時点)。最初にやること**
- `.agent.md` の探索場所(優先順: ユーザー > リポジトリ > 組織 > エンタープライズ):
  - リポジトリ: `.github/agents/`
  - ユーザー: `~/.copilot/agents/`(`COPILOT_HOME` で起点変更可)
  - 組織/エンタープライズ: `.github(-private)` リポジトリの `/agents`(→ v0.2 の共有スコープに対応)
- frontmatter スキーマ: `description`(必須)、`name` / `tools` / `model` /
  `disable-model-invocation` / `user-invocable` / `mcp-servers` / `metadata`(任意)
- 実行: `copilot --agent=<name> --prompt "..."`。モデルは `--model` フラグでも定義側 `model` でも
  指定可(競合時の優先順位は公式に記載なし → 未決事項 #3 の判断材料)
- 組織ポリシー無効時のエラー:
  「You are not authorized to use this Copilot feature, it requires an enterprise or organization policy to be enabled.」

## Tauri v2

- 検証済みバージョン(2026-08-12、crates.io / npm API 直取得):
  `tauri` 2.11.5 / `tauri-cli` 2.11.4 / `@tauri-apps/api` 2.11.1 / `@tauri-apps/cli` 2.11.4 /
  `tauri-plugin-dialog` 2.7.2 / `tauri-plugin-opener` 2.5.4
- emit: `AppHandle` の `Emitter` トレイト(`app.emit("channel", &payload)`)。
  バックグラウンドからは `tauri::async_runtime::spawn` 内で `AppHandle` クローンを使う
  (v2 では素の `tokio::spawn` は「no reactor running」パニックの報告あり)
- listen: `import { listen } from "@tauri-apps/api/event"`
- async コマンドの引数に借用型(`&str` 等)は不可。所有型にする
- フォルダ選択はステップ4で `tauri-plugin-dialog` を追加(`open({ directory: true })`)。
  「エクスプローラで開く」は Windows 専用アプリなので `explorer` 直接起動で済ませ、opener プラグインは不要
- **ポータブル exe**: 公式のバンドルターゲットには存在しないが、`src-tauri/target/release/agent-deck.exe`
  を直接コピーして配布可能(公式 Discussion で複数確認)。前提は WebView2 ランタイム
  (Windows 10 2004+ / 11 は標準搭載)。updater / sidecar / deep link 等は使えないが v0.1 のスコープ外なので問題なし
