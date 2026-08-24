import { visit } from "unist-util-visit";
import type { Element, ElementContent, Root } from "hast";

/**
 * チャット本文中のファイルパスをリンク化する rehype プラグイン。
 * 開く処理自体は App.tsx の <a> クリックハンドラ(Rust の open_chat_link)が行う。
 *
 * - インラインコード: 全体がパスらしければ(相対パス・拡張子付きファイル名も)リンク化
 * - 地の文: 絶対パス(ドライブレター・UNC)のみ。相対パスは普通の語句との誤検出が多いので拾わない
 *
 * ponytail: 空白を含むパスは地の文では検出できない(語の区切りと区別できない)。
 * バッククォート囲みなら拾えるので、必要になったらそちらへ誘導する。
 */

/** 地の文から拾う絶対パス。空白・引用符・約物(和文含む)で途切れる。 */
const ABS_PATH = /(?:[A-Za-z]:[\\/]|\\\\)[^\s"'<>|?*()、。()「」,;]+/g;

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

/** テキストを「絶対パス→リンク」で分割する。パスが無ければ null。 */
export function splitAbsPaths(value: string): ElementContent[] | null {
  ABS_PATH.lastIndex = 0;
  const out: ElementContent[] = [];
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = ABS_PATH.exec(value))) {
    // 文末にパスが来たとき句読点を巻き込まないよう末尾を落とす(Windows のパスは . で終われない)
    const raw = m[0].replace(/[.,;:]+$/, "");
    if (m.index > last) out.push({ type: "text", value: value.slice(last, m.index) });
    out.push(link(raw));
    last = m.index + raw.length;
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
