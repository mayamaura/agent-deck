import { useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open } from "@tauri-apps/plugin-dialog";
import type { AgentDefinitionDto, AgentSettings } from "./types";
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
  const [definition, setDefinition] = useState<AgentDefinitionDto | null>(null);
  const [definitionError, setDefinitionError] = useState<string | null>(null);
  const [defForm, setDefForm] = useState<DefinitionFormState>(EMPTY_DEF_FORM);
  const [defSaveStatus, setDefSaveStatus] = useState<string | null>(null);

  const [form, setForm] = useState<ConfigFormState>(EMPTY_FORM);
  const [saveStatus, setSaveStatus] = useState<string | null>(null);

  function loadDefinition(d: AgentDefinitionDto) {
    setDefinition(d);
    setDefForm({
      name: d.name,
      description: d.description,
      model: d.model ?? "",
      tools: decomposeAgentTools(d.tools),
      body: d.body,
    });
  }

  useEffect(() => {
    document.title = `エージェント設定 — ${agentId}`;
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
      await invoke("save_agent_config", { agentId, settings });
      setSaveStatus("✅ 保存しました");
      await emit(AGENTS_CHANGED);
    } catch (e) {
      setSaveStatus(`⚠ 保存に失敗しました: ${e}`);
    }
  }

  async function handleSaveDefinition() {
    setDefSaveStatus(null);
    try {
      await invoke("save_agent_definition", {
        agentId,
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

  async function handleDeleteDefinition() {
    if (!window.confirm(`エージェント定義「${agentId}」を削除しますか?`)) return;
    try {
      await invoke("delete_agent_definition", { agentId });
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
      await invoke("duplicate_agent", { agentId });
      await emit(AGENTS_CHANGED);
      // 複製後は個人優先で解決され直すため、編集可能な個人版を再取得する。
      loadDefinition(await invoke<AgentDefinitionDto>("get_agent_definition", { agentId }));
    } catch (e) {
      setDefinitionError(String(e));
    }
  }

  return (
    <div className="editor-window">
      <h2>{agentId}</h2>
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
