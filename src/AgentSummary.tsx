import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Md, formatDuration, formatJst, groupHistory } from "./App";
import type { HistoryEntry } from "./types";

/** list_history に渡す「全件」相当の上限(App.tsx の HISTORY_ALL と同じ意味)。 */
const HISTORY_ALL = 100_000;

const STATUS_LABEL: Record<HistoryEntry["status"], string> = {
  completed: "✅ 完了",
  failed: "❌ 失敗",
  cancelled: "⏹ 中断",
};

/**
 * エージェント 1 件のサマリウインドウ(?summary=<id>)。履歴(history.jsonl)から
 * 「これまでの会話」と「作った成果物」を 1 画面に並べる。表示専用で、ファイルは
 * open_chat_link(エージェントのフォルダ配下のみ直接開く)で開く。
 */
export default function AgentSummary({ agentId }: { agentId: string }) {
  const [rows, setRows] = useState<HistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  function reload() {
    invoke<HistoryEntry[]>("list_history", { limit: HISTORY_ALL })
      .then((all) => {
        setRows(all.filter((r) => r.agentId === agentId));
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }
  useEffect(reload, [agentId]);

  function open(target: string) {
    invoke("open_chat_link", { agentId, target }).catch((e) => setError(String(e)));
  }

  const sessions = rows ? groupHistory(rows) : [];
  // 新しい順に並んでいる履歴から、同じパスは最新の1件だけ残す(上書き再生成されたファイルの重複除去)。
  const files = new Map<string, string>();
  for (const r of rows ?? []) for (const f of r.outputFiles) if (!files.has(f)) files.set(f, r.startedAt);
  const done = (rows ?? []).filter((r) => r.status === "completed").length;
  const tokens = (rows ?? []).reduce((sum, r) => sum + (r.totalTokens ?? 0), 0);

  return (
    <div className="editor-window">
      <h2>{agentId} のサマリ</h2>
      <p>
        <button type="button" onClick={reload}>
          ↻ 更新
        </button>{" "}
        <button type="button" onClick={() => invoke("open_output_folder", { agentId }).catch((e) => setError(String(e)))}>
          📂 出力フォルダ
        </button>
      </p>
      {error && <p className="error">⚠ {error}</p>}
      {rows === null ? (
        <p className="muted">読み込み中…</p>
      ) : rows.length === 0 ? (
        <p className="muted">このエージェントの実行履歴はまだありません</p>
      ) : (
        <>
          <p className="muted">
            会話 {sessions.length} 件 / 実行 {rows.length} 回(完了 {done})/ 累計 {tokens.toLocaleString()} トークン / 最終実行{" "}
            {formatJst(rows[0].startedAt)}
          </p>

          <h3>成果物({files.size})</h3>
          {files.size === 0 ? (
            <p className="muted">出力ファイルはありません</p>
          ) : (
            <ul>
              {[...files].map(([f, at]) => (
                <li key={f}>
                  <a href="#" title={f} onClick={(e) => (e.preventDefault(), open(f))}>
                    {f.split(/[\\/]/).pop()}
                  </a>{" "}
                  <span className="muted">{formatJst(at)}</span>
                </li>
              ))}
            </ul>
          )}

          <h3>会話</h3>
          {sessions.map(({ latest, runs, topic }) => {
            // 継続依頼は同じ sessionId で行が増える。古い順に並べ直して 1 つの会話として見せる。
            const turns = rows.filter((r) => r.sessionId === latest.sessionId).reverse();
            return (
              <details key={latest.sessionId}>
                <summary>
                  {formatJst(latest.startedAt)} {STATUS_LABEL[latest.status]} — {topic.trim().split(/\r?\n/)[0].slice(0, 60)}
                  {runs > 1 && <span className="muted"> 💬 {runs} 回</span>}
                </summary>
                {turns.map((t, i) => (
                  <div key={i}>
                    <p className="muted">
                      {formatJst(t.startedAt)} · {formatDuration(t.durationMs)}
                    </p>
                    <blockquote>{t.prompt}</blockquote>
                    <Md text={t.summary || "(この実行の結果概要は記録されていません)"} agentId={agentId} />
                    {t.outputFiles.map((f) => (
                      <div key={f}>
                        📄{" "}
                        <a href="#" title={f} onClick={(e) => (e.preventDefault(), open(f))}>
                          {f.split(/[\\/]/).pop()}
                        </a>
                      </div>
                    ))}
                  </div>
                ))}
              </details>
            );
          })}
        </>
      )}
    </div>
  );
}
