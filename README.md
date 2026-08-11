# agent-deck

役割を定義した Copilot エージェントに定型業務(アンケート集計・予算分析などのレポート作成)を依頼し、
稼働状況をリアルタイムに把握して成果物を受け取る Windows デスクトップアプリ。

Tauri v2 + Rust / React + TypeScript。Copilot 連携は公式 `github-copilot-sdk`。
現在 v0.1 を開発中(ステップ1: SDK 疎通確認まで完了)。

## セットアップ

Rust(stable)・Node.js 20+・GitHub Copilot CLI(認証済み)が必要。
詳細は [docs/development.md](docs/development.md)。

```
npm install
npm run tauri dev
```

## ドキュメント

| 文書 | 内容 |
|---|---|
| [docs/requirements.md](docs/requirements.md) | 要求仕様: 目的・スコープ・画面・受け入れ条件 |
| [docs/architecture.md](docs/architecture.md) | 設計: 責務分割・IPC・イベント・データモデル・権限制御・設計原則 |
| [docs/development.md](docs/development.md) | 開発ガイド: 環境構築・コマンド・実装ステップと進捗 |
| [docs/roadmap.md](docs/roadmap.md) | 将来拡張(v0.2/v0.3)と v0.1 が塞いではいけない点 |
| [docs/open-questions.md](docs/open-questions.md) | 未決事項と決定ログ |
| [docs/sdk-notes.md](docs/sdk-notes.md) | Copilot SDK / CLI / Tauri の調査メモ(実機検証済み) |
| [docs/archive/agent-deck-spec.md](docs/archive/agent-deck-spec.md) | 初期仕様書のアーカイブ(歴史的資料。現行の正は上記文書群) |

## リポジトリ構成

| パス | 内容 |
|---|---|
| `src-tauri/` | Rust バックエンド(セッション管理・権限判定・設定・履歴) |
| `src/` | React フロントエンド(表示と入力のみ) |
| `agents/` | サンプルの `.agent.md`(Copilot カスタムエージェント定義) |
| `docs/` | 上記ドキュメント |
| `.claude/agents/` | 開発用 Claude Code サブエージェント定義 |
