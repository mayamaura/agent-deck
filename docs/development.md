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

### マイルストーン進捗

- [x] v0.1(ステップ1〜6 + 受け入れ照合)— tag v0.1.0
- [x] v0.2 定義の管理と共有(スコープ分離・編集 UI・共有フォルダ同期)— tag v0.2.0
- [x] v0.3 更新の検知と通知(マニフェスト + sha256 検証)— tag v0.3.0
- [x] v0.4 定期実行と通知(スケジューラはアプリ起動中のみ・無人実行は Ask 即拒否・トースト通知)— tag v0.4.0
- [x] v0.5 並行実行とダッシュボード(上限は maxConcurrentTasks 既定 2、同一出力フォルダは排他)— tag v0.5.0
- [x] v0.6 監査とガバナンス(監査ログ・来歴・policy.json・保持期間 90 日暫定)— tag v0.6.0
- [~] v1.0 安定版 — **コード側の成果物は完了**(tag v1.0.0-rc1)。roadmap の状態条件との対応:
  1. 二次利用者が実運用で使えている — **ユーザーの運用確認待ち**
  2. 定義同期・更新配布の運用 — 機能は実装済み。**共有フォルダの実体の用意が必要**(ユーザー)
  3. 主要エラーの自己説明性 — 実装済み(認証・ポリシー・CLI パス・クレジット・タイムアウトにヒント付与)
  4. 利用者ガイド — [user-guide.md](user-guide.md) 作成済み
  条件 1・2 が確認できたら v1.0.0 の正式タグを打つこと
- rc1 以降の追加(実運用フィードバック由来。要求仕様は [requirements.md](requirements.md) §2.2 に反映済み):
  質問への回答(`ask_user`)、継続依頼の会話保持とレジューム、承認の 3 択化、
  ツール指定の複数選択 UI、定義・入出力設定の別ウインドウ化、エージェント ID のリネーム、
  出力フォルダのパスをモデルへ伝達、HUD テーマとアバター表示。
  実機検証は `cargo run --bin step10_check`(ask_user 往復・resume の文脈継承)
- **v1.0 の次**: [roadmap.md](roadmap.md) の v1.1(依頼を書かせない)以降。
  着手前にスコープをユーザーと確認し、requirements.md に昇格させてから実装に入る

### v0.1 ステップ進捗

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
- [x] **ステップ6: 履歴**(2026-08-12 完了)
  run_task が RunOutcome(status/所要/トークン/出力ファイル/サブエージェント)を返し、
  タスク終端で history.jsonl に追記。UI は履歴更新・出力フォルダを開く・未完成注記。
  検証: `cargo run --bin step6_check`(完了+中断の 2 ラウンド)

### v0.1 受け入れ条件の照合(2026-08-12)

requirements.md §4 の 10 項目の検証状況:

| # | 条件 | 検証 |
|---|---|---|
| 1 | ポータブル exe | **確認済み**: release ビルドの単体 exe(14.2MB)を別フォルダへコピーして起動、data/ が exe 横に生成されることを実機確認 |
| 2 | 一覧表示 | GUI 実機確認(スクリーンショット。未設定バッジ含む) |
| 3 | 選択して実行 | step3_check + GUI 配線確認 |
| 4 | リアルタイム表示 | step2/5_check + ツリー描画 |
| 5 | サブエージェント行と所要時間 | step5_check(実機で委任を観測) |
| 6 | 出力フォルダ内は承認なし | step4_check ラウンド A |
| 7 | 外は承認ダイアログ、拒否で停止 | step4_check ラウンド B(ファイル未作成まで確認) |
| 8 | 完了後のファイル一覧とフォルダを開く | step6_check + GUI ボタン |
| 9 | 設定の永続化 | config round-trip テスト + 設定画面 |
| 10 | 履歴記録 | step6_check(completed / cancelled の 2 種) |

GUI 上での通し操作(実際の業務データでのレポート生成)はユーザーの実運用確認待ち。

### 既知の設計上の限界

- モデルが write ツールでなくシェル(Set-Content 等)でファイルを書いた場合、
  自動承認の対象外(Ask に倒れる=安全側)であり、output_files にも載らない。
  対策はエージェント定義の Instructions で write ツール使用を指示すること(agents/ のサンプル参照)
- **`.agent.md` の未知 frontmatter キーは定義エディタでの保存時に失われる。**
  `main.rs` の `render_agent_md` が name/description/model/tools のみを書き出すため、
  `mcp-servers` などを手書きしても消える。roadmap v1.1/v1.3 の前提になるので、
  そちらに着手する際は最初にここを直す

### 検証の習慣

- 修正ループの検証(cargo check / tsc / テスト)は `build-checker` エージェント(Haiku)で回す
- 設計が確定した実装は `impl` エージェント(Sonnet)に、ファイル・型・関数名まで指定して依頼する

## 5. リリースとポータブル配布

- `npm run tauri build` 後、`src-tauri/target/release/agent-deck.exe` を**単体でコピー**して配布する
  (Tauri 公式のバンドルターゲットに「ポータブル exe」は無いが、この方式で動作する)
- 前提: 配布先に WebView2 ランタイム(Windows 10 2004+ / 11 は標準搭載)
- **配布先での CLI 解決を必ず検証すること。** 開発機は SDK の build.rs が展開したキャッシュで
  動いてしまうため、「開発機で動く」は配布先で動く根拠にならない(architecture.md §1.2)
