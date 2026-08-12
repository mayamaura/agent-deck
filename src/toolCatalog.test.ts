import { describe, expect, it } from "vitest";
import {
  composeAgentTools,
  composePermissionTools,
  decomposeAgentTools,
  decomposePermissionTools,
} from "./toolCatalog";

describe("decomposeAgentTools / composeAgentTools", () => {
  it("null は全ツール(mode: all)になる", () => {
    expect(decomposeAgentTools(null)).toEqual({ mode: "all", checked: [], other: "" });
    expect(composeAgentTools({ mode: "all", checked: ["read"], other: "foo" })).toBeNull();
  });

  it("空配列は選択なし(ツールなし)になる。全ツールとは区別される", () => {
    expect(decomposeAgentTools([])).toEqual({ mode: "selected", checked: [], other: "" });
    expect(composeAgentTools({ mode: "selected", checked: [], other: "" })).toEqual([]);
  });

  it("既知の値はチェック、未知の値(MCP server/tool 形式など)は other に振り分ける", () => {
    const state = decomposeAgentTools(["read", "edit", "mcp-server/tool", "*legacy"]);
    expect(state.mode).toBe("selected");
    expect(state.checked).toEqual(["read", "edit"]);
    expect(state.other).toBe("mcp-server/tool, *legacy");
  });

  it("往復で値が保たれる", () => {
    const original = ["execute", "web", "custom/tool"];
    const state = decomposeAgentTools(original);
    const roundTripped = composeAgentTools(state);
    expect(roundTripped).toEqual(expect.arrayContaining(original));
    expect(roundTripped).toHaveLength(original.length);
  });
});

describe("decomposePermissionTools / composePermissionTools", () => {
  it("空配列は選択なしになる(権限側は null との区別が不要)", () => {
    expect(decomposePermissionTools([])).toEqual({ checked: [], other: "" });
    expect(composePermissionTools({ checked: [], other: "" })).toEqual([]);
  });

  it("既知の基本形はチェック、括弧付きパターン等は other に振り分ける", () => {
    const state = decomposePermissionTools(["shell", "shell(python:*)", "write", "shell(rm)"]);
    expect(state.checked).toEqual(["shell", "write"]);
    expect(state.other).toBe("shell(python:*), shell(rm)");
  });

  it("往復で値が保たれる(AllowRuleAdded が追記したパターンも含む)", () => {
    const original = ["read", "shell(git:*)"];
    const state = decomposePermissionTools(original);
    const roundTripped = composePermissionTools(state);
    expect(roundTripped).toEqual(expect.arrayContaining(original));
    expect(roundTripped).toHaveLength(original.length);
  });
});
