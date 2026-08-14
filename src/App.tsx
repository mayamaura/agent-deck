import { useEffect, useState, type CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  AgentSettings,
  AgentSummary,
  AppConfigDto,
  AppEvent,
  HistoryEntry,
  QueueStatusDto,
  Recurrence,
  Schedule,
  SyncSummary,
  UpdateInfoDto,
} from "./types";
import { EVENT_CHANNEL } from "./types";
import { buildTree } from "./tree";
import type { AgentRow, TreeState } from "./tree";
import { sessionSummary } from "./sessions";
import { AGENTS_CHANGED } from "./AgentEditor";

// 実行ビュー上部のダッシュボード帯・タブに残す「直近の完了/失敗/中断セッション」件数の上限
// (docs/roadmap.md v0.5: 実行中+直近数件のセッション)。実行中セッションは件数に含めない。
const MAX_RECENT_FINISHED_SESSIONS = 5;

const NO_RESPONSES = new Set<string>();

// 3 ペイン構成の骨格(docs/requirements.md §3.1)。
// 実行ビューはツリー表示(docs/requirements.md §3.3。このアプリの主目的)。
// 生イベントの時系列ログ・トークン使用量・出力ファイルは詳細ペインへ移設。

/** スケジュール追加・編集フォームの状態(docs/roadmap.md v0.4)。周期の種類ごとの
 * フィールド(weekday/day)は保存時に選択中の type のものだけを Recurrence へ詰める。 */
interface ScheduleFormState {
  id: string | null;
  agentId: string;
  prompt: string;
  type: Recurrence["type"];
  time: string;
  weekday: number;
  day: number;
  enabled: boolean;
  lastRunAt: string | null;
}

const EMPTY_SCHEDULE_FORM: ScheduleFormState = {
  id: null,
  agentId: "",
  prompt: "",
  type: "daily",
  time: "09:00",
  weekday: 1,
  day: 1,
  enabled: true,
  lastRunAt: null,
};

const WEEKDAY_LABELS = ["日", "月", "火", "水", "木", "金", "土"];

function formatRecurrence(r: Recurrence): string {
  switch (r.type) {
    case "daily":
      return `毎日 ${r.time}`;
    case "weekly":
      return `毎週${WEEKDAY_LABELS[r.weekday] ?? "?"}曜 ${r.time}`;
    case "monthly":
      return `毎月${r.day}日 ${r.time}`;
  }
}

const TRIGGER_LABEL: Record<HistoryEntry["trigger"], string> = {
  manual: "🖐 手動",
  scheduled: "⏰ 定期",
};

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
    case "allowRuleAdded":
      return `常に許可を登録しました: ${ev.pattern}`;
    case "userInputRequested":
      return `質問: ${ev.question}`;
  }
}

interface LoggedEvent {
  time: string;
  event: AppEvent;
}

/** セッション 1 本分のイベント束(docs/roadmap.md v0.5: 並行実行)。実行ビューのタブ 1 枚に対応する。
 * respondedRequestIds はセッションごとに持つ(承認ダイアログはタブに紐づく)。 */
interface SessionState {
  agentId: string;
  events: LoggedEvent[];
  rowStartedAt: Record<string, number>;
  respondedRequestIds: Set<string>;
}

function sessionElapsedMs(rowStartedAt: Record<string, number>, nowTick: number): number | null {
  const start = rowStartedAt.main;
  return start != null ? nowTick - start : null;
}

/** 実行中でないセッションのうち古いものから間引く(タブ・ダッシュボードの肥大化防止)。
 * 実行中セッションは対象外(件数上限を超えても常に表示する)。 */
function pruneOldFinishedSessions(sessions: Record<string, SessionState>): Record<string, SessionState> {
  const finishedIds = Object.keys(sessions).filter(
    (sid) => sessionSummary(sessions[sid].events.map((e) => e.event)).status !== "running",
  );
  const excess = finishedIds.length - MAX_RECENT_FINISHED_SESSIONS;
  if (excess <= 0) return sessions;
  const next = { ...sessions };
  for (const sid of finishedIds.slice(0, excess)) delete next[sid];
  return next;
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

/** エージェント名から決まる色相(0-359)。同じエージェントは毎回同じ顔色になり、
 * 一覧・実行ツリー・ダッシュボードのどこで見ても「同じ人」だと分かる。 */
function agentHue(name: string): number {
  let h = 0;
  for (const ch of name) h = (h * 31 + (ch.codePointAt(0) ?? 0)) % 360;
  return h;
}

/** エージェントの顔。status を渡すと実行中はリングが脈打つ(生きている感じを出す)。 */
function Avatar({ name, status }: { name: string; status?: AgentRow["status"] }) {
  return (
    <span
      className={status ? `avatar avatar-${status}` : "avatar"}
      style={{ "--hue": agentHue(name) } as CSSProperties}
      aria-hidden
    >
      {[...name][0] ?? "?"}
    </span>
  );
}

/** 承認ダイアログの3択(docs/architecture.md §7.1 拡張)。respond_permission コマンドの
 * decision 引数と同じ文字列(Rust 側で copilot::PermissionReply に変換される)。 */
type PermissionDecision = "approveOnce" | "approveAlways" | "deny";

/** ask_user への回答カード(v1.0 経路A)。choices があれば選択ボタン、allowFreeform なら
 * 自由入力欄+送信ボタンも出す。「回答しない」は常に出す(answer=null で respond_user_input へ)。 */
function UserInputRowView({
  q,
  onRespond,
}: {
  q: AgentRow["pendingUserInputs"][number];
  onRespond: (requestId: string, answer: string | null) => void;
}) {
  const [freeform, setFreeform] = useState("");
  return (
    <div className="permission-dialog">
      <span>❓ 質問: {q.question}</span>
      {q.choices.map((choice) => (
        <button type="button" key={choice} onClick={() => onRespond(q.requestId, choice)}>
          {choice}
        </button>
      ))}
      {q.allowFreeform && (
        <>
          <input
            type="text"
            value={freeform}
            onChange={(e) => setFreeform(e.target.value)}
            placeholder="自由回答"
          />
          <button type="button" onClick={() => onRespond(q.requestId, freeform)} disabled={!freeform.trim()}>
            送信
          </button>
        </>
      )}
      <button type="button" onClick={() => onRespond(q.requestId, null)}>
        回答しない
      </button>
    </div>
  );
}

function AgentRowView({
  row,
  elapsedMs,
  isSub,
  onRespond,
  onRespondUserInput,
}: {
  row: AgentRow;
  elapsedMs: number | null;
  isSub: boolean;
  onRespond: (requestId: string, decision: PermissionDecision) => void;
  onRespondUserInput: (requestId: string, answer: string | null) => void;
}) {
  return (
    <div className={isSub ? "agent-row sub" : "agent-row"}>
      <div className="agent-row-header">
        <Avatar name={row.label} status={row.status} />
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
          <button type="button" onClick={() => onRespond(p.requestId, "approveOnce")}>
            今回のみ承認
          </button>
          {p.suggestedPattern && (
            <button type="button" onClick={() => onRespond(p.requestId, "approveAlways")}>
              常に許可(このエージェント)
            </button>
          )}
          <button type="button" onClick={() => onRespond(p.requestId, "deny")}>
            拒否
          </button>
          {p.suggestedPattern && <span className="muted">以後 {p.suggestedPattern} を自動承認します</span>}
        </div>
      ))}
      {row.pendingUserInputs.map((q) => (
        <UserInputRowView key={q.requestId} q={q} onRespond={onRespondUserInput} />
      ))}
    </div>
  );
}

/**
 * エージェント 1 件の設定ウインドウ(定義+入出力設定)を開く。頻度の低い設定を
 * 一覧ペインから追い出すため別ウインドウにしている(ユーザー要望)。
 * 既に開いていれば作り直さず前面に出す。
 */
async function openAgentEditor(agentId: string, onError: (message: string) => void) {
  // ponytail: ラベルに使えない文字は _ に潰すだけ。別 ID が同じラベルに衝突すると
  // 既存ウインドウが前面に出る。実運用の ID(英数字+ハイフン)では起きないので放置。
  const label = `agent-editor-${agentId.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
  const existing = await WebviewWindow.getByLabel(label);
  if (existing) {
    await existing.setFocus();
    return;
  }
  const w = new WebviewWindow(label, {
    url: `index.html?agent=${encodeURIComponent(agentId)}`,
    title: `エージェント設定 — ${agentId}`,
    width: 560,
    height: 900,
  });
  w.once("tauri://error", (e) => onError(`設定ウインドウを開けませんでした: ${JSON.stringify(e.payload)}`));
}

export default function App() {
  const [agents, setAgents] = useState<AgentSummary[]>([]);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [prompt, setPrompt] = useState("");
  // セッション別のイベント束(docs/roadmap.md v0.5: 並行実行)。キーは sessionId。
  const [sessions, setSessions] = useState<Record<string, SessionState>>({});
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [runError, setRunError] = useState<string | null>(null);

  // 一覧の「未設定」バッジ用。編集自体は設定ウインドウ(AgentEditor.tsx)で行う。
  const [configs, setConfigs] = useState<Record<string, AgentSettings | null>>({});

  const [newAgentId, setNewAgentId] = useState("");
  const [newAgentError, setNewAgentError] = useState<string | null>(null);

  // 共有設定(docs/roadmap.md v0.2、決定ログ 2026-08-12: 共有フォルダ方式)。
  const [appConfig, setAppConfig] = useState<AppConfigDto | null>(null);
  const [sharedSourceInput, setSharedSourceInput] = useState("");
  const [syncResult, setSyncResult] = useState<SyncSummary | null>(null);
  const [syncError, setSyncError] = useState<string | null>(null);

  // アプリ本体の更新通知(docs/roadmap.md v0.3.0: 検知と通知のみ)。
  const [updateInfo, setUpdateInfo] = useState<UpdateInfoDto | null>(null);
  const [updateDismissed, setUpdateDismissed] = useState(false);
  const [updateSourceInput, setUpdateSourceInput] = useState("");
  const [updateCheckError, setUpdateCheckError] = useState<string | null>(null);
  const [openUpdateFolderError, setOpenUpdateFolderError] = useState<string | null>(null);

  // スケジュール管理(docs/roadmap.md v0.4)。
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const [scheduleForm, setScheduleForm] = useState<ScheduleFormState>(EMPTY_SCHEDULE_FORM);
  const [scheduleError, setScheduleError] = useState<string | null>(null);
  const [scheduleSaveStatus, setScheduleSaveStatus] = useState<string | null>(null);
  const [queueStatus, setQueueStatus] = useState<QueueStatusDto | null>(null);

  // タスク完了後の追い返信(v1.0 経路B)。タブを切り替えたら前のタブの下書きは持ち越さない。
  const [replyMessage, setReplyMessage] = useState("");
  const [replyError, setReplyError] = useState<string | null>(null);

  const [outputFolderError, setOutputFolderError] = useState<string | null>(null);
  // 監査ログフォルダを開くボタンのエラー表示用(docs/roadmap.md v0.6)。
  const [logsFolderError, setLogsFolderError] = useState<string | null>(null);
  // 経過時間表示の現在時刻(1秒毎に更新。全セッション共通)。
  const [nowTick, setNowTick] = useState(() => Date.now());

  async function reloadAgents() {
    try {
      const list = await invoke<AgentSummary[]>("list_agents");
      setAgents(list);
      // 削除・ID 変更で選択中の ID が消えることがある。残すと実行時に「見つかりません」になる。
      setSelected((cur) => (cur && list.some((a) => a.id === cur) ? cur : null));
    } catch (e) {
      setError(String(e));
    }
  }

  async function reloadSchedules() {
    try {
      setSchedules(await invoke<Schedule[]>("list_schedules"));
    } catch (e) {
      setScheduleError(String(e));
    }
  }

  async function reloadQueueStatus() {
    try {
      setQueueStatus(await invoke<QueueStatusDto>("get_queue_status"));
    } catch (e) {
      // 待機件数表示はベストエフォート(取得失敗を UI のエラー欄で騒がない)。
      console.error("待機中スケジュール件数の取得に失敗しました:", e);
    }
  }

  useEffect(() => {
    reloadAgents();
    reloadSchedules();
    reloadQueueStatus();
    invoke<HistoryEntry[]>("list_history", { limit: 20 })
      .then(setHistory)
      .catch((e) => setError(String(e)));
    invoke<AppConfigDto>("get_app_config")
      .then((c) => {
        setAppConfig(c);
        setSharedSourceInput(c.sharedAgentsSource ?? "");
        setUpdateSourceInput(c.updateSource ?? "");
      })
      .catch((e) => setError(String(e)));
    // 起動時の更新確認(docs/roadmap.md v0.3.0)。失敗しても静かに(UI には出さず console.error のみ)。
    invoke<UpdateInfoDto | null>("check_for_updates")
      .then(setUpdateInfo)
      .catch((e) => console.error("更新確認に失敗しました:", e));
  }, []);

  // 設定ウインドウでの保存・削除を一覧へ反映する(別 webview なので状態は共有されない)。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen(AGENTS_CHANGED, () => reloadAgents()).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
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

  // タスク実行イベントの購読(docs/architecture.md §4: 単一チャネルを kind で判別)。
  // sessionId ごとにセッションを束ねる(docs/roadmap.md v0.5: 並行実行)。taskStarted は
  // 新しい sessionId で来るため、既存セッションを壊さず新しいタブが増える形になる。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<AppEvent>(EVENT_CHANNEL, ({ payload: ev }) => {
      const logged: LoggedEvent = { time: new Date().toLocaleTimeString(), event: ev };
      const sid = ev.sessionId;
      // allowRuleAdded 用: このセッションが属するエージェント id(setSessions のアップデータ内
      // でしか prev[sid] を安全に読めない ── このエフェクトは deps [] で購読しているため、
      // クロージャ内の `sessions` はマウント時点のまま古くなる)。
      let allowRuleAgentId: string | null = null;

      setSessions((prev) => {
        const base: SessionState = prev[sid] ?? {
          agentId: "",
          events: [],
          rowStartedAt: {},
          respondedRequestIds: new Set(),
        };
        if (ev.kind === "allowRuleAdded") allowRuleAgentId = base.agentId || null;
        const next: SessionState = { ...base, events: [...base.events, logged] };
        if (ev.kind === "taskStarted") {
          next.agentId = ev.agentId;
          next.rowStartedAt = { main: Date.now() };
        } else if (ev.kind === "subagentStarted") {
          next.rowStartedAt = { ...next.rowStartedAt, [ev.agentId]: Date.now() };
        }
        return { ...prev, [sid]: next };
      });

      if (ev.kind === "allowRuleAdded" && allowRuleAgentId) {
        // 一覧の「未設定」バッジ判定を実態(agents.json への永続化)とずれさせない。
        const targetAgentId = allowRuleAgentId;
        const pattern = ev.pattern;
        setConfigs((prev) => {
          const current = prev[targetAgentId] ?? null;
          const allowedTools = current?.allowedTools ?? [];
          if (allowedTools.includes(pattern)) return prev;
          const updated: AgentSettings = current
            ? { ...current, allowedTools: [...allowedTools, pattern] }
            : {
                inputDir: null,
                outputDir: null,
                allowedTools: [pattern],
                deniedTools: [],
                autoApproveWriteInOutputDir: true,
              };
          return { ...prev, [targetAgentId]: updated };
        });
      }

      if (ev.kind === "taskStarted") {
        setActiveSessionId(sid);
        setNowTick(Date.now());
        // スケジュール実行がキューから1件消費されたはずなので待機件数を更新する。
        reloadQueueStatus();
      } else if (ev.kind === "taskCompleted" || ev.kind === "taskFailed" || ev.kind === "taskCancelled") {
        // 履歴ペインをこのタスクの結果で更新する(docs/requirements.md 受け入れ条件10)。
        invoke<HistoryEntry[]>("list_history", { limit: 20 })
          .then(setHistory)
          .catch((e) => setError(String(e)));
        reloadQueueStatus();
        reloadSchedules();
        setSessions((prev) => pruneOldFinishedSessions(prev));
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  // アクティブなタブが間引かれて消えた場合、直近のセッションへフォールバックする。
  useEffect(() => {
    if (activeSessionId && !sessions[activeSessionId]) {
      const ids = Object.keys(sessions);
      setActiveSessionId(ids.length > 0 ? ids[ids.length - 1] : null);
    }
  }, [sessions, activeSessionId]);

  // タブを切り替えたら返信欄の下書きを引き継がない(誤送信防止)。
  useEffect(() => {
    setReplyMessage("");
    setReplyError(null);
  }, [activeSessionId]);

  const anyRunning = Object.values(sessions).some(
    (s) => sessionSummary(s.events.map((e) => e.event)).status === "running",
  );

  // 経過時間の表示更新(いずれかのセッションが実行中の間は1秒ごと)。
  useEffect(() => {
    if (!anyRunning) return;
    const id = setInterval(() => setNowTick(Date.now()), 1000);
    return () => clearInterval(id);
  }, [anyRunning]);

  const activeSession = activeSessionId ? sessions[activeSessionId] : undefined;
  const tree: TreeState = buildTree(
    activeSession ? activeSession.events.map((e) => e.event) : [],
    activeSession ? activeSession.respondedRequestIds : NO_RESPONSES,
  );
  const runningSessionIds = Object.keys(sessions).filter(
    (sid) => sessionSummary(sessions[sid].events.map((e) => e.event)).status === "running",
  );
  const latestFailure = history.find((h) => h.status === "failed") ?? null;

  async function handleRun() {
    // docs/roadmap.md v0.5: 実行中でも他エージェントの実行ボタンは有効(並行実行)。
    // 同一エージェントの二重実行は outputDir 制限があればバックエンドがエラーを返す。
    if (!selected || !prompt.trim()) return;
    setRunError(null);
    try {
      await invoke("start_task", { agentId: selected, prompt });
    } catch (e) {
      setRunError(String(e));
    }
  }

  /** 選択中タブ(activeSessionId)のセッションを中断する。 */
  async function handleCancel() {
    if (!activeSessionId) return;
    try {
      await invoke("cancel_task", { sessionId: activeSessionId });
    } catch (e) {
      setRunError(String(e));
    }
  }

  /** 新規作成はその場で空の定義を作り、あとは設定ウインドウに任せる。 */
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
      await openAgentEditor(id, setNewAgentError);
    } catch (e) {
      setNewAgentError(String(e));
    }
  }

  function handleNewSchedule() {
    setScheduleForm(EMPTY_SCHEDULE_FORM);
    setScheduleSaveStatus(null);
  }

  function handleEditSchedule(s: Schedule) {
    setScheduleForm({
      id: s.id,
      agentId: s.agentId,
      prompt: s.prompt,
      type: s.recurrence.type,
      time: s.recurrence.time,
      weekday: s.recurrence.type === "weekly" ? s.recurrence.weekday : EMPTY_SCHEDULE_FORM.weekday,
      day: s.recurrence.type === "monthly" ? s.recurrence.day : EMPTY_SCHEDULE_FORM.day,
      enabled: s.enabled,
      lastRunAt: s.lastRunAt,
    });
    setScheduleSaveStatus(null);
  }

  async function handleSaveSchedule() {
    if (!scheduleForm.agentId || !scheduleForm.prompt.trim()) return;
    setScheduleSaveStatus(null);
    const recurrence: Recurrence =
      scheduleForm.type === "daily"
        ? { type: "daily", time: scheduleForm.time }
        : scheduleForm.type === "weekly"
          ? { type: "weekly", weekday: scheduleForm.weekday, time: scheduleForm.time }
          : { type: "monthly", day: scheduleForm.day, time: scheduleForm.time };
    const schedule: Schedule = {
      id: scheduleForm.id ?? crypto.randomUUID(),
      agentId: scheduleForm.agentId,
      prompt: scheduleForm.prompt,
      recurrence,
      enabled: scheduleForm.enabled,
      lastRunAt: scheduleForm.lastRunAt,
    };
    try {
      await invoke("save_schedule", { schedule });
      setScheduleSaveStatus("✅ 保存しました");
      setScheduleForm(EMPTY_SCHEDULE_FORM);
      await reloadSchedules();
    } catch (e) {
      setScheduleSaveStatus(`⚠ 保存に失敗しました: ${e}`);
    }
  }

  async function handleDeleteSchedule(id: string) {
    if (!window.confirm("このスケジュールを削除しますか?")) return;
    setScheduleError(null);
    try {
      await invoke("delete_schedule", { id });
      if (scheduleForm.id === id) setScheduleForm(EMPTY_SCHEDULE_FORM);
      await reloadSchedules();
    } catch (e) {
      setScheduleError(String(e));
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

  async function handlePickUpdateSource() {
    const dir = await open({ directory: true });
    if (typeof dir === "string") setUpdateSourceInput(dir);
  }

  async function handleSaveUpdateSource() {
    setUpdateCheckError(null);
    try {
      const path = updateSourceInput.trim() || null;
      await invoke("save_update_source", { path });
      setAppConfig((c) => (c ? { ...c, updateSource: path } : c));
    } catch (e) {
      setUpdateCheckError(String(e));
    }
  }

  async function handleCheckForUpdates() {
    setUpdateCheckError(null);
    try {
      const info = await invoke<UpdateInfoDto | null>("check_for_updates");
      setUpdateInfo(info);
      setUpdateDismissed(false);
    } catch (e) {
      setUpdateCheckError(String(e));
    }
  }

  async function handleOpenUpdateFolder() {
    setOpenUpdateFolderError(null);
    try {
      await invoke("open_update_folder");
    } catch (e) {
      setOpenUpdateFolderError(String(e));
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

  /** 監査ログフォルダ(data/logs/)を開く(docs/roadmap.md v0.6)。エージェント非依存。 */
  async function handleOpenLogsFolder() {
    setLogsFolderError(null);
    try {
      await invoke("open_logs_folder");
    } catch (e) {
      setLogsFolderError(String(e));
    }
  }

  /** 選択中タブ(activeSessionId)の respondedRequestIds に requestId を積む。 */
  async function respondPermission(requestId: string, decision: PermissionDecision) {
    if (!activeSessionId) return;
    try {
      await invoke("respond_permission", { requestId, decision });
      setSessions((prev) => {
        const s = prev[activeSessionId];
        if (!s) return prev;
        const responded = new Set(s.respondedRequestIds).add(requestId);
        return { ...prev, [activeSessionId]: { ...s, respondedRequestIds: responded } };
      });
    } catch (e) {
      setRunError(String(e));
    }
  }

  /** ask_user への回答(v1.0 経路A)。respondPermission と同じ respondedRequestIds を共有する
   * (request_id の名前空間が異なるため衝突しない)。 */
  async function respondUserInput(requestId: string, answer: string | null) {
    if (!activeSessionId) return;
    try {
      await invoke("respond_user_input", { requestId, answer });
      setSessions((prev) => {
        const s = prev[activeSessionId];
        if (!s) return prev;
        const responded = new Set(s.respondedRequestIds).add(requestId);
        return { ...prev, [activeSessionId]: { ...s, respondedRequestIds: responded } };
      });
    } catch (e) {
      setRunError(String(e));
    }
  }

  /** 実行履歴の1行を実行ビューのタブとして開く(会話のレジューム)。過去のイベントログは
   * 保存していないため、履歴行(依頼文・結果概要)から会話を復元する。続きの依頼は
   * 既存の返信欄 → reply_task(resume)がそのまま使える。 */
  function openHistorySession(h: HistoryEntry) {
    setSessions((prev) => {
      if (prev[h.sessionId]) return prev; // 画面に残っているタブはそのまま使う
      const time = new Date(h.startedAt).toLocaleTimeString();
      const started: AppEvent = {
        kind: "taskStarted",
        sessionId: h.sessionId,
        agentId: h.agentId,
        startedAt: h.startedAt,
        prompt: h.prompt,
      };
      const terminal: AppEvent =
        h.status === "completed"
          ? {
              kind: "taskCompleted",
              sessionId: h.sessionId,
              summary: h.summary || "(この実行の結果概要は記録されていません)",
              outputFiles: h.outputFiles,
            }
          : h.status === "failed"
            ? {
                kind: "taskFailed",
                sessionId: h.sessionId,
                error: h.summary || "(エラー内容は記録されていません)",
              }
            : { kind: "taskCancelled", sessionId: h.sessionId };
      return {
        ...prev,
        [h.sessionId]: {
          agentId: h.agentId,
          events: [
            { time, event: started },
            { time, event: terminal },
          ],
          rowStartedAt: {},
          respondedRequestIds: new Set(),
        },
      };
    });
    setActiveSessionId(h.sessionId);
  }

  /** タスク完了後の追い返信(v1.0 経路B)。同じ session_id を resume して続きを実行する。 */
  async function handleReply() {
    if (!activeSessionId || !replyMessage.trim()) return;
    setReplyError(null);
    const agentId = activeSession?.agentId ?? "";
    try {
      await invoke("reply_task", { sessionId: activeSessionId, agentId, message: replyMessage });
      setReplyMessage("");
    } catch (e) {
      setReplyError(String(e));
    }
  }

  return (
    <>
      {updateInfo && !updateDismissed && (
        <div className={updateInfo.hashOk ? "update-banner" : "update-banner update-banner-danger"}>
          <div className="update-banner-row">
            <span className="update-banner-text">
              新しいバージョン v{updateInfo.version} が利用可能です(現在 v
              {appConfig?.currentVersion ?? "?"})。{updateInfo.notes}
            </span>
            {updateInfo.hashOk ? (
              <button type="button" onClick={handleOpenUpdateFolder}>
                配布フォルダを開く
              </button>
            ) : (
              <span className="update-banner-warning">
                ⚠ 配布物のハッシュが一致しません。適用しないでください
              </span>
            )}
            <button type="button" onClick={() => setUpdateDismissed(true)} aria-label="閉じる">
              ✕
            </button>
          </div>
          {openUpdateFolderError && <p className="error">⚠ {openUpdateFolderError}</p>}
        </div>
      )}
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
                  onDoubleClick={() => openAgentEditor(a.id, setError)}
                >
                  <Avatar name={a.name} />
                  <span className="agent-card-body">
                    <strong>
                      {a.name}
                      <span className={`badge scope-${a.scope}`}>
                        {a.scope === "shared" ? "🌐 共有" : "👤 個人"}
                      </span>
                      {a.version && <span className="badge">v{a.version}</span>}
                      {a.shadowed && <span className="badge">個人版あり</span>}
                      {unset && <span className="badge">⚠ 未設定</span>}
                    </strong>
                    <span className="muted">{a.description}</span>
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
        {selected && (
          <div className="run-controls">
            <button type="button" onClick={() => openAgentEditor(selected, setError)}>
              ⚙ 設定を開く(別ウインドウ)
            </button>
          </div>
        )}
        {appConfig && appConfig.forcedDeniedTools.length > 0 && (
          <div className="shared-settings">
            <h3>管理者ポリシー</h3>
            <p className="muted">管理者ポリシーで拒否: {appConfig.forcedDeniedTools.join(", ")}(変更不可)</p>
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
        <div className="shared-settings">
          <h3>更新配布フォルダ</h3>
          <label>
            配布フォルダ(manifest.json を置く場所)
            <div className="folder-row">
              <input
                type="text"
                value={updateSourceInput}
                onChange={(e) => setUpdateSourceInput(e.target.value)}
              />
              <button type="button" onClick={handlePickUpdateSource}>
                選択...
              </button>
            </div>
          </label>
          <div className="run-controls">
            <button type="button" onClick={handleSaveUpdateSource}>
              保存
            </button>
            <button type="button" onClick={handleCheckForUpdates} disabled={!appConfig?.updateSource}>
              今すぐ確認
            </button>
          </div>
          {updateCheckError && <p className="error">⚠ {updateCheckError}</p>}
          {!updateInfo && !updateCheckError && (
            <p className="muted">更新はありません(現在 v{appConfig?.currentVersion ?? "?"})</p>
          )}
        </div>
        <div className="shared-settings">
          <h3>スケジュール</h3>
          <p className="muted">スケジュールはアプリ起動中のみ動作します。</p>
          {schedules.length === 0 ? (
            <p className="muted">スケジュールはまだありません</p>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>エージェント</th>
                  <th>周期</th>
                  <th>状態</th>
                  <th>最終実行</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {schedules.map((s) => (
                  <tr key={s.id}>
                    <td>{s.agentId}</td>
                    <td>{formatRecurrence(s.recurrence)}</td>
                    <td>{s.enabled ? "有効" : "無効"}</td>
                    <td>{s.lastRunAt ?? "—"}</td>
                    <td>
                      <button type="button" onClick={() => handleEditSchedule(s)}>
                        編集
                      </button>
                      <button type="button" onClick={() => handleDeleteSchedule(s.id)}>
                        削除
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          <p className="muted">{scheduleForm.id ? "スケジュールの編集" : "スケジュールの追加"}</p>
          <label>
            エージェント
            <select
              value={scheduleForm.agentId}
              onChange={(e) => setScheduleForm((f) => ({ ...f, agentId: e.target.value }))}
            >
              <option value="">選択してください</option>
              {agents.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            プロンプト
            <textarea
              className="prompt-input"
              rows={2}
              value={scheduleForm.prompt}
              onChange={(e) => setScheduleForm((f) => ({ ...f, prompt: e.target.value }))}
            />
          </label>
          <label>
            周期
            <select
              value={scheduleForm.type}
              onChange={(e) =>
                setScheduleForm((f) => ({ ...f, type: e.target.value as ScheduleFormState["type"] }))
              }
            >
              <option value="daily">毎日</option>
              <option value="weekly">毎週</option>
              <option value="monthly">毎月</option>
            </select>
          </label>
          {scheduleForm.type === "weekly" && (
            <label>
              曜日
              <select
                value={scheduleForm.weekday}
                onChange={(e) => setScheduleForm((f) => ({ ...f, weekday: Number(e.target.value) }))}
              >
                {WEEKDAY_LABELS.map((label, i) => (
                  <option key={label} value={i}>
                    {label}曜
                  </option>
                ))}
              </select>
            </label>
          )}
          {scheduleForm.type === "monthly" && (
            <label>
              日(29〜31日は月末に丸められます)
              <input
                type="number"
                min={1}
                max={31}
                value={scheduleForm.day}
                onChange={(e) => setScheduleForm((f) => ({ ...f, day: Number(e.target.value) }))}
              />
            </label>
          )}
          <label>
            時刻
            <input
              type="time"
              value={scheduleForm.time}
              onChange={(e) => setScheduleForm((f) => ({ ...f, time: e.target.value }))}
            />
          </label>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={scheduleForm.enabled}
              onChange={(e) => setScheduleForm((f) => ({ ...f, enabled: e.target.checked }))}
            />
            有効
          </label>
          <div className="run-controls">
            <button
              type="button"
              onClick={handleSaveSchedule}
              disabled={!scheduleForm.agentId || !scheduleForm.prompt.trim()}
            >
              保存
            </button>
            {scheduleForm.id && (
              <button type="button" onClick={handleNewSchedule}>
                新規作成に戻る
              </button>
            )}
          </div>
          {scheduleSaveStatus && <p className="muted">{scheduleSaveStatus}</p>}
          {scheduleError && <p className="error">⚠ {scheduleError}</p>}
        </div>
      </aside>
      <main className="pane run">
        <h2>実行ビュー</h2>
        {/* ダッシュボード帯(docs/roadmap.md v0.5): 実行中セッションのチップ・待機キュー・直近の失敗。 */}
        <div className="dashboard-bar">
          <div className="dashboard-chips">
            {runningSessionIds.length === 0 && <span className="muted">実行中のセッションはありません</span>}
            {runningSessionIds.map((sid) => {
              const s = sessions[sid];
              const t = buildTree(s.events.map((e) => e.event), s.respondedRequestIds);
              const pending =
                (t.main?.pendingPermissions.length ?? 0) +
                t.subagents.reduce((n, r) => n + r.pendingPermissions.length, 0);
              const elapsed = sessionElapsedMs(s.rowStartedAt, nowTick);
              return (
                <button
                  key={sid}
                  type="button"
                  className={sid === activeSessionId ? "dashboard-chip active" : "dashboard-chip"}
                  onClick={() => setActiveSessionId(sid)}
                >
                  <Avatar name={s.agentId} status="running" />
                  <strong>{s.agentId}</strong>
                  <span className="muted">{elapsed != null ? formatDuration(elapsed) : "—"}</span>
                  {pending > 0 && <span className="badge">⚠ 承認待ち {pending}</span>}
                </button>
              );
            })}
          </div>
          <span className="muted">待機中のスケジュール実行: {queueStatus?.queued ?? 0} 件</span>
          {latestFailure && (
            <span className="dashboard-failure">
              ⚠ 直近の失敗: {latestFailure.agentId}({latestFailure.startedAt})
            </span>
          )}
        </div>
        {selected ? (
          <>
            <p className="muted">{agents.find((a) => a.id === selected)?.description}</p>
            <textarea
              className="prompt-input"
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && e.ctrlKey) {
                  e.preventDefault();
                  handleRun();
                }
              }}
              placeholder="依頼内容を入力してください(Ctrl+Enter で実行)"
              rows={3}
            />
            <div className="run-controls">
              <button className="primary" onClick={handleRun} disabled={!prompt.trim()}>
                ▶ 実行
              </button>
            </div>
            {runError && <p className="error">⚠ {runError}</p>}
          </>
        ) : (
          <p className="muted">左の一覧からエージェントを選択してください</p>
        )}
        {/* セッションタブ(docs/roadmap.md v0.5: 実行中+直近数件のセッション)。各タブは既存の
            buildTree でツリーを描画する(呼び出し側でセッション分割してから渡すだけ)。 */}
        {Object.keys(sessions).length > 0 && (
          <div className="session-tabs">
            {Object.keys(sessions).map((sid) => {
              const summary = sessionSummary(sessions[sid].events.map((e) => e.event));
              return (
                <button
                  key={sid}
                  type="button"
                  className={sid === activeSessionId ? "session-tab active" : "session-tab"}
                  onClick={() => setActiveSessionId(sid)}
                >
                  {summary.agentId} {STATUS_LABEL[summary.status]}
                </button>
              );
            })}
          </div>
        )}
        {activeSession ? (
          <>
            {/* 継続セッションの会話表示: 過去のターン(依頼→結果)を古い順に並べ、
                その下に現在の実行ツリーが続く(会話のレジューム)。 */}
            {tree.turns.length > 0 && (
              <p className="muted">🔗 継続した会話です({tree.turns.length + 1} 回目の依頼)</p>
            )}
            {tree.turns.map((t, i) => (
              <div className="conversation-turn" key={i}>
                <p className="conversation-prompt">🖐 {t.prompt ?? "(依頼内容は記録されていません)"}</p>
                <p className="conversation-result">
                  {t.status === "completed" ? "✅" : t.status === "failed" ? "❌" : "⏹"}{" "}
                  {t.summary ?? "(結果なし)"}
                </p>
              </div>
            ))}
            {tree.prompt && <p className="conversation-prompt">🖐 {tree.prompt}</p>}
            {tree.taskStatus === "running" && (
              <div className="run-controls">
                <button onClick={handleCancel}>中断</button>
              </div>
            )}
            {tree.main && (
              <div className="agent-tree">
                <AgentRowView
                  row={tree.main}
                  elapsedMs={elapsedMsFor(tree.main, activeSession.rowStartedAt, nowTick)}
                  isSub={false}
                  onRespond={respondPermission}
                  onRespondUserInput={respondUserInput}
                />
                {tree.subagents.map((sub) => (
                  <div className="sub-indent" key={sub.key}>
                    <AgentRowView
                      row={sub}
                      elapsedMs={elapsedMsFor(sub, activeSession.rowStartedAt, nowTick)}
                      isSub
                      onRespond={respondPermission}
                      onRespondUserInput={respondUserInput}
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
            {(tree.taskStatus === "completed" || tree.taskStatus === "failed" || tree.taskStatus === "cancelled") && (
              <div className="task-summary">
                <h3>続けて依頼する(このセッションに返信)</h3>
                <p className="muted">これまでの会話は保持したまま、同じセッションの続きとして実行します。</p>
                <textarea
                  className="prompt-input"
                  rows={2}
                  value={replyMessage}
                  onChange={(e) => setReplyMessage(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && e.ctrlKey) {
                      e.preventDefault();
                      handleReply();
                    }
                  }}
                  placeholder="続けて依頼する内容を入力してください(Ctrl+Enter で送信)"
                />
                <div className="run-controls">
                  <button type="button" onClick={handleReply} disabled={!replyMessage.trim()}>
                    返信して続行
                  </button>
                </div>
                {replyError && <p className="error">⚠ {replyError}</p>}
              </div>
            )}
          </>
        ) : (
          <p className="muted">実行中・直近のセッションはまだありません</p>
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
        <div className="run-controls">
          <button type="button" onClick={handleOpenLogsFolder}>
            監査ログフォルダを開く
          </button>
        </div>
        {logsFolderError && <p className="error">⚠ {logsFolderError}</p>}
        <h3>ログ({activeSession ? sessionSummary(activeSession.events.map((e) => e.event)).agentId : "—"})</h3>
        <ul className="event-log">
          {(activeSession?.events ?? []).map((e, i) => (
            <li key={i} className={e.event.kind === "taskFailed" ? "error" : undefined}>
              <span className="muted">{e.time}</span> [{e.event.kind}] {summarize(e.event)}
            </li>
          ))}
        </ul>
        <p className="muted app-version">agent-deck v{appConfig?.currentVersion ?? "?"}</p>
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
                <th>種別</th>
                <th>状態</th>
                <th>所要</th>
                <th>トークン</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {history.map((h) => (
                // 継続依頼(resume)では同じ sessionId の履歴行が実行ごとに増えるため、
                // startedAt と組み合わせて一意にする。
                <tr key={`${h.sessionId}:${h.startedAt}`}>
                  <td>{h.startedAt}</td>
                  <td>{h.agentId}</td>
                  <td>{TRIGGER_LABEL[h.trigger]}</td>
                  <td>
                    {h.status === "completed" ? "✅ 完了" : h.status === "failed" ? "❌ 失敗" : "⏹ 中断"}
                    {/* 完成品か途中で止まったものかを判別できるようにする(docs/architecture.md §8.3)。 */}
                    {h.status !== "completed" && <div className="muted">⚠ 出力は未完成の可能性</div>}
                  </td>
                  <td>{Math.round(h.durationMs / 1000)} 秒</td>
                  <td>{h.totalTokens != null ? h.totalTokens : "—"}</td>
                  <td>
                    <button type="button" onClick={() => openHistorySession(h)}>
                      会話を開く
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </footer>
      </div>
    </>
  );
}
