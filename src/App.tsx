import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { AgentSettings, AgentSummary, AppEvent, HistoryEntry } from "./types";
import { EVENT_CHANNEL } from "./types";

// 3 ペイン構成の骨格(docs/requirements.md §3.1)。
// 実行ビューのツリー表示はステップ5で実装する。
// このステップ(4)では入出力設定フォームと権限確認ダイアログを追加する。

/** 設定フォームの編集用の状態。カンマ区切りテキストは保存時に配列へ分解する。 */
interface ConfigFormState {
  inputDir: string;
  outputDir: string;
  allowedTools: string;
  deniedTools: string;
  autoApprove: boolean;
}

const EMPTY_FORM: ConfigFormState = {
  inputDir: "",
  outputDir: "",
  allowedTools: "",
  deniedTools: "",
  autoApprove: true,
};

function splitList(text: string): string[] {
  return text
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

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

  // 選択中エージェントの入出力設定(docs/requirements.md §3.4)。
  const [configs, setConfigs] = useState<Record<string, AgentSettings | null>>({});
  const [form, setForm] = useState<ConfigFormState>(EMPTY_FORM);
  const [saveStatus, setSaveStatus] = useState<string | null>(null);

  // Ask になった権限要求のうち、まだ応答していないもの(docs/architecture.md §7.1)。
  const [pendingPermissions, setPendingPermissions] = useState<
    Record<string, { permissionKind: string; detail: string }>
  >({});

  useEffect(() => {
    invoke<AgentSummary[]>("list_agents")
      .then(setAgents)
      .catch((e) => setError(String(e)));
    invoke<HistoryEntry[]>("list_history", { limit: 20 })
      .then(setHistory)
      .catch((e) => setError(String(e)));
  }, []);

  // 一覧に出す「未設定」バッジのため、全エージェント分の設定をまとめて取得する
  // (get_agent_config は1件ずつしか取れないため並列 invoke。docs/requirements.md §3.2)。
  useEffect(() => {
    if (agents.length === 0) return;
    Promise.all(
      agents.map((a) =>
        invoke<AgentSettings | null>("get_agent_config", { agentId: a.id }).then(
          (c) => [a.id, c] as const,
        ),
      ),
    )
      .then((pairs) => setConfigs(Object.fromEntries(pairs)))
      .catch((e) => setError(String(e)));
  }, [agents]);

  // 選択中エージェントが変わったら、フォームへ現在の設定を読み込む。
  useEffect(() => {
    if (!selected) return;
    const c = configs[selected];
    setForm({
      inputDir: c?.inputDir ?? "",
      outputDir: c?.outputDir ?? "",
      allowedTools: c?.allowedTools.join(", ") ?? "",
      deniedTools: c?.deniedTools.join(", ") ?? "",
      autoApprove: c?.autoApproveWriteInOutputDir ?? true,
    });
    setSaveStatus(null);
  }, [selected, configs]);

  // タスク実行イベントの購読(docs/architecture.md §4: 単一チャネルを kind で判別)。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<AppEvent>(EVENT_CHANNEL, ({ payload: ev }) => {
      setEvents((prev) => [...prev, { time: new Date().toLocaleTimeString(), event: ev }]);
      // 実行中フラグは taskStarted / taskCompleted / taskFailed / taskCancelled から導出する。
      if (ev.kind === "taskStarted") {
        setRunning(true);
        setSessionId(ev.sessionId);
        setPendingPermissions({});
      } else if (ev.kind === "taskCompleted" || ev.kind === "taskFailed" || ev.kind === "taskCancelled") {
        setRunning(false);
      } else if (ev.kind === "permissionRequested") {
        setPendingPermissions((prev) => ({
          ...prev,
          [ev.requestId]: { permissionKind: ev.permissionKind, detail: ev.detail },
        }));
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

  async function handlePickFolder(target: "inputDir" | "outputDir") {
    const dir = await open({ directory: true });
    if (typeof dir === "string") {
      setForm((f) => ({ ...f, [target]: dir }));
    }
  }

  async function handleSaveConfig() {
    if (!selected) return;
    const settings: AgentSettings = {
      inputDir: form.inputDir.trim() || null,
      outputDir: form.outputDir.trim() || null,
      allowedTools: splitList(form.allowedTools),
      deniedTools: splitList(form.deniedTools),
      autoApproveWriteInOutputDir: form.autoApprove,
    };
    try {
      await invoke("save_agent_config", { agentId: selected, settings });
      setConfigs((prev) => ({ ...prev, [selected]: settings }));
      setSaveStatus("✅ 保存しました");
    } catch (e) {
      setSaveStatus(`⚠ 保存に失敗しました: ${e}`);
    }
  }

  async function respondPermission(requestId: string, decision: boolean) {
    try {
      await invoke("respond_permission", { requestId, decision });
      setPendingPermissions((prev) => {
        const next = { ...prev };
        delete next[requestId];
        return next;
      });
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
          {agents.map((a) => {
            const c = configs[a.id];
            const unset = !c || !c.inputDir || !c.outputDir;
            return (
              <li key={a.id}>
                <button
                  className={selected === a.id ? "selected" : ""}
                  onClick={() => setSelected(a.id)}
                >
                  <strong>
                    {a.name}
                    {unset && <span className="badge">⚠ 未設定</span>}
                  </strong>
                  <span className="muted">{a.description}</span>
                </button>
              </li>
            );
          })}
        </ul>
        {selected && (
          <div className="config-form">
            <h3>入出力設定</h3>
            <label>
              入力フォルダ
              <div className="folder-row">
                <input
                  type="text"
                  value={form.inputDir}
                  onChange={(e) => setForm((f) => ({ ...f, inputDir: e.target.value }))}
                />
                <button type="button" onClick={() => handlePickFolder("inputDir")}>
                  選択...
                </button>
              </div>
            </label>
            <label>
              出力フォルダ
              <div className="folder-row">
                <input
                  type="text"
                  value={form.outputDir}
                  onChange={(e) => setForm((f) => ({ ...f, outputDir: e.target.value }))}
                />
                <button type="button" onClick={() => handlePickFolder("outputDir")}>
                  選択...
                </button>
              </div>
            </label>
            <label>
              許可ツール(カンマ区切り。例: write, shell(python:*))
              <input
                type="text"
                value={form.allowedTools}
                onChange={(e) => setForm((f) => ({ ...f, allowedTools: e.target.value }))}
              />
            </label>
            <label>
              拒否ツール(カンマ区切り。例: shell(rm))
              <input
                type="text"
                value={form.deniedTools}
                onChange={(e) => setForm((f) => ({ ...f, deniedTools: e.target.value }))}
              />
            </label>
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={form.autoApprove}
                onChange={(e) => setForm((f) => ({ ...f, autoApprove: e.target.checked }))}
              />
              出力フォルダへの書き込みを自動承認する
            </label>
            <div className="run-controls">
              <button type="button" onClick={handleSaveConfig}>
                保存
              </button>
            </div>
            {saveStatus && <p className="muted">{saveStatus}</p>}
          </div>
        )}
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
              {events.map((e, i) => {
                const ev = e.event;
                const requestId = ev.kind === "permissionRequested" ? ev.requestId : undefined;
                const pending = requestId ? pendingPermissions[requestId] : undefined;
                return (
                  <li key={i} className={ev.kind === "taskFailed" ? "error" : undefined}>
                    <span className="muted">{e.time}</span> [{ev.kind}] {summarize(ev)}
                    {pending && requestId && (
                      <div className="permission-dialog">
                        <span>⚠ 承認が必要: {pending.detail}</span>
                        <button type="button" onClick={() => respondPermission(requestId, true)}>
                          承認
                        </button>
                        <button type="button" onClick={() => respondPermission(requestId, false)}>
                          拒否
                        </button>
                      </div>
                    )}
                  </li>
                );
              })}
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
