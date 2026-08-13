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
import {
  AGENT_TOOL_OPTIONS,
  PERMISSION_TOOL_OPTIONS,
  composeAgentTools,
  composePermissionTools,
  decomposeAgentTools,
  decomposePermissionTools,
} from "./toolCatalog";
import type { AgentToolsFormState, PermissionToolsFormState } from "./toolCatalog";

// 実行ビュー上部のダッシュボード帯・タブに残す「直近の完了/失敗/中断セッション」件数の上限
// (docs/roadmap.md v0.5: 実行中+直近数件のセッション)。実行中セッションは件数に含めない。
const MAX_RECENT_FINISHED_SESSIONS = 5;

const NO_RESPONSES = new Set<string>();

// 3 ペイン構成の骨格(docs/requirements.md §3.1)。
// 実行ビューはツリー表示(docs/requirements.md §3.3。このアプリの主目的)。
// 生イベントの時系列ログ・トークン使用量・出力ファイルは詳細ペインへ移設。

/** 設定フォームの編集用の状態。許可/拒否ツールはチェック集合+その他テキストで持ち、
 * 保存時に toolCatalog の compose 関数で配列へ結合する(スペルミス防止。ユーザー要望)。 */
interface ConfigFormState {
  inputDir: string;
  outputDir: string;
  allowedTools: PermissionToolsFormState;
  deniedTools: PermissionToolsFormState;
  autoApprove: boolean;
}

const EMPTY_FORM: ConfigFormState = {
  inputDir: "",
  outputDir: "",
  allowedTools: { checked: [], other: "" },
  deniedTools: { checked: [], other: "" },
  autoApprove: true,
};

/** エージェント定義エディタ(個人スコープ)の編集用状態(docs/roadmap.md v0.2)。
 * tools はチェック集合+その他テキスト+全ツール/選択の切り替えで持つ(スペルミス防止。ユーザー要望)。 */
interface DefinitionFormState {
  name: string;
  description: string;
  model: string;
  tools: AgentToolsFormState;
  body: string;
}

const EMPTY_DEF_FORM: DefinitionFormState = {
  name: "",
  description: "",
  model: "",
  tools: { mode: "all", checked: [], other: "" },
  body: "",
};

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

/** 入出力設定の許可/拒否ツール入力欄。基本形はチェックボックス(スペルミス防止。ユーザー要望)、
 * 括弧付きパターン等は「高度なパターン」欄に手入力する。 */
function PermissionToolsField({
  label,
  description,
  advancedHint,
  state,
  onChange,
  showNotes,
}: {
  label: string;
  /** ラベル直下に出す説明(この欄が何をするものかの一文)。 */
  description?: string;
  advancedHint: string;
  state: PermissionToolsFormState;
  onChange: (next: PermissionToolsFormState) => void;
  showNotes?: boolean;
}) {
  return (
    <div className="tools-field">
      <span className="tools-field-label">{label}</span>
      {description && <p className="muted hint">{description}</p>}
      {PERMISSION_TOOL_OPTIONS.map((opt) => (
        <div key={opt.value}>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={state.checked.includes(opt.value)}
              onChange={(e) =>
                onChange({
                  ...state,
                  checked: e.target.checked
                    ? [...state.checked, opt.value]
                    : state.checked.filter((v) => v !== opt.value),
                })
              }
            />
            {opt.label}({opt.value})
          </label>
          {showNotes && opt.note && <p className="muted hint">{opt.note}</p>}
        </div>
      ))}
      <label>
        高度なパターン(手入力・カンマ区切り。例: {advancedHint})
        <input type="text" value={state.other} onChange={(e) => onChange({ ...state, other: e.target.value })} />
      </label>
    </div>
  );
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

  // 選択中エージェントの入出力設定(docs/requirements.md §3.4)。
  const [configs, setConfigs] = useState<Record<string, AgentSettings | null>>({});
  const [form, setForm] = useState<ConfigFormState>(EMPTY_FORM);
  // 未保存の編集があるか。true の間は configs の再取得(起動直後の非同期完了や
  // reloadAgents 後)でフォームを上書きしない — フォルダ選択直後に値が消えるバグの根本対策。
  const [formDirty, setFormDirty] = useState(false);
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
      setAgents(await invoke<AgentSummary[]>("list_agents"));
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
          tools: decomposeAgentTools(d.tools),
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
  // 編集中(formDirty)は configs の再取得が来ても上書きしない。
  useEffect(() => {
    if (!selected || formDirty) return;
    const c = configs[selected];
    setForm({
      inputDir: c?.inputDir ?? "",
      outputDir: c?.outputDir ?? "",
      allowedTools: decomposePermissionTools(c?.allowedTools ?? []),
      deniedTools: decomposePermissionTools(c?.deniedTools ?? []),
      autoApprove: c?.autoApproveWriteInOutputDir ?? true,
    });
    setSaveStatus(null);
  }, [selected, configs, formDirty]);

  /** 設定フォームの編集。dirty を立てて再取得による巻き戻りを防ぐ。 */
  function updateForm(patch: Partial<ConfigFormState>) {
    setForm((f) => ({ ...f, ...patch }));
    setFormDirty(true);
  }

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
        // 設定フォームの表示を実態(agents.json への永続化)とずれさせない。formDirty=true の
        // 間はフォーム自体を触らないが、それは configs→form の同期 useEffect が既にガードしている
        // (下の useEffect の deps に formDirty がある)ため、ここでは configs だけ更新すればよい。
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

  async function handlePickFolder(target: "inputDir" | "outputDir") {
    const dir = await open({ directory: true });
    if (typeof dir === "string") {
      updateForm({ [target]: dir });
    }
  }

  async function handleSaveConfig() {
    if (!selected) return;
    const settings: AgentSettings = {
      inputDir: form.inputDir.trim() || null,
      outputDir: form.outputDir.trim() || null,
      allowedTools: composePermissionTools(form.allowedTools),
      deniedTools: composePermissionTools(form.deniedTools),
      autoApproveWriteInOutputDir: form.autoApprove,
    };
    try {
      await invoke("save_agent_config", { agentId: selected, settings });
      setConfigs((prev) => ({ ...prev, [selected]: settings }));
      setFormDirty(false);
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
        tools: composeAgentTools(defForm.tools),
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
        tools: decomposeAgentTools(d.tools),
        body: d.body,
      });
    } catch (e) {
      setDefinitionError(String(e));
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
                  onClick={() => {
                    setSelected(a.id);
                    // 別エージェントへ切り替えたら編集は破棄してそのエージェントの設定を読む
                    setFormDirty(false);
                  }}
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
                <div className="tools-field">
                  <span className="tools-field-label">ツール</span>
                  <label className="radio-row">
                    <input
                      type="radio"
                      name="def-tools-mode"
                      checked={defForm.tools.mode === "all"}
                      onChange={() => setDefForm((f) => ({ ...f, tools: { ...f.tools, mode: "all" } }))}
                    />
                    全ツール(既定)
                  </label>
                  <label className="radio-row">
                    <input
                      type="radio"
                      name="def-tools-mode"
                      checked={defForm.tools.mode === "selected"}
                      onChange={() => setDefForm((f) => ({ ...f, tools: { ...f.tools, mode: "selected" } }))}
                    />
                    選択したツールのみ
                  </label>
                  {defForm.tools.mode === "selected" && (
                    <>
                      {AGENT_TOOL_OPTIONS.map((opt) => (
                        <label key={opt.value} className="checkbox-row">
                          <input
                            type="checkbox"
                            checked={defForm.tools.checked.includes(opt.value)}
                            onChange={(e) =>
                              setDefForm((f) => ({
                                ...f,
                                tools: {
                                  ...f.tools,
                                  checked: e.target.checked
                                    ? [...f.tools.checked, opt.value]
                                    : f.tools.checked.filter((v) => v !== opt.value),
                                },
                              }))
                            }
                          />
                          {opt.label}({opt.value})
                        </label>
                      ))}
                      <label>
                        その他(手入力・カンマ区切り。認識できない名前は Copilot 側で無視されます)
                        <input
                          type="text"
                          value={defForm.tools.other}
                          onChange={(e) =>
                            setDefForm((f) => ({ ...f, tools: { ...f.tools, other: e.target.value } }))
                          }
                        />
                      </label>
                    </>
                  )}
                </div>
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
                  onChange={(e) => updateForm({ inputDir: e.target.value })}
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
                  onChange={(e) => updateForm({ outputDir: e.target.value })}
                />
                <button type="button" onClick={() => handlePickFolder("outputDir")}>
                  選択...
                </button>
              </div>
            </label>
            <PermissionToolsField
              label="自動承認ツール(確認なしで実行を許可)"
              description="チェックした種類の操作は、承認ダイアログを出さずに実行されます。実行中のダイアログで「常に許可」を選ぶと、ここに自動で追加されます。"
              advancedHint="shell(python:*)"
              state={form.allowedTools}
              onChange={(next) => updateForm({ allowedTools: next })}
              showNotes
            />
            <PermissionToolsField
              label="拒否ツール(常にブロック)"
              description="チェックした種類の操作は確認なしで拒否されます。自動承認より優先されます。"
              advancedHint="shell(rm)"
              state={form.deniedTools}
              onChange={(next) => updateForm({ deniedTools: next })}
            />
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={form.autoApprove}
                onChange={(e) => updateForm({ autoApprove: e.target.checked })}
              />
              出力フォルダへの書き込みを自動承認する
            </label>
            <p className="muted hint">
              オンでも出力フォルダの外への書き込みは確認ダイアログが出ます(フォルダで書き込み許可を分ける仕組み)。
            </p>
            <div className="run-controls">
              <button type="button" onClick={handleSaveConfig}>
                保存
              </button>
            </div>
            {saveStatus && <p className="muted">{saveStatus}</p>}
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
              placeholder="依頼内容を入力してください"
              rows={3}
            />
            <div className="run-controls">
              <button onClick={handleRun} disabled={!prompt.trim()}>
                実行
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
            {(tree.taskStatus === "completed" || tree.taskStatus === "failed") && (
              <div className="task-summary">
                <h3>続けて依頼する(このセッションに返信)</h3>
                <textarea
                  className="prompt-input"
                  rows={2}
                  value={replyMessage}
                  onChange={(e) => setReplyMessage(e.target.value)}
                  placeholder="続けて依頼する内容を入力してください"
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
              </tr>
            </thead>
            <tbody>
              {history.map((h) => (
                <tr key={h.sessionId}>
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
