# 開発ガイド

環境構築、日常のコマンド、実装の進め方と現在の進捗。設計判断は [architecture.md](architecture.md) へ。

## 1. 環境構築

必要なもの:

| ツール | 確認方法 | 備考 |
|---|---|---|
| Rust(stable) | `cargo --version` | 1.97 で検証済み |
| Node.js 20+ | `node --version` | 24 で検証済み |
| GitHub Copilot CLI | `copilot --version` | 1.0.79 で検証済み。**認証済みであること**(`copilot login`)。組織ポリシーで CLI が有効になっている必要あり |

Copilot CLI が PATH に無い環境では、環境変数 `COPILOT_CLI_PATH` に `copilot.exe` の
フルパスを設定する(開発機では winget 導入のため
`%LOCALAPPDATA%\Microsoft\WinGet\Packages\GitHub.Copilot_...\copilot.exe`)。

```
npm install        # 初回のみ
```

## 2. 日常のコマンド

```
npm run tauri dev                                   開発起動(ホットリロード)
npm run tauri build                                 リリースビルド
cargo check --manifest-path src-tauri/Cargo.toml    Rust だけ素早く検証
cargo test  --manifest-path src-tauri/Cargo.toml    Rust ユニットテスト
npx tsc --noEmit                                    TypeScript 型チェック
```

SDK 疎通の再確認(ステップ1の成果物):

```
$env:COPILOT_CLI_PATH = "<copilot.exe のパス>"
cargo run --manifest-path src-tauri/Cargo.toml --bin smoke
```

## 3. コーディング方針

- **可読性と監査性を最優先。** 動けばよいコードではなく、後から人が読んで追える構造にする
- **設定はすべてファイルで表現する。** GUI でしか変更できない状態を作らない
- 外部依存クレートは必要最小限。追加する場合は理由をコメントに残す
- エラーは握りつぶさない。UI に理由が出るところまで通す
- コメントは日本語可。コード上の識別子は英語で統一
- Rust 型とフロントの `src/types.ts` は必ず同期させる
- Copilot SDK / CLI / Tauri は変化が速い。記憶で書かず、`doc-researcher` エージェントで裏取りしてから書く

## 4. 実装の進め方

以下の順序で進め、**各ステップが動作することを確認してから次に進む。**

### 進捗

- [x] **ステップ1: 疎通確認**(2026-08-12 完了)
  SDK でセッションを作り、固定プロンプトへの応答を確認。成果物は `src-tauri/src/bin/smoke.rs`。
  確定した API の実態は [sdk-notes.md](sdk-notes.md) の「ステップ1 疎通確認の結果」参照
- [x] **ステップ2: イベントの可視化**(2026-08-12 完了)
  `copilot.rs` 新設(resolve_cli_path / EventContext::convert / run_task)。フロントは時系列羅列表示。
  ヘッドレス動作確認は `cargo run --bin step2_check`(COPILOT_CLI_PATH 必須)。
  既知の残課題: 中断は disconnect ベース → ステップ3で `Session::abort()` + `aborted` フラグ分岐に切替予定
- [x] **ステップ3: エージェント定義の読み込み**(2026-08-12 完了)
  `.agent.md` のフルパース(tools/model/本文)→ `CustomAgentConfig` 詰め替え → `with_agent` で選択実行。
  選択外の定義も委任候補としてセッションに渡す。中断は `Session::abort()` ベースに切替済み。
  ヘッドレス検証は `cargo run --bin step3_check`(通常完了+中断の 2 ラウンド)
- [x] **ステップ4: 入出力設定と権限制御**(2026-08-12 完了)
  UiPermissionHandler(decide 接続・UI 承認ブリッジ・拒否時 abort)、設定画面(tauri-plugin-dialog)、
  CLI 記法のパターン照合(`shell(python:*)` 等)。境界ケースは `cargo run --bin step4_check` で
  実機3ラウンド検証済み(自動承認 / Ask→拒否で中断+ファイル無し / Ask→承認)。
  権限要求の実形(fileName / fullCommandText)は sdk-notes.md に記録
- [x] **ステップ5: ツリー表示**(2026-08-12 完了)
  `buildTree` 純関数(src/tree.ts、vitest 9 ケース)+ 実行ビューのツリー描画。
  実機で委任を観測し相関確定(`agent_id` = 委任元 task ツールの tool_call_id、sdk-notes.md 参照)。
  検証: `npx vitest run` と `cargo run --bin step5_check`
- [ ] **ステップ6: 履歴**
  history.jsonl への追記(実装済み: `history.rs` の `append`)をタスク完了処理に接続し、一覧表示

### 検証の習慣

- 修正ループの検証(cargo check / tsc / テスト)は `build-checker` エージェント(Haiku)で回す
- 設計が確定した実装は `impl` エージェント(Sonnet)に、ファイル・型・関数名まで指定して依頼する

## 5. リリースとポータブル配布

- `npm run tauri build` 後、`src-tauri/target/release/agent-deck.exe` を**単体でコピー**して配布する
  (Tauri 公式のバンドルターゲットに「ポータブル exe」は無いが、この方式で動作する)
- 前提: 配布先に WebView2 ランタイム(Windows 10 2004+ / 11 は標準搭載)
- **配布先での CLI 解決を必ず検証すること。** 開発機は SDK の build.rs が展開したキャッシュで
  動いてしまうため、「開発機で動く」は配布先で動く根拠にならない(architecture.md §1.2)
