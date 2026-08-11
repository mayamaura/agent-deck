import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  AgentDefinitionDto,
  AgentSettings,
  AgentSummary,
  AppConfigDto,
  AppEvent,
  HistoryEntry,
  SyncSummary,
} from "./types";
import { EVENT_CHANNEL } from "./types";
import { buildTree } from "./tree";
import type { AgentRow, TreeState } from "./tree";

// 3 ペイン構成の骨格(docs/requirements.md §3.1)。
// 実行ビューはツリー表示(docs/requirements.md §3.3。このアプリの主目的)。
// 生イベントの時系列ログ・トークン使用量・出力ファイルは詳細ペインへ移設。

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

/** エージェント定義エディタ(個人スコープ)の編集用状態(docs/roadmap.md v0.2)。 */
interface DefinitionFormState {
  name: string;
  description: string;
  model: string;
  tools: string;
  body: string;
}

const EMPTY_DEF_FORM: DefinitionFormState = { name: "", description: "", model: "", tools: "", body: "" };

function splitList(text: string): string[] {
  return text
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/** イベント 1 件を短い日本語要約にする(詳細ペインの時系列ログ用)。 */
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

const STATUS_LABEL: Record<AgentRow["status"], string> = {
  running: "▶ 実行中",
  completed: "✅ 完了",
  failed: "❌ 失敗",
  cancelled: "⏹ 中断",
};

const TOOL_STATUS_LABEL: Record<"running" | "ok" | "failed", string> = {
  running: "実行中",
  ok: "完了",
  failed: "失敗",
};

function formatDuration(ms: number): string {
  const totalSeconds = Math.max(0, Math.round(ms / 1000));
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

/**
 * 行の経過/確定時間(ミリ秒)。AppEvent には壁時計の終了時刻が無いため、
 * 実行中は rowStartedAt(App.tsx が受信時刻で記録)と nowTick(1秒毎に更新)から
 * 計算する。完了済みで確定値(サブエージェントの durationMs)があればそれを使う。
 * 確定値が無い完了行(メイン、または duration_ms を持たない失敗)は、running が
 * false になった時点で nowTick の更新も止まる(App.tsx 側)ため、最後の tick 値が
 * そのまま確定値として表示される。
 */
function elapsedMsFor(row: AgentRow, rowStartedAt: Record<string, number>, nowTick: number): number | null {
  if (row.status !== "running" && row.durationMs != null) return row.durationMs;
  const start = rowStartedAt[row.key];
  return start != null ? nowTick - start : null;
}

function AgentRowView({
  row,
  elapsedMs,
  isSub,
  onRespond,
}: {
  row: AgentRow;
  elapsedMs: number | null;
  isSub: boolean;
  onRespond: (requestId: string, decision: boolean) => void;
}) {
  return (
    <div className={isSub ? "agent-row sub" : "agent-row"}>
      <div className="agent-row-header">
        <span className="tree-marker">▼</span>
        <strong>
          {row.label}
          {isSub && " (サブ)"}
        </strong>
        <span className={`status-badge status-${row.status}`}>{STATUS_LABEL[row.status]}</span>
        {elapsedMs != null && <span className="muted">{formatDuration(elapsedMs)}</span>}
        {row.status !== "running" && row.totalTokens != null && (
          <span className="muted">{row.totalTokens} tokens</span>
        )}
      </div>
      {row.currentIntent && <p className="agent-intent">現在: {row.currentIntent}</p>}
      {row.error && <p className="error">⚠ {row.error}</p>}
      {row.tools.length > 0 && (
        <ul className="tool-list">
          {row.tools.map((t) => (
            <li key={t.toolCallId}>
              🔧 {t.toolName} — {TOOL_STATUS_LABEL[t.status]}
            </li>
          ))}
        </ul>
      )}
      {row.pendingPermissions.map((p) => (
        <div key={p.requestId} className="permission-dialog">
          <span>⚠ 承認が必要: {p.detail}</span>
          <button type="button" onClick={() => onRespond(p.requestId, true)}>
            承認
          </button>
          <button type="button" onClick={() => onRespond(p.requestId, false)}>
            拒否
          </button>
        </div>
      ))}
    </div>
  );
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

  // エージェント定義エディタ(docs/roadmap.md v0.2)。
  const [definition, setDefinition] = useState<AgentDefinitionDto | null>(null);
  const [definitionError, setDefinitionError] = useState<string | null>(null);
  const [defForm, setDefForm] = useState<DefinitionFormState>(EMPTY_DEF_FORM);
  const [defSaveStatus, setDefSaveStatus] = useState<string | null>(null);
  const [newAgentId, setNewAgentId] = useState("");
  const [newAgentError, setNewAgentError] = useState<string | null>(null);

  // 共有設定(docs/roadmap.md v0.2、決定ログ 2026-08-12: 共有フォルダ方式)。
  const [appConfig, setAppConfig] = useState<AppConfigDto | null>(null);
  const [sharedSourceInput, setSharedSourceInput] = useState("");
  const [syncResult, setSyncResult] = useState<SyncSummary | null>(null);
  const [syncError, setSyncError] = useState<string | null>(null);

  // 応答済みの権限要求 requestId(docs/tree.ts の buildTree 第2引数)。
  const [respondedRequestIds, setRespondedRequestIds] = useState<Set<string>>(new Set());
  const [outputFolderError, setOutputFolderError] = useState<string | null>(null);
  // 行ごとの経過時間計算用の開始時刻(受信時刻, epoch ms)。key は tree.ts の AgentRow.key。
  const [rowStartedAt, setRowStartedAt] = useState<Record<string, number>>({});
  const [nowTick, setNowTick] = useState(() => Date.now());

  async function reloadAgents() {
    try {
      setAgents(await invoke<AgentSummary[]>("list_agents"));
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    reloadAgents();
    invoke<HistoryEntry[]>("list_history", { limit: 20 })
      .then(setHistory)
      .catch((e) => setError(String(e)));
    invoke<AppConfigDto>("get_app_config")
      .then((c) => {
        setAppConfig(c);
        setSharedSourceInput(c.sharedAgentsSource ?? "");
      })
      .catch((e) => setError(String(e)));
  }, []);

  // 選択中エージェントの定義(個人=編集可、共有=読み取り専用。docs/roadmap.md v0.2)。
  // 個人優先で解決された1件を返す(get_agent_definition の仕様)。
  useEffect(() => {
    if (!selected) {
      setDefinition(null);
      return;
    }
    setDefinitionError(null);
    invoke<AgentDefinitionDto>("get_agent_definition", { agentId: selected })
      .then((d) => {
        setDefinition(d);
        setDefForm({
          name: d.name,
          description: d.description,
          model: d.model ?? "",
          tools: (d.tools ?? []).join(", "),
          body: d.body,
        });
        setDefSaveStatus(null);
      })
      .catch((e) => setDefinitionError(String(e)));
  }, [selected]);

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
  // 実行ビューはタスクごとに新しくなるため、taskStarted でログ・応答済み集合・
  // 経過時間の起点をリセットする。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<AppEvent>(EVENT_CHANNEL, ({ payload: ev }) => {
      const logged: LoggedEvent = { time: new Date().toLocaleTimeString(), event: ev };
      setEvents((prev) => (ev.kind === "taskStarted" ? [logged] : [...prev, logged]));

      if (ev.kind === "taskStarted") {
        setRunning(true);
        setSessionId(ev.sessionId);
        setRespondedRequestIds(new Set());
        setRowStartedAt({ main: Date.now() });
        setNowTick(Date.now());
      } else if (ev.kind === "subagentStarted") {
        setRowStartedAt((prev) => ({ ...prev, [ev.agentId]: Date.now() }));
      } else if (ev.kind === "taskCompleted" || ev.kind === "taskFailed" || ev.kind === "taskCancelled") {
        setRunning(false);
        // 履歴ペインをこのタスクの結果で更新する(docs/requirements.md 受け入れ条件10)。
        invoke<HistoryEntry[]>("list_history", { limit: 20 })
          .then(setHistory)
          .catch((e) => setError(String(e)));
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  // 経過時間の表示更新(実行中は1秒ごと。running が false になると自動的に止まり、
  // その時点の nowTick が「確定値」として残る)。
  useEffect(() => {
    if (!running) return;
    const id = setInterval(() => setNowTick(Date.now()), 1000);
    return () => clearInterval(id);
  }, [running]);

  const tree: TreeState = buildTree(
    events.map((e) => e.event),
    respondedRequestIds,
  );

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

  async function handleCreateAgent() {
    const id = newAgentId.trim();
    if (!id) return;
    setNewAgentError(null);
    try {
      await invoke("create_agent_definition", {
        agentId: id,
        name: id,
        description: "",
        tools: null,
        model: null,
        body: "",
      });
      setNewAgentId("");
      await reloadAgents();
      setSelected(id);
    } catch (e) {
      setNewAgentError(String(e));
    }
  }

  async function handleSaveDefinition() {
    if (!selected) return;
    setDefSaveStatus(null);
    try {
      await invoke("save_agent_definition", {
        agentId: selected,
        name: defForm.name,
        description: defForm.description,
        tools: defForm.tools.trim() ? splitList(defForm.tools) : null,
        model: defForm.model.trim() || null,
        body: defForm.body,
      });
      setDefSaveStatus("✅ 保存しました");
      await reloadAgents();
    } catch (e) {
      setDefSaveStatus(`⚠ 保存に失敗しました: ${e}`);
    }
  }

  async function handleDeleteDefinition() {
    if (!selected) return;
    if (!window.confirm(`エージェント定義「${selected}」を削除しますか?`)) return;
    try {
      await invoke("delete_agent_definition", { agentId: selected });
      setSelected(null);
      await reloadAgents();
    } catch (e) {
      setDefinitionError(String(e));
    }
  }

  async function handleDuplicateAgent() {
    if (!selected) return;
    setDefinitionError(null);
    try {
      await invoke("duplicate_agent", { agentId: selected });
      await reloadAgents();
      // 複製後は個人優先で解決され直すため、編集可能な個人版を再取得する。
      const d = await invoke<AgentDefinitionDto>("get_agent_definition", { agentId: selected });
      setDefinition(d);
      setDefForm({
        name: d.name,
        description: d.description,
        model: d.model ?? "",
        tools: (d.tools ?? []).join(", "),
        body: d.body,
      });
    } catch (e) {
      setDefinitionError(String(e));
    }
  }

  async function handlePickSharedSource() {
    const dir = await open({ directory: true });
    if (typeof dir === "string") setSharedSourceInput(dir);
  }

  async function handleSaveSharedSource() {
    setSyncError(null);
    try {
      const path = sharedSourceInput.trim() || null;
      await invoke("save_shared_agents_source", { path });
      setAppConfig((c) => (c ? { ...c, sharedAgentsSource: path } : c));
    } catch (e) {
      setSyncError(String(e));
    }
  }

  async function handleSyncSharedAgents() {
    setSyncError(null);
    try {
      const result = await invoke<SyncSummary>("sync_shared_agents_cmd");
      setSyncResult(result);
      await reloadAgents();
    } catch (e) {
      setSyncError(String(e));
    }
  }

  async function handleOpenOutputFolder() {
    if (!selected) return;
    setOutputFolderError(null);
    try {
      await invoke("open_output_folder", { agentId: selected });
    } catch (e) {
      setOutputFolderError(String(e));
    }
  }

  async function respondPermission(requestId: string, decision: boolean) {
    try {
      await invoke("respond_permission", { requestId, decision });
      setRespondedRequestIds((prev) => new Set(prev).add(requestId));
    } catch (e) {
      setRunError(String(e));
    }
  }

  return (
    <div className="layout">
      <aside className="pane agents">
        <h2>エージェント</h2>
        {error && <p className="error">⚠ {error}</p>}
        <div className="new-agent-row">
          <input
            type="text"
            placeholder="新規エージェント ID"
            value={newAgentId}
            onChange={(e) => setNewAgentId(e.target.value)}
          />
          <button type="button" onClick={handleCreateAgent} disabled={!newAgentId.trim()}>
            新規作成
          </button>
        </div>
        {newAgentError && <p className="error">⚠ {newAgentError}</p>}
        {agents.length === 0 && !error && <p className="muted">定義がありません</p>}
        <ul>
          {agents.map((a) => {
            const c = configs[a.id];
            const unset = !c || !c.inputDir || !c.outputDir;
            return (
              <li key={`${a.id}:${a.scope}`}>
                <button
                  className={selected === a.id ? "selected" : ""}
                  onClick={() => setSelected(a.id)}
                >
                  <strong>
                    {a.name}
                    <span className={`badge scope-${a.scope}`}>
                      {a.scope === "shared" ? "🌐 共有" : "👤 個人"}
                    </span>
                    {a.version && <span className="muted">v{a.version}</span>}
                    {a.shadowed && <span className="badge">個人版あり</span>}
                    {unset && <span className="badge">⚠ 未設定</span>}
                  </strong>
                  <span className="muted">{a.description}</span>
                </button>
              </li>
            );
          })}
        </ul>
        {selected && definition && (
          <div className="definition-editor">
            <h3>定義({definition.scope === "shared" ? "共有・読み取り専用" : "個人"})</h3>
            {definition.scope === "shared" ? (
              <>
                <p className="muted">name: {definition.name}</p>
                <p className="muted">description: {definition.description}</p>
                <p className="muted">model: {definition.model ?? "(未指定)"}</p>
                <p className="muted">tools: {definition.tools ? definition.tools.join(", ") : "(全ツール)"}</p>
                <pre className="definition-body">{definition.body}</pre>
                <div className="run-controls">
                  <button type="button" onClick={handleDuplicateAgent}>
                    複製して編集
                  </button>
                </div>
              </>
            ) : (
              <>
                <label>
                  名前
                  <input
                    type="text"
                    value={defForm.name}
                    onChange={(e) => setDefForm((f) => ({ ...f, name: e.target.value }))}
                  />
                </label>
                <label>
                  説明
                  <input
                    type="text"
                    value={defForm.description}
                    onChange={(e) => setDefForm((f) => ({ ...f, description: e.target.value }))}
                  />
                </label>
                <label>
                  モデル(空欄でアプリ既定)
                  <input
                    type="text"
                    value={defForm.model}
                    onChange={(e) => setDefForm((f) => ({ ...f, model: e.target.value }))}
                  />
                </label>
                <label>
                  ツール(カンマ区切り。空欄で全ツール)
                  <input
                    type="text"
                    value={defForm.tools}
                    onChange={(e) => setDefForm((f) => ({ ...f, tools: e.target.value }))}
                  />
                </label>
                <label>
                  本文(Instructions)
                  <textarea
                    className="prompt-input"
                    rows={6}
                    value={defForm.body}
                    onChange={(e) => setDefForm((f) => ({ ...f, body: e.target.value }))}
                  />
                </label>
                <div className="run-controls">
                  <button type="button" onClick={handleSaveDefinition}>
                    保存
                  </button>
                  <button type="button" onClick={handleDeleteDefinition}>
                    削除
                  </button>
                </div>
                {defSaveStatus && <p className="muted">{defSaveStatus}</p>}
              </>
            )}
          </div>
        )}
        {definitionError && <p className="error">⚠ {definitionError}</p>}
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
        <div className="shared-settings">
          <h3>共有設定</h3>
          <label>
            共有元フォルダ
            <div className="folder-row">
              <input
                type="text"
                value={sharedSourceInput}
                onChange={(e) => setSharedSourceInput(e.target.value)}
              />
              <button type="button" onClick={handlePickSharedSource}>
                選択...
              </button>
            </div>
          </label>
          <div className="run-controls">
            <button type="button" onClick={handleSaveSharedSource}>
              保存
            </button>
            <button type="button" onClick={handleSyncSharedAgents} disabled={!appConfig?.sharedAgentsSource}>
              同期
            </button>
          </div>
          {syncError && <p className="error">⚠ {syncError}</p>}
          {syncResult && (
            <p className="muted">
              同期完了({syncResult.syncedAt}): 追加{syncResult.added} / 更新{syncResult.updated} / 削除
              {syncResult.removed}
            </p>
          )}
        </div>
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
            {tree.main && (
              <div className="agent-tree">
                <AgentRowView
                  row={tree.main}
                  elapsedMs={elapsedMsFor(tree.main, rowStartedAt, nowTick)}
                  isSub={false}
                  onRespond={respondPermission}
                />
                {tree.subagents.map((sub) => (
                  <div className="sub-indent" key={sub.key}>
                    <AgentRowView
                      row={sub}
                      elapsedMs={elapsedMsFor(sub, rowStartedAt, nowTick)}
                      isSub
                      onRespond={respondPermission}
                    />
                  </div>
                ))}
              </div>
            )}
            {tree.taskStatus === "completed" && tree.summary && (
              <div className="task-summary">
                <h3>結果</h3>
                <p>{tree.summary}</p>
              </div>
            )}
          </>
        ) : (
          <p className="muted">左の一覧からエージェントを選択してください</p>
        )}
      </main>
      <aside className="pane detail">
        <h2>詳細</h2>
        {tree.usage && (
          <p className="muted">
            トークン使用量: {tree.usage.currentTokens}
            {tree.usage.tokenLimit != null ? ` / ${tree.usage.tokenLimit}` : ""}
          </p>
        )}
        {tree.outputFiles.length > 0 && (
          <div>
            <h3>出力ファイル</h3>
            <ul>
              {tree.outputFiles.map((f) => (
                <li key={f}>{f}</li>
              ))}
            </ul>
          </div>
        )}
        {selected && (
          <div className="run-controls">
            <button type="button" onClick={handleOpenOutputFolder}>
              出力フォルダを開く
            </button>
          </div>
        )}
        {outputFolderError && <p className="error">⚠ {outputFolderError}</p>}
        <h3>ログ</h3>
        <ul className="event-log">
          {events.map((e, i) => (
            <li key={i} className={e.event.kind === "taskFailed" ? "error" : undefined}>
              <span className="muted">{e.time}</span> [{e.event.kind}] {summarize(e.event)}
            </li>
          ))}
        </ul>
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
                <th>トークン</th>
              </tr>
            </thead>
            <tbody>
              {history.map((h) => (
                <tr key={h.sessionId}>
                  <td>{h.startedAt}</td>
                  <td>{h.agentId}</td>
                  <td>
                    {h.status === "completed" ? "✅ 完了" : h.status === "failed" ? "❌ 失敗" : "⏹ 中断"}
                    {/* 完成品か途中で止まったものかを判別できるようにする(docs/architecture.md §8.3)。 */}
                    {h.status !== "completed" && <div className="muted">⚠ 出力は未完成の可能性</div>}
                  </td>
                  <td>{Math.round(h.durationMs / 1000)} 秒</td>
                  <td>{h.totalTokens != null ? h.totalTokens : "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </footer>
    </div>
  );
}
