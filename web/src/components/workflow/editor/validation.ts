/// サーバ検証エラー（全件・node_id/edge/path 付き）のノード/エッジへの写像。
///
/// backend は `{ code, message, node_id?, edge?("from -> to"), path?("/params/...") }` を
/// 全件返す（ir.md §8）。dnd は該当ノード上にバッジ表示し、右パネルはフィールドへ写像する。

import type { ValidationError } from "@/generated/workflow-ir";

export type ErrorMap = {
  byNode: Map<string, ValidationError[]>;
  byEdge: Map<string, ValidationError[]>;
  global: ValidationError[];
};

export function mapErrors(errors: ValidationError[]): ErrorMap {
  const byNode = new Map<string, ValidationError[]>();
  const byEdge = new Map<string, ValidationError[]>();
  const global: ValidationError[] = [];
  for (const e of errors) {
    if (e.node_id) {
      const list = byNode.get(e.node_id) ?? [];
      list.push(e);
      byNode.set(e.node_id, list);
    } else if (e.edge) {
      const list = byEdge.get(e.edge) ?? [];
      list.push(e);
      byEdge.set(e.edge, list);
    } else {
      global.push(e);
    }
  }
  return { byNode, byEdge, global };
}

/// `/params/<field>/...` の先頭フィールド名（右パネルのフィールドハイライト用）。
/// サーバの params の読み取りの誤り（serde の原文 `params が不正です: missing field \`data\`` など）から、
/// 欄の名前と利用者向けの文言を取り出す（欄の赤字と上の帯に同じ内容が英語で二重に出ていた・C2 N11）。
export function serdeParamError(message: string): { field: string; message: string } | null {
  const missing = /missing field `([a-z_][a-z0-9_]*)`/.exec(message);
  if (missing) return { field: missing[1], message: "入力してください" };
  const invalid = /invalid (?:type|value)[^`]*?(?:at|for) `?([a-z_][a-z0-9_]*)`?/.exec(message);
  if (invalid) return { field: invalid[1], message: "値の形が正しくありません" };
  return null;
}

export function paramField(path: string | undefined | null): string | null {
  if (!path?.startsWith("/params/")) return null;
  const rest = path.slice("/params/".length);
  const head = rest.split("/")[0];
  return head || null;
}
