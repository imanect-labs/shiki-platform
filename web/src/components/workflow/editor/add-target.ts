/// ブロックの追加先（スマホの縦一覧の「＋」・⋯ の「ブロックを追加」・パレットのタップ）と、
/// 追加先 → reducer action の写像。キャンバスの座標は既存配置の近くへ置く（座標は化粧品）。

import type { Edge, WorkflowIr } from "@/generated/workflow-ir";
import type { EditorLayout } from "@/lib/workflow-api";
import { NODE_W, nextPosition, resolvePositions } from "./auto-layout";
import type { EditorAction } from "./ir-state";
import { outputPorts } from "./node-card";

export type AddTarget =
  /// ノードが 1 つも無い（最初のブロック）。
  | { kind: "first" }
  /// `from` の `port` の後ろにつないで追加する（下流がまだ無い出口）。
  | { kind: "after"; from: string; port: string }
  /// 既存のつながりの間に割り込ませる。
  | { kind: "edge"; edge: Edge }
  /// 入口ノードの手前（きっかけの直後）に挿入する。
  | { kind: "before"; before: string };

/// 「流れの末尾」: 先頭の入口から既定の出口を辿った最後のノードの後ろ（無ければ最初のブロック）。
export function endTarget(ir: WorkflowIr): AddTarget {
  if (ir.nodes.length === 0) return { kind: "first" };
  const incoming = new Set(ir.edges.map((e) => e.to));
  let current = ir.nodes.find((n) => !incoming.has(n.id)) ?? ir.nodes[0];
  const seen = new Set<string>();
  for (;;) {
    seen.add(current.id);
    const port = outputPorts(current)[0] ?? "out";
    const next = ir.edges.find((e) => e.from === current.id && (e.from_port ?? "out") === port);
    const node = next ? ir.nodes.find((n) => n.id === next.to) : undefined;
    if (!node || seen.has(node.id)) return { kind: "after", from: current.id, port };
    current = node;
  }
}

/// 追加先＋ノード種 → reducer action（座標は現在の配置から決める）。
export function addAction(
  target: AddTarget,
  nodeType: string,
  ir: WorkflowIr,
  layout: EditorLayout,
): EditorAction {
  const positions = resolvePositions(ir, layout).nodes;
  switch (target.kind) {
    case "after": {
      const from = positions[target.from] ?? { x: 0, y: 0 };
      return {
        type: "add_node",
        nodeType,
        position: nextPosition(from, positions),
        from: { id: target.from, port: target.port },
      };
    }
    case "edge": {
      const a = positions[target.edge.from] ?? { x: 0, y: 0 };
      const b = positions[target.edge.to] ?? a;
      return {
        type: "insert_on_edge",
        nodeType,
        edge: target.edge,
        position: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 + 40 },
      };
    }
    case "before": {
      const b = positions[target.before] ?? { x: 0, y: 0 };
      return {
        type: "insert_before",
        nodeType,
        before: target.before,
        position: { x: b.x - NODE_W - 96, y: b.y },
      };
    }
    default:
      return { type: "add_node", nodeType, position: { x: 0, y: 0 } };
  }
}
