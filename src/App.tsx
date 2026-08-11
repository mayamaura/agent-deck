import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AgentSummary, AppEvent, HistoryEntry } from "./types";
import { EVENT_CHANNEL } from "./types";

// 3 ペイン構成の骨格(docs/requirements.md §3.1)。
// 実行ビューのツリー表示はステップ5、設定画面はステップ4で実装する。
// このステップ(2)では受信したイベントを時系列でそのまま羅列表示する。

/** イベント 1 件を短い日本語要約にする(kind ごとの switch。ツリー化はステップ5)。 */
function summarize(ev: AppEvent): string {
  switch (ev.kind) {
    case "taskStarted":
      return `タスク開始(session: ${ev.sessionId})`;
    case "agentIntent":
      return `意図: ${ev.text}`;
    case "subagentStarted":
      return `サブエージェント開始: ${ev.displayName}`;
    case "subagentCompleted":
      return `サブエージェント完了: ${ev.agentId}(${Math.round(ev.durationMs / 1000)}秒${
        ev.totalTokens != null ? `, ${ev.totalTokens} tokens` : ""
      })`;
    case "subagentFailed":
      return `サブエージェント失敗: ${ev.agentId} — ${ev.error}`;
    case "toolStarted":
      return `ツール実行開始: ${ev.toolName}`;
    case "toolCompleted":
      return `ツール実行${ev.success ? "成功" : "失敗"}: ${ev.toolName}`;
    case "permissionRequested":
      return `権限確認要求: ${ev.permissionKind} — ${ev.detail}`;
    case "usageUpdated":
      return `トークン使用量: ${ev.currentTokens}${ev.tokenLimit != null ? ` / ${ev.tokenLimit}` : ""}`;
    case "taskCompleted":
      return `タスク完了: ${ev.summary}`;
    case "taskFailed":
      return `タスク失敗: ${ev.error}`;
    case "taskCancelled":
      return "タスク中断";
  }
}

interface LoggedEvent {
  time: string;
  event: AppEvent;
}

export default function App() {
  const [agents, setAgents] = useState<AgentSummary[]>([]);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [prompt, setPrompt] = useState("");
  const [running, setRunning] = useState(false);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [runError, setRunError] = useState<string | null>(null);
  const [events, setEvents] = useState<LoggedEvent[]>([]);

  useEffect(() => {
    invoke<AgentSummary[]>("list_agents")
      .then(setAgents)
      .catch((e) => setError(String(e)));
    invoke<HistoryEntry[]>("list_history", { limit: 20 })
      .then(setHistory)
      .catch((e) => setError(String(e)));
  }, []);

  // タスク実行イベントの購読(docs/architecture.md §4: 単一チャネルを kind で判別)。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<AppEvent>(EVENT_CHANNEL, ({ payload: ev }) => {
      setEvents((prev) => [...prev, { time: new Date().toLocaleTimeString(), event: ev }]);
      // 実行中フラグは taskStarted / taskCompleted / taskFailed / taskCancelled から導出する。
      if (ev.kind === "taskStarted") {
        setRunning(true);
        setSessionId(ev.sessionId);
      } else if (ev.kind === "taskCompleted" || ev.kind === "taskFailed" || ev.kind === "taskCancelled") {
        setRunning(false);
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  async function handleRun() {
    if (!selected || !prompt.trim() || running) return;
    setRunError(null);
    try {
      await invoke("start_task", { agentId: selected, prompt });
    } catch (e) {
      setRunError(String(e));
    }
  }

  async function handleCancel() {
    if (!sessionId) return;
    try {
      await invoke("cancel_task", { sessionId });
    } catch (e) {
      setRunError(String(e));
    }
  }

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
          <>
            <p className="muted">{agents.find((a) => a.id === selected)?.description}</p>
            <textarea
              className="prompt-input"
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="依頼内容を入力してください"
              rows={3}
              disabled={running}
            />
            <div className="run-controls">
              <button onClick={handleRun} disabled={running || !prompt.trim()}>
                実行
              </button>
              {running && <button onClick={handleCancel}>中断</button>}
            </div>
            {runError && <p className="error">⚠ {runError}</p>}
            <ul className="event-log">
              {events.map((e, i) => (
                <li key={i} className={e.event.kind === "taskFailed" ? "error" : undefined}>
                  <span className="muted">{e.time}</span> [{e.event.kind}] {summarize(e.event)}
                </li>
              ))}
            </ul>
          </>
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
