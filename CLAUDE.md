# agent-deck 開発ガイド(Claude Code 向け)

Copilot エージェントに定型業務を依頼し、稼働状況をリアルタイム表示する Windows デスクトップアプリ。

## 文書マップ(迷ったらここから辿る)

| 知りたいこと | 文書 |
|---|---|
| 何を作るか(スコープ・画面・受け入れ条件) | [docs/requirements.md](docs/requirements.md) |
| どう作るか(責務・イベント・データ・権限) | [docs/architecture.md](docs/architecture.md) |
| 進め方・進捗・コマンド・コーディング方針 | [docs/development.md](docs/development.md) |
| SDK / CLI / Tauri の API 実態(実機検証済み) | [docs/sdk-notes.md](docs/sdk-notes.md) |
| 決めてよいこと・いけないこと | [docs/open-questions.md](docs/open-questions.md) |
| 将来の拡張と塞いではいけない点 | [docs/roadmap.md](docs/roadmap.md) |
| 素の Copilot との違い・機能を足す前の確認 | [docs/positioning.md](docs/positioning.md) |

初期仕様書は docs/ に分割済み。原本アーカイブは
[docs/archive/agent-deck-spec.md](docs/archive/agent-deck-spec.md)(参照のみ、編集しない)。

## 絶対に守ること

- [open-questions.md](docs/open-questions.md) の未決事項を独断で決めない。判断が必要になったらユーザーに確認する
- [requirements.md](docs/requirements.md) §2.2 のスコープ外機能(定義編集 UI、リポジトリ同期、自動更新、並行実行、スケジューラ)を実装しない
- ビジネスロジックは Rust 側。フロントは表示と入力のみ
- SDK の型をフロントに直接流さない。`AppEvent` に変換してから emit。Rust 型と `src/types.ts` は必ず同期
- パス判定は正規化済み絶対パスで行う。文字列前方一致は禁止
- 数値集計を LLM にやらせない。スクリプトを書かせて実行させる(architecture.md §8.1)
- エラーは握りつぶさず UI に理由が出るまで通す
- UI の機能を追加・変更するときは右クリックメニュー([requirements.md](docs/requirements.md) §3.6)も併せて設計し、§3.6 の表を更新する
- 外部クレート追加は必要最小限、追加時は理由をコメントに残す(公式 `github-copilot-sdk` 以外の Copilot 系クレート禁止)

## 開発コマンド

```
npm run tauri dev    開発起動
cargo check --manifest-path src-tauri/Cargo.toml   Rust だけ素早く検証
cargo test  --manifest-path src-tauri/Cargo.toml   Rust ユニットテスト
```

全コマンドと環境構築は [docs/development.md](docs/development.md)。

## トークン節約のためのサブエージェント運用

`.claude/agents/` に定義済み。**Fable/Opus を使うのは設計判断と難所のデバッグだけ**にし、
定型作業は下位モデルに投げること。

| agent | model | 使いどころ |
|---|---|---|
| `doc-researcher` | sonnet | Copilot SDK / CLI / Tauri の公式ドキュメント確認。この領域は変化が速いので、記憶で書かず必ずこれで裏取りする |
| `impl` | sonnet | 設計が確定した実装タスク。ファイル・型・関数名まで指定して依頼する |
| `build-checker` | haiku | cargo check / clippy / tsc / ビルドの実行とエラー要約。修正ループの検証はこれで回す |

## 実装の進め方

[docs/development.md](docs/development.md) §4 のステップ 1〜6 を順守。
各ステップの動作確認が済むまで次へ進まない。
