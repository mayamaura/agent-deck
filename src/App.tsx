import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentSummary, HistoryEntry } from "./types";

// 3 ペイン構成の骨格(docs/requirements.md §3.1)。
// 実行ビューのツリー表示はステップ2/5、設定画面はステップ4で実装する。
export default function App() {
  const [agents, setAgents] = useState<AgentSummary[]>([]);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<AgentSummary[]>("list_agents")
      .then(setAgents)
      .catch((e) => setError(String(e)));
    invoke<HistoryEntry[]>("list_history", { limit: 20 })
      .then(setHistory)
      .catch((e) => setError(String(e)));
  }, []);

  return (
    <div className="layout">
      <aside className="pane agents">
        <h2>エージェント</h2>
        {error && <p className="error">⚠ {error}</p>}
        {agents.length === 0 && !error && <p className="muted">定義がありません</p>}
        <ul>
          {agents.map((a) => (
            <li key={a.id}>
              <button
                className={selected === a.id ? "selected" : ""}
                onClick={() => setSelected(a.id)}
              >
                <strong>{a.name}</strong>
                <span className="muted">{a.description}</span>
              </button>
            </li>
          ))}
        </ul>
      </aside>
      <main className="pane run">
        <h2>実行ビュー</h2>
        {selected ? (
          <p className="muted">
            {selected} を選択中 — タスク実行はステップ1(SDK 疎通)以降で実装
          </p>
        ) : (
          <p className="muted">左の一覧からエージェントを選択してください</p>
        )}
      </main>
      <aside className="pane detail">
        <h2>詳細</h2>
        <p className="muted">ログ・トークン・出力ファイル(ステップ2以降)</p>
      </aside>
      <footer className="pane history">
        <h2>実行履歴</h2>
        {history.length === 0 ? (
          <p className="muted">履歴はまだありません</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>開始</th>
                <th>エージェント</th>
                <th>状態</th>
                <th>所要</th>
              </tr>
            </thead>
            <tbody>
              {history.map((h) => (
                <tr key={h.sessionId}>
                  <td>{h.startedAt}</td>
                  <td>{h.agentId}</td>
                  <td>
                    {h.status === "completed" ? "✅ 完了" : h.status === "failed" ? "❌ 失敗" : "⏹ 中断"}
                  </td>
                  <td>{Math.round(h.durationMs / 1000)} 秒</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </footer>
    </div>
  );
}
