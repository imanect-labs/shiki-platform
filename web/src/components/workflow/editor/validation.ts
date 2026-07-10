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
export function paramField(path: string | undefined | null): string | null {
  if (!path?.startsWith("/params/")) return null;
  const rest = path.slice("/params/".length);
  const head = rest.split("/")[0];
  return head || null;
}
