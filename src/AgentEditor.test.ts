import { describe, expect, it } from "vitest";
import { sameForm } from "./AgentEditor";

describe("sameForm(未保存判定)", () => {
  it("チェックを付け外しして順序が変わっただけなら同じ扱い", () => {
    const a = { allowedTools: { checked: ["read", "write"], other: "" } };
    const b = { allowedTools: { checked: ["write", "read"], other: "" } };
    expect(sameForm(a, b)).toBe(true);
  });

  it("値が変わっていれば未保存とみなす", () => {
    expect(sameForm({ outputDir: "C:/out" }, { outputDir: "C:/other" })).toBe(false);
  });

  it("チェックが増えていれば未保存とみなす", () => {
    const a = { deniedTools: { checked: ["shell"], other: "" } };
    const b = { deniedTools: { checked: ["shell", "write"], other: "" } };
    expect(sameForm(a, b)).toBe(false);
  });

  it("真偽値の変更も拾う", () => {
    expect(sameForm({ autoApprove: true }, { autoApprove: false })).toBe(false);
  });
});
