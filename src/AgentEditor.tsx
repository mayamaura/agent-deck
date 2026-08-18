import { useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open } from "@tauri-apps/plugin-dialog";
import type { AgentDefinitionDto, AgentSettings, DraftedAgent, ModelCatalog, ModelOption } from "./types";
import {
  AGENT_TOOL_OPTIONS,
  PERMISSION_TOOL_OPTIONS,
  composeAgentTools,
  composePermissionTools,
  decomposeAgentTools,
  decomposePermissionTools,
} from "./toolCatalog";
import type { AgentToolsFormState, PermissionToolsFormState } from "./toolCatalog";

/** 設定ウインドウでの保存・削除をメインウインドウへ知らせるイベント名。
 * メイン側は一覧と「未設定」バッジを取り直す。 */
export const AGENTS_CHANGED = "agents-changed";

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
  /** エージェント ID(= 定義ファイル名)。保存時に変更されていればリネームしてから保存する。 */
  id: string;
  name: string;
  description: string;
  model: string;
  tools: AgentToolsFormState;
  body: string;
}

const EMPTY_DEF_FORM: DefinitionFormState = {
  id: "",
  name: "",
  description: "",
  model: "",
  tools: { mode: "all", checked: [], other: "" },
  body: "",
};

/** モデル選択欄の表示。倍率はプレミアムリクエストの消費量(0 は無料枠)。 */
function modelLabel(m: ModelOption): string {
  const cost = m.multiplier == null ? "" : m.multiplier === 0 ? " — 無料枠" : ` — プレミアム ×${m.multiplier}`;
  return `${m.name}(${m.id})${cost}`;
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
  afterOptions,
}: {
  label: string;
  /** ラベル直下に出す説明(この欄が何をするものかの一文)。 */
  description?: string;
  advancedHint: string;
  state: PermissionToolsFormState;
  onChange: (next: PermissionToolsFormState) => void;
  showNotes?: boolean;
  /** チェックボックス群の直後(高度なパターン欄の前)に挿し込む追加行。
   * 自動承認ツール側で「出力フォルダのみの書き込み自動承認」を write の直下に
   * 連続配置するために使う(ユーザー要望: 書き込み系の設定を離さない)。 */
  afterOptions?: ReactNode;
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
      {afterOptions}
      <label>
        高度なパターン(手入力・カンマ区切り。例: {advancedHint})
        <input type="text" value={state.other} onChange={(e) => onChange({ ...state, other: e.target.value })} />
      </label>
    </div>
  );
}

/**
 * エージェント 1 件の設定ウインドウ(定義 + 入出力設定)。メインウインドウとは別プロセスの
 * webview なので状態は共有しない。保存のたびに AGENTS_CHANGED を emit してメイン側に再取得させる。
 */
export default function AgentEditor({ agentId }: { agentId: string }) {
  // 編集中の実 ID。保存時のリネームで変わるので、prop ではなくこちらを全操作の対象にする。
  // ponytail: ウインドウのラベルは開いた時の ID のまま(Tauri は付け替えられない)。
  // リネーム後にメイン一覧から同じエージェントを開くと 2 枚目が開くが、単一ユーザーの
  // デスクトップアプリなので放置する。困るようならリネーム時にウインドウを閉じる。
  const [id, setId] = useState(agentId);
  const [definition, setDefinition] = useState<AgentDefinitionDto | null>(null);
  const [definitionError, setDefinitionError] = useState<string | null>(null);
  const [defForm, setDefForm] = useState<DefinitionFormState>(EMPTY_DEF_FORM);
  const [defSaveStatus, setDefSaveStatus] = useState<string | null>(null);

  const [form, setForm] = useState<ConfigFormState>(EMPTY_FORM);
  const [saveStatus, setSaveStatus] = useState<string | null>(null);

  // 定義の下書きを Copilot に書かせる(docs/roadmap.md v1.1)。生成結果はフォームに
  // 流し込むだけで、ファイルに書くのは利用者が「保存」を押したとき。
  const [draftRequest, setDraftRequest] = useState("");
  const [drafting, setDrafting] = useState(false);
  const [draftStatus, setDraftStatus] = useState<string | null>(null);

  // モデルの選択肢。契約プランで使えるものを Copilot から都度取得する(アプリ側に
  // モデル名を持たないので、プランの違いも将来のモデル追加もそのまま反映される)。
  // 取得は Copilot CLI の起動を伴うので数秒かかる。届くまでは「取得中」表示。
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null);
  const [catalogError, setCatalogError] = useState<string | null>(null);

  function loadDefinition(d: AgentDefinitionDto) {
    setDefinition(d);
    setDefForm({
      id: d.id,
      name: d.name,
      description: d.description,
      model: d.model ?? "",
      tools: decomposeAgentTools(d.tools),
      body: d.body,
    });
  }

  useEffect(() => {
    document.title = `エージェント設定 — ${id}`;
  }, [id]);

  useEffect(() => {
    invoke<AgentDefinitionDto>("get_agent_definition", { agentId })
      .then(loadDefinition)
      .catch((e) => setDefinitionError(String(e)));
    invoke<AgentSettings | null>("get_agent_config", { agentId })
      .then((c) =>
        setForm({
          inputDir: c?.inputDir ?? "",
          outputDir: c?.outputDir ?? "",
          allowedTools: decomposePermissionTools(c?.allowedTools ?? []),
          deniedTools: decomposePermissionTools(c?.deniedTools ?? []),
          autoApprove: c?.autoApproveWriteInOutputDir ?? true,
        }),
      )
      .catch((e) => setDefinitionError(String(e)));
  }, [agentId]);

  useEffect(() => {
    invoke<ModelCatalog>("list_models")
      .then(setCatalog)
      .catch((e) => setCatalogError(String(e)));
  }, []);

  async function handlePickFolder(target: "inputDir" | "outputDir") {
    const dir = await open({ directory: true });
    if (typeof dir === "string") setForm((f) => ({ ...f, [target]: dir }));
  }

  async function handleSaveConfig() {
    const settings: AgentSettings = {
      inputDir: form.inputDir.trim() || null,
      outputDir: form.outputDir.trim() || null,
      allowedTools: composePermissionTools(form.allowedTools),
      deniedTools: composePermissionTools(form.deniedTools),
      autoApproveWriteInOutputDir: form.autoApprove,
    };
    try {
      await invoke("save_agent_config", { agentId: id, settings });
      setSaveStatus("✅ 保存しました");
      await emit(AGENTS_CHANGED);
    } catch (e) {
      setSaveStatus(`⚠ 保存に失敗しました: ${e}`);
    }
  }

  async function handleSaveDefinition() {
    setDefSaveStatus(null);
    try {
      // ID(= ファイル名)が変わっていれば先にリネームする。入出力設定・スケジュール・
      // 既定の作業フォルダは Rust 側が一緒に付け替える。
      const nextId = defForm.id.trim();
      if (nextId !== id) {
        await invoke("rename_agent_definition", { agentId: id, newId: nextId });
        setId(nextId);
        await emit(AGENTS_CHANGED);
      }
      await invoke("save_agent_definition", {
        agentId: nextId,
        name: defForm.name,
        description: defForm.description,
        tools: composeAgentTools(defForm.tools),
        model: defForm.model.trim() || null,
        body: defForm.body,
      });
      setDefSaveStatus("✅ 保存しました");
      await emit(AGENTS_CHANGED);
    } catch (e) {
      setDefSaveStatus(`⚠ 保存に失敗しました: ${e}`);
    }
  }

  async function handleDraft() {
    const request = draftRequest.trim();
    if (!request) return;
    // 生成は name/description/tools/body をまとめて置き換える。書きかけがあれば確認する
    // (保存前なのでファイルは無事だが、編集中の文章が消えるのは驚きになる)。
    const hasEdits = Boolean(defForm.name || defForm.description || defForm.body);
    if (hasEdits && !window.confirm("名前・説明・ツール・本文を下書きで置き換えます。よろしいですか?")) return;
    setDrafting(true);
    setDraftStatus(null);
    try {
      const drafted = await invoke<DraftedAgent>("draft_agent_definition", { request });
      setDefForm((f) => ({
        ...f,
        name: drafted.name,
        description: drafted.description,
        tools: decomposeAgentTools(drafted.tools),
        body: drafted.body,
      }));
      setDraftStatus("✅ 下書きを反映しました。内容を確認して「保存」を押してください");
    } catch (e) {
      setDraftStatus(`⚠ 下書きの生成に失敗しました: ${e}`);
    } finally {
      setDrafting(false);
    }
  }

  async function handleDeleteDefinition() {
    if (!window.confirm(`エージェント定義「${id}」を削除しますか?`)) return;
    try {
      await invoke("delete_agent_definition", { agentId: id });
      await emit(AGENTS_CHANGED);
      // 対象が消えたウインドウを残しても操作できないので閉じる。
      await getCurrentWebviewWindow().close();
    } catch (e) {
      setDefinitionError(String(e));
    }
  }

  async function handleDuplicateAgent() {
    setDefinitionError(null);
    try {
      await invoke("duplicate_agent", { agentId: id });
      await emit(AGENTS_CHANGED);
      // 複製後は個人優先で解決され直すため、編集可能な個人版を再取得する。
      loadDefinition(await invoke<AgentDefinitionDto>("get_agent_definition", { agentId: id }));
    } catch (e) {
      setDefinitionError(String(e));
    }
  }

  return (
    <div className="editor-window">
      <h2>{id}</h2>
      {definitionError && <p className="error">⚠ {definitionError}</p>}
      {definition && (
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
                Copilot に下書きしてもらう(やりたいことを日本語で)
                <textarea
                  className="prompt-input"
                  rows={3}
                  value={draftRequest}
                  disabled={drafting}
                  placeholder="例: 部署別のアンケート結果 CSV を集計して、傾向をまとめたレポートを作りたい"
                  onChange={(e) => setDraftRequest(e.target.value)}
                />
              </label>
              <p className="muted hint">
                下書きが下の各欄に入るだけです。「保存」を押すまでファイルには書き込まれません。
                ツールは必要なものだけに絞られるので、足りなければ自分で追加してください。
              </p>
              <div className="run-controls">
                <button type="button" onClick={handleDraft} disabled={drafting || !draftRequest.trim()}>
                  {drafting ? "生成中..." : "下書きしてもらう"}
                </button>
              </div>
              {draftStatus && <p className="muted">{draftStatus}</p>}
              <label>
                ID(定義ファイル名。保存時に変更されます)
                <input
                  type="text"
                  value={defForm.id}
                  onChange={(e) => setDefForm((f) => ({ ...f, id: e.target.value }))}
                />
              </label>
              <p className="muted hint">
                入出力設定・スケジュール・作業フォルダも一緒に移ります。履歴は旧 ID のまま残ります。
                実行中は変更できません。
              </p>
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
                モデル(未選択でアプリ既定)
                {catalog ? (
                  <select
                    value={defForm.model}
                    onChange={(e) => setDefForm((f) => ({ ...f, model: e.target.value }))}
                  >
                    <option value="">(アプリ既定)</option>
                    {/* 一覧に無い設定値(プラン変更・モデル廃止など)も選択を保ったまま見せる */}
                    {defForm.model && !catalog.models.some((m) => m.id === defForm.model) && (
                      <option value={defForm.model}>{defForm.model}(現在の設定・今は選べません)</option>
                    )}
                    {catalog.models.map((m) => (
                      <option key={m.id} value={m.id}>
                        {modelLabel(m)}
                      </option>
                    ))}
                  </select>
                ) : (
                  // 取得中は待たせず手入力も許す。失敗時はここが唯一の入力手段になる。
                  <input
                    type="text"
                    value={defForm.model}
                    placeholder={catalogError ? "モデル ID を入力" : "モデル一覧を取得中…"}
                    onChange={(e) => setDefForm((f) => ({ ...f, model: e.target.value }))}
                  />
                )}
              </label>
              {catalog && (
                <p className="muted hint">
                  {catalog.plan ? `契約プラン: ${catalog.plan}。` : ""}
                  ログイン中のアカウントで使えるモデルを Copilot から取得しています。
                </p>
              )}
              {catalogError && (
                <p className="muted hint">⚠ モデル一覧を取得できませんでした({catalogError})</p>
              )}
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
                  rows={8}
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
        <PermissionToolsField
          label="自動承認ツール(確認なしで実行を許可)"
          description="チェックした種類の操作は、承認ダイアログを出さずに実行されます。実行中のダイアログで「常に許可」を選ぶと、ここに自動で追加されます。"
          advancedHint="shell(python:*)"
          state={form.allowedTools}
          onChange={(next) => setForm((f) => ({ ...f, allowedTools: next }))}
          showNotes
          afterOptions={
            <div>
              <label className="checkbox-row">
                <input
                  type="checkbox"
                  checked={form.autoApprove}
                  onChange={(e) => setForm((f) => ({ ...f, autoApprove: e.target.checked }))}
                />
                ファイル書き込み(出力フォルダの中のみ)
              </label>
              <p className="muted hint">
                推奨・既定オン。出力フォルダの外への書き込みは確認ダイアログが出ます。
              </p>
            </div>
          }
        />
        <PermissionToolsField
          label="拒否ツール(常にブロック)"
          description="チェックした種類の操作は確認なしで拒否されます。自動承認より優先されます。"
          advancedHint="shell(rm)"
          state={form.deniedTools}
          onChange={(next) => setForm((f) => ({ ...f, deniedTools: next }))}
        />
        <div className="run-controls">
          <button type="button" onClick={handleSaveConfig}>
            保存
          </button>
        </div>
        {saveStatus && <p className="muted">{saveStatus}</p>}
      </div>
    </div>
  );
}
