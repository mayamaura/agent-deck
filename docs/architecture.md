# 設計

agent-deck を「どう」実現するかを定義する文書。要求は [requirements.md](requirements.md)、
SDK/CLI の API 実態は [sdk-notes.md](sdk-notes.md) を参照。
本文書の記述は実機検証済みの事実(sdk-notes.md)を反映済み。

## 1. 技術スタック

| 区分 | 選定 | 補足 |
|---|---|---|
| デスクトップフレームワーク | Tauri v2 | |
| バックエンド | Rust | ビジネスロジックはすべてここ |
| Copilot 連携 | `github-copilot-sdk`(公式 Rust SDK) | **非公式クレート禁止**(下記) |
| フロントエンド | React + TypeScript + Vite | 表示と入力のみ |
| 状態管理 | useState / useReducer で足りる範囲に収める | |
| 配布 | ポータブル exe(`target/release` の単体コピー) | |

### 1.1 クレート選定の注意

crates.io には Copilot CLI を制御する非公式クレートが複数存在する
(`copilot-sdk-supercharged`、`copilot-client` など)。
**採用するのは `github/copilot-sdk` リポジトリ由来の `github-copilot-sdk` のみ。**
外部クレートの追加は必要最小限とし、追加時は理由をコメントに残す。

### 1.2 CLI の扱い

- `default-features = false` で `bundled-cli` を無効化(ビルド時間とサイズ優先)
- SDK は PATH スキャンをしないため、**CLI パスの解決はアプリ側の責務**。
  解決順: `config.json` の `copilotCliPath` → 環境変数 `COPILOT_CLI_PATH` →
  アプリ自身が PATH を探索(`where copilot` 相当)して SDK に明示パスを渡す
- 注意: `default-features = false` でも SDK の build.rs は開発機キャッシュに CLI を展開するため
  **開発機で動いても配布先で動く保証はない**。配布時の CLI 解決は必ず実機で検証する
- SDK と CLI はバージョンが対で管理される(crate 同梱の `cli-version.txt`)。
  更新時は必ず動作確認をセットで行う

## 2. 責務の分割

```
┌─────────────────────────────────────────┐
│ フロントエンド (React / TypeScript)      │
│  - 画面描画、ユーザー操作                 │
│  - invoke でコマンド呼び出し              │
│  - listen でイベント受信                  │
└──────────────┬──────────────────────────┘
               │ Tauri IPC
┌──────────────┴──────────────────────────┐
│ バックエンド (Rust)                       │
│  - Copilot SDK のセッション管理           │
│  - イベント購読 → フロントへ emit         │
│  - 設定ファイルの読み書き                 │
│  - 権限判定                               │
└──────────────┬──────────────────────────┘
               │
        Copilot CLI ランタイム
```

**原則: ビジネスロジックはすべて Rust 側に置く。**
フロントエンドは表示と入力に徹し、判断を持たない。

### 2.1 Rust モジュール対応表

| ファイル | 責務 |
|---|---|
| `src-tauri/src/main.rs` | Tauri 起動、コマンド定義、AppState(実行中タスク・PermissionBridge) |
| `src-tauri/src/copilot.rs` | SDK セッション管理・実行(run_task/RunOutcome)、イベント変換(EventContext)、権限ハンドラ(UiPermissionHandler/PermissionBridge)。SDK 型はこのモジュールに閉じる |
| `src-tauri/src/config.rs` | config.json / agents.json の読み書き、data/・個人/共有ディレクトリの解決 |
| `src-tauri/src/agents.rs` | `.agent.md` のフルパース、個人+共有スキャン(個人優先 dedup) |
| `src-tauri/src/sync.rs` | 共有定義のフォルダ同期とマニフェスト(sha256) |
| `src-tauri/src/events.rs` | `AppEvent` 定義(SDK イベントの変換先) |
| `src-tauri/src/permissions.rs` | 権限判定(decide)・CLI 記法パターン照合・パス正規化 |
| `src-tauri/src/history.rs` | history.jsonl の追記・読み出し・RunOutcome からの組み立て |
| `src-tauri/src/bin/*.rs` | smoke / step2〜6_check: 各ステップのヘッドレス実機検証バイナリ |
| `src/tree.ts` | イベント列 → ツリー状態の純関数(vitest でテスト) |

## 3. Tauri コマンド(フロント → Rust)

| コマンド | 引数 | 戻り値 | 用途 |
|---|---|---|---|
| `list_agents` | なし | `Vec<AgentSummary>` | 一覧の取得(個人+共有のマージ。version / shadowed 込み) |
| `get_agent_config` | `agent_id` | `Option<AgentSettings>` | 入出力設定の取得 |
| `save_agent_config` | `agent_id`, `AgentSettings` | `()` | 入出力設定の保存 |
| `start_task` | `agent_id`, `prompt` | `()` | タスクの実行開始 |
| `cancel_task` | `session_id` | `()` | 実行の中断(Session::abort) |
| `respond_permission` | `request_id`, `decision` | `()` | 承認ダイアログの応答 |
| `list_history` | `limit` | `Vec<HistoryEntry>` | 実行履歴の取得 |
| `open_output_folder` | `agent_id` | `()` | 出力先をエクスプローラで開く(`explorer` 直接起動、プラグイン不要) |
| `get_agent_definition` | `agent_id` | `AgentDefinitionDto` | 定義エディタ用の全文取得(v0.2) |
| `save_agent_definition` | `agent_id`, 各フィールド | `()` | 個人スコープ定義の保存(共有はエラー) |
| `create_agent_definition` | `agent_id`, 各フィールド | `()` | 個人スコープに新規作成 |
| `duplicate_agent` | `agent_id` | `()` | 共有定義を同 id で個人へ複製(複製して編集) |
| `delete_agent_definition` | `agent_id` | `()` | 個人スコープ定義の削除 |
| `sync_shared_agents_cmd` | なし | `SyncSummary` | 共有元フォルダから data/shared-agents へ同期 |
| `get_app_config` | なし | `AppConfigDto` | アプリ設定の取得(共有元フォルダ等) |
| `save_shared_agents_source` | `path` | `()` | 共有元フォルダの保存(config.json を更新) |

## 4. イベント(Rust → フロント)

SDK のセッションイベントはアプリ独自の `AppEvent` に**変換してから** emit する。
SDK の型をそのままフロントに流さない(SDK 更新時の影響を Rust 側で吸収するため)。

- チャネルは `agent://event` の単一チャネル。`kind` で判別
- 定義: `src-tauri/src/events.rs`(Rust)/ `src/types.ts`(フロント側ミラー。**必ず同期**)
- `PermissionRequested` の権限種別フィールドは `permission_kind`
  (serde の判別タグ `kind` と衝突するため。初期仕様書からの変更点)

SDK イベント → AppEvent の対応(ステップ2で実装。イベント名は実機観測済み):

| SDK イベント | AppEvent |
|---|---|
| 送信成功(start_task 内) | `TaskStarted` |
| `assistant.intent` | `AgentIntent` |
| `subagent.started` / `completed` / `failed` | `SubagentStarted` / `Completed` / `Failed` |
| `tool.execution_start` / `tool.execution_complete` | `ToolStarted` / `ToolCompleted` |
| `permission.requested`(カスタム PermissionHandler 経由) | `PermissionRequested` |
| `assistant.usage` / `session.usage_info` | `UsageUpdated` |
| `session.idle` | `TaskCompleted` |
| `session.error` | `TaskFailed` |
| (ユーザー中断 — cancel_task) | `TaskCancelled` |

注意(実機確認): ストリーミングの有無は実行ごとに揺れる。delta 系
(`assistant.message_delta` の `deltaContent`)が来ない実行があるため、
確定本文は `assistant.message` の `content` を正とする。

## 5. メイン/サブエージェントの判別

**これがツリー表示の要。**(実機検証済み — sdk-notes.md)

セッションイベントのエンベロープの `agent_id` フィールドは、
**サブエージェント由来のイベントにのみ付与され、メインエージェントと
セッションレベルのイベントには付かない。**

- `agent_id` が `None` → メインエージェントの行に表示
- `agent_id` が `Some(id)` → その ID のサブエージェント行に表示

`subagent.started` でサブエージェント行を生成し、`subagent.completed` /
`subagent.failed` で確定させる。

## 6. データモデル

### 6.1 ファイル配置

```
<exe と同じ階層>/
├─ agent-deck.exe
└─ data/
   ├─ config.json               アプリ全体の設定
   ├─ agents.json               エージェントごとの入出力設定
   ├─ history.jsonl             実行履歴(追記のみ)
   ├─ agents/                   個人スコープの定義(agentDirs 未設定時の既定)
   ├─ shared-agents/            共有スコープの定義(同期先・アプリ内読み取り専用)
   ├─ shared-agents.meta.json   同期マニフェスト(同期元・時刻・sha256)
   ├─ workspace/<agent_id>/     inputDir 未設定エージェントの作業ディレクトリ
   ├─ schedules.json            定期実行の定義(v0.4)
   ├─ policy.json               管理者ポリシー(任意配布。forcedDeniedTools)
   └─ logs/                     セッション単位の監査ログ(session-*.jsonl)と
                                来歴(provenance-*.json)。logRetentionDays で自動削除
```

ポータブル運用のため、**ユーザープロファイル配下ではなく exe と同階層**に置く。
書き込み不可の場所(Program Files 等)に置かれた場合は、起動時にエラーを出して
別の場所への配置を促す(実装: `config::data_dir()` の書き込みプローブ)。

エージェント定義(`.agent.md`)は Copilot CLI の規約に従った場所に置き、
その探索パスを `config.json` で指定する(CLI の規約パスは sdk-notes.md 参照)。

### 6.2 config.json

```json
{
  "version": 1,
  "agentDirs": ["C:/work/agent-deck/agents"],
  "copilotCliPath": null,
  "defaultModel": null,
  "logLevel": "info",
  "sharedAgentsSource": null,
  "updateSource": null,
  "maxConcurrentTasks": 2,
  "logRetentionDays": 90
}
```

`copilotCliPath` が `null` の場合は `COPILOT_CLI_PATH` 環境変数、それも無ければ
アプリが PATH を探索して解決する(§1.2。解決した絶対パスを SDK に明示的に渡す)。

### 6.3 agents.json

エージェント定義そのものではなく、**アプリ側で持つ運用設定**。
`.agent.md` は Copilot CLI の管轄なので、アプリが勝手に書き換えない。

```json
{
  "version": 1,
  "agents": {
    "survey-analyst": {
      "inputDir": "C:/work/data/survey/input",
      "outputDir": "C:/work/data/survey/output",
      "allowedTools": ["write", "shell(python:*)"],
      "deniedTools": ["shell(rm)"],
      "autoApproveWriteInOutputDir": true
    }
  }
}
```

`allowedTools` / `deniedTools` の記法は CLI の `--allow-tool` / `--deny-tool` フラグと同じ
(`shell(COMMAND)`、ワイルドカードは shell のみ)。`.agent.md` の `tools` フィールドの記法ではない。

### 6.4 history.jsonl

1 行 1 実行。追記のみ。

```json
{"sessionId":"...","agentId":"survey-analyst","prompt":"...","startedAt":"2026-08-12T09:00:00Z","durationMs":251000,"status":"completed","outputFiles":["report_2026Q2.md"],"totalTokens":48210,"subagents":[{"name":"data-cruncher","durationMs":120000}]}
```

## 7. 権限制御

**業務データを扱うため、ここは手を抜かない。**

### 7.1 判定ロジック(実装: `permissions::decide`)

1. `deniedTools` に該当 → 無条件で拒否
2. 書き込み先が `outputDir` 配下、かつ `autoApproveWriteInOutputDir` が true → 自動承認
3. `allowedTools` に該当 → 自動承認
4. それ以外 → `PermissionRequested` を emit して UI で確認

SDK 側の接続点は `PermissionHandler` トレイトの自前実装
(組み込みの `approve_if` は同期述語のため、UI 確認をはさむ本アプリの要件には不足)。

### 7.2 作業ディレクトリ

セッションの作業ディレクトリは、そのエージェントの `inputDir` の親、
または専用のワークスペースフォルダに限定する。
**ユーザープロファイル直下や、無関係なファイルを含む親フォルダを
作業ディレクトリにしないこと。**

### 7.3 パス判定

`outputDir` 配下かどうかの判定は、文字列前方一致ではなく
**正規化した絶対パス**で行う(`..` やシンボリックリンクによる脱出を防ぐ)。
実装は `permissions::is_within`(存在しないパスは実在する最深の祖先まで
canonicalize し、残り成分に `..` があれば不正扱い)。境界ケースはユニットテストで担保。

## 8. 設計原則(失敗しやすい点)

### 8.1 数値の集計を LLM にやらせない

アンケート集計も予算消費率も、答えが一意に決まる計算。LLM に CSV を読ませて
直接集計させると、もっともらしいが誤った数字が出る。

正しい分担:

1. エージェントに**集計スクリプトを書かせて実行させる** — 数字はスクリプトが出す
2. その出力を根拠に、**エージェントがレポートの文章を書く**
3. スクリプトは再利用できるよう保存する

この方針は `.agent.md` の Instructions に明記する(`agents/` のサンプル参照)。
アプリ側では、生成されたスクリプトが出力フォルダに残ることを保証する。

### 8.2 バージョンの追跡可能性

「同じ依頼をしたのに人によって結果が違う」という相談は必ず来る。
成果物には、使用したエージェント定義のバージョンとモデル名をフッターに記録する。

### 8.3 失敗時の状態

タスクが途中で失敗したとき、出力フォルダに中途半端なファイルが残る。
「このファイルは完成品か、途中で止まったものか」が UI から判別できるようにする
(履歴の `status` と紐づける)。
