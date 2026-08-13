/** ツールを列挙するチェックボックスの選択肢と、値リスト⇔チェック状態の変換ロジック。
 * 対象は語彙が異なる2箇所(docs/sdk-notes.md「組み込みツールの語彙」参照):
 *   A. 定義エディタの `.agent.md` tools フィールド(公式エイリアス)
 *   B. 入出力設定の許可/拒否ツール(Copilot CLI の権限パターン。CLI フラグ語彙)
 */

export interface ToolOption {
  value: string;
  label: string;
  /** チェックボックスの下に表示する注記(write の非推奨注記など)。 */
  note?: string;
}

/** A: `.agent.md` の tools に書ける公式エイリアス。
 * 出典: https://docs.github.com/en/copilot/how-tos/agents/custom-agents/custom-agents-configuration
 * (2026-08-13 確認。docs/sdk-notes.md 参照)。特殊値 `["*"]`(全許可)/ `[]`(全禁止)/
 * MCP の `server/tool` はここには含めない(UI 側で null/空配列/その他欄として扱う)。 */
export const AGENT_TOOL_OPTIONS: ToolOption[] = [
  { value: "execute", label: "コマンド実行(シェル)" },
  { value: "read", label: "ファイル読み取り" },
  { value: "edit", label: "ファイル作成・編集" },
  { value: "search", label: "ファイル・テキスト検索" },
  { value: "agent", label: "他エージェントへの委任" },
  { value: "web", label: "Web 取得・検索" },
  { value: "todo", label: "タスクリスト管理" },
];

/** B: 入出力設定の許可/拒否ツール(権限パターン)の基本形。 */
export const PERMISSION_TOOL_OPTIONS: ToolOption[] = [
  { value: "shell", label: "シェル実行(すべて)" },
  { value: "read", label: "ファイル読み取り" },
  { value: "url", label: "URL アクセス(web_fetch 等)" },
  {
    value: "write",
    label: "ファイル書き込み(すべてのフォルダ)",
    note: "⚠ 出力フォルダの外への書き込みまで無承認になります。通常はすぐ下の「出力フォルダの中のみ」を使ってください",
  },
];

function splitList(text: string): string[] {
  return text
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/** A用フォーム状態。mode="all" が tools: null(全ツール)に対応する。 */
export interface AgentToolsFormState {
  mode: "all" | "selected";
  checked: string[];
  other: string;
}

/** tools: string[] | null → フォーム状態。未知の値(MCP の server/tool 形式や互換名)は
 * 消さず other にカンマ区切りで残す。 */
export function decomposeAgentTools(tools: string[] | null): AgentToolsFormState {
  if (tools === null) return { mode: "all", checked: [], other: "" };
  const known = new Set(AGENT_TOOL_OPTIONS.map((o) => o.value));
  return {
    mode: "selected",
    checked: AGENT_TOOL_OPTIONS.map((o) => o.value).filter((v) => tools.includes(v)),
    other: tools.filter((t) => !known.has(t)).join(", "),
  };
}

/** フォーム状態 → tools: string[] | null(保存値)。 */
export function composeAgentTools(state: AgentToolsFormState): string[] | null {
  if (state.mode === "all") return null;
  return [...state.checked, ...splitList(state.other)];
}

/** B用フォーム状態。 */
export interface PermissionToolsFormState {
  checked: string[];
  other: string;
}

/** 許可/拒否ツールの文字列配列 → フォーム状態。基本形4種以外(括弧付きパターン等)は
 * other にカンマ区切りで残す。「常に許可」機能が追記したパターンもここを通る。 */
export function decomposePermissionTools(patterns: string[]): PermissionToolsFormState {
  const known = new Set(PERMISSION_TOOL_OPTIONS.map((o) => o.value));
  return {
    checked: PERMISSION_TOOL_OPTIONS.map((o) => o.value).filter((v) => patterns.includes(v)),
    other: patterns.filter((p) => !known.has(p)).join(", "),
  };
}

/** フォーム状態 → 許可/拒否ツールの文字列配列(保存値)。 */
export function composePermissionTools(state: PermissionToolsFormState): string[] {
  return [...state.checked, ...splitList(state.other)];
}
