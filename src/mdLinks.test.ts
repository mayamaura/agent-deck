import { describe, expect, it } from "vitest";
import type { Element, Root, Text } from "hast";
import { looksLikePath, rehypeLinkifyPaths, splitAbsPaths } from "./mdLinks";

describe("looksLikePath(インラインコード判定)", () => {
  it("絶対パス・相対パス・拡張子付きファイル名を認める", () => {
    expect(looksLikePath("C:\\out\\report.md")).toBe(true);
    expect(looksLikePath("C:\\Program Files\\App\\readme.txt")).toBe(true);
    expect(looksLikePath("\\\\server\\share\\a.csv")).toBe(true);
    expect(looksLikePath("output/report.md")).toBe(true);
    expect(looksLikePath("report.md")).toBe(true);
  });

  it("コード片・URL・複数行は認めない", () => {
    expect(looksLikePath("npm run build")).toBe(false);
    expect(looksLikePath("https://example.com/a")).toBe(false);
    expect(looksLikePath("a\nb")).toBe(false);
    expect(looksLikePath("x = 1")).toBe(false);
  });
});

describe("splitAbsPaths(地の文の絶対パス)", () => {
  it("和文に埋まった絶対パスをリンクに分割する", () => {
    const parts = splitAbsPaths("結果を C:\\out\\report.md に保存しました。");
    expect(parts).not.toBeNull();
    expect(parts!.map((p) => p.type)).toEqual(["text", "element", "text"]);
    const a = parts![1] as Element;
    expect(a.properties.href).toBe("C:\\out\\report.md");
  });

  it("文末の句読点を巻き込まない", () => {
    const parts = splitAbsPaths("保存先は C:\\out\\report.md.");
    const a = parts![1] as Element;
    expect(a.properties.href).toBe("C:\\out\\report.md");
  });

  it("相対パスや普通の文はリンク化しない", () => {
    expect(splitAbsPaths("output/report.md に保存しました")).toBeNull();
    expect(splitAbsPaths("処理が完了しました")).toBeNull();
  });
});

describe("rehypeLinkifyPaths(hast 変換)", () => {
  const text = (value: string): Text => ({ type: "text", value });
  const el = (tagName: string, children: Element["children"]): Element => ({
    type: "element",
    tagName,
    properties: {},
    children,
  });

  it("インラインコードのパスをリンクで包み、コードブロックは触らない", () => {
    const tree: Root = {
      type: "root",
      children: [
        el("p", [el("code", [text("output\\report.md")])]),
        el("pre", [el("code", [text("report.md")])]),
      ],
    };
    rehypeLinkifyPaths()(tree);
    const inline = (tree.children[0] as Element).children[0] as Element;
    expect((inline.children[0] as Element).tagName).toBe("a");
    const block = (tree.children[1] as Element).children[0] as Element;
    expect(block.children[0].type).toBe("text");
  });

  it("既存リンクの中のテキストは二重リンク化しない", () => {
    const a = el("a", [text("C:\\out\\report.md")]);
    const tree: Root = { type: "root", children: [el("p", [a])] };
    rehypeLinkifyPaths()(tree);
    expect(a.children[0].type).toBe("text");
  });
});
