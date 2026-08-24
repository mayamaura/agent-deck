import { visit } from "unist-util-visit";
import type { Element, ElementContent, Root } from "hast";

/**
 * チャット本文中のファイルパスをリンク化する rehype プラグイン。
 * 開く処理自体は App.tsx の <a> クリックハンドラ(Rust の open_chat_link)が行う。
 *
 * - インラインコード: 全体がパスらしければ(相対パス・拡張子付きファイル名も)リンク化
 * - 地の文: 絶対パス(ドライブレター・UNC)のみ。相対パスは普通の語句との誤検出が多いので拾わない
 *
 * 地の文の空白を含むパスはヒューリスティックで対応する(splitAbsPaths 参照):
 * - 引用符(" ' 「」『』)で囲まれていれば囲み全体を 1 つのパスとして扱う
 * - 裸のパスは語を先読みし、区切り(\ /)を含む語まで ASCII の語 2 つを上限に橋渡しする。
 *   拡張子付きの語(report.md)は直後 1 語だけ繋げる
 * ponytail: 空白入りフォルダ名で終わる裸のパス(C:\My Documents)は検出できない。
 * 引用符かバッククォートで囲まれていれば拾える。
 */

/** 絶対パスの始まり(ドライブレター or UNC)。 */
const ABS_START = /[A-Za-z]:[\\/]|\\\\/g;

/** 空白区切りの 1 語。引用符・約物で途切れる(日本語のファイル名と
 * 半角括弧 — Program Files (x86) — は許す。全角括弧は地の文の約物とみなす)。 */
const TOKEN = /^[^\s"'「」『』<>|?*、。,;（）]+/;

/** 引用符の対応表。囲まれた絶対パスは空白ごと 1 つのパスとして扱う。 */
const QUOTE_PAIRS: Record<string, string> = { '"': '"', "'": "'", "「": "」", "『": "』" };

/** インラインコードの中身が丸ごと「開けるパス」らしいか。 */
export function looksLikePath(s: string): boolean {
  if (/[\r\n]/.test(s)) return false;
  if (/^(?:[A-Za-z]:[\\/]|\\\\)[^"'<>|?*]+$/.test(s)) return true; // 絶対(空白可)
  if (/^[^\s:"'<>|?*]+[\\/][^:"'<>|?*]+$/.test(s)) return true; // 区切りを含む相対
  return /^[^\s\\/:"'<>|?*]+\.[A-Za-z0-9]{1,10}$/.test(s); // 拡張子付きファイル名単体
}

function link(href: string): Element {
  return {
    type: "element",
    tagName: "a",
    properties: { href },
    children: [{ type: "text", value: href }],
  };
}

function tokenAt(value: string, pos: number): string {
  const m = TOKEN.exec(value.slice(pos));
  return m ? m[0] : "";
}

/** start(絶対パスの始まり)からパス 1 つ分を読み取る。 */
function readPath(value: string, start: number): string | null {
  // 引用符囲みなら閉じまでを丸ごとパスにする(空白入りフォルダ名はこれで拾う)
  const close = QUOTE_PAIRS[value[start - 1] ?? ""];
  if (close) {
    const end = value.indexOf(close, start);
    if (end > start) return trimPath(value.slice(start, end));
  }
  let end = start + tokenAt(value, start).length;
  let cursor = end;
  let bridged = 0; // 橋渡し中(未確定)の語数
  while (value[cursor] === " ") {
    const tok = tokenAt(value, cursor + 1);
    if (!tok || /^(?:[A-Za-z]:[\\/]|\\\\)/.test(tok)) break; // 次のパスの始まり
    if (/[\\/]/.test(tok)) {
      // 区切りを含む語: 橋渡し分ごとパスとして確定
      cursor += 1 + tok.length;
      end = cursor;
      bridged = 0;
    } else if (bridged === 0 && /^[^\\/]+\.[A-Za-z0-9]{1,10}$/.test(tok)) {
      // 拡張子付きの語は直後 1 語だけ繋げる(例: 「…\my report.md」の report.md)
      cursor += 1 + tok.length;
      end = cursor;
    } else if (/^[!-~]+$/.test(tok) && bridged < 2) {
      // ASCII の語だけ橋渡し候補にする。和文の助詞・語句を跨いだ誤結合
      // (例: 「C:\out から output\x.md を作成」)を防ぐ
      bridged += 1;
      cursor += 1 + tok.length;
    } else {
      break;
    }
  }
  return trimPath(value.slice(start, end));
}

/** パス末尾の掃除。文末の句読点と、拡張子直後に続く和文(…レポート.mdに保存)を落とす。 */
function trimPath(raw: string): string | null {
  let p = raw;
  // 拡張子の直後に非 ASCII が続き、以降に区切りが無ければパスはそこで終わっている
  const ext = /\.[A-Za-z0-9]{1,10}(?![A-Za-z0-9])/g;
  let m: RegExpExecArray | null;
  while ((m = ext.exec(p))) {
    const end = m.index + m[0].length;
    const rest = p.slice(end);
    if (rest && /^[^\x00-\x7F]/.test(rest) && !/[\\/]/.test(rest)) {
      p = p.slice(0, end);
      break;
    }
  }
  p = p.replace(/[.,;:]+$/, "");
  // 「(C:\out\report.md)」のような括弧閉じ。パス内に ( が無いときだけ落とす
  if (!p.includes("(")) p = p.replace(/[).,;:]+$/, "");
  return p.length > 3 ? p : null; // "C:\" 単体などは対象外
}

/** テキストを「絶対パス→リンク」で分割する。パスが無ければ null。 */
export function splitAbsPaths(value: string): ElementContent[] | null {
  ABS_START.lastIndex = 0;
  const out: ElementContent[] = [];
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = ABS_START.exec(value))) {
    const prev = value[m.index - 1];
    if (prev && /[A-Za-z0-9]/.test(prev)) continue; // 語の途中(例: "ABC:\x")は誤検出
    const path = readPath(value, m.index);
    if (!path) continue;
    if (m.index > last) out.push({ type: "text", value: value.slice(last, m.index) });
    out.push(link(path));
    last = m.index + path.length;
    ABS_START.lastIndex = last;
  }
  if (out.length === 0) return null;
  if (last < value.length) out.push({ type: "text", value: value.slice(last) });
  return out;
}

export function rehypeLinkifyPaths() {
  return (tree: Root) => {
    // インラインコード(pre 配下でない code)の中身が丸ごとパスならリンクで包む
    visit(tree, "element", (node, _index, parent) => {
      if (node.tagName !== "code") return;
      if (parent?.type === "element" && parent.tagName === "pre") return;
      const child = node.children[0];
      if (node.children.length === 1 && child.type === "text" && looksLikePath(child.value)) {
        node.children = [link(child.value)];
      }
    });
    // 地の文の絶対パス(リンク・コード内は対象外)
    visit(tree, "text", (node, index, parent) => {
      if (!parent || index === undefined) return;
      if (parent.type === "element" && (parent.tagName === "a" || parent.tagName === "code")) return;
      const parts = splitAbsPaths(node.value);
      if (!parts) return;
      parent.children.splice(index, 1, ...parts);
      return index + parts.length; // 差し替えた分を飛ばして続行
    });
  };
}
