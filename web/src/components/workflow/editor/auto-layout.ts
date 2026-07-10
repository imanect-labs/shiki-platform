/// dagre による自動レイアウト（座標未保存ノードの初期配置・AI 生成 IR の表示）。
///
/// 座標は IR に入れない（deny-unknown・ir_version 据え置き）。保存済み layout を優先し、
/// 欠けているノードだけ dagre の層状配置で埋める（既存配置を壊さない）。

import dagre from "dagre";
import type { WorkflowIr } from "@/generated/workflow-ir";
import type { EditorLayout } from "@/lib/workflow-api";

export const NODE_W = 240;
export const NODE_H = 88;
const TRIGGER_H = 56;

export type Positions = Record<string, { x: number; y: number }>;

/// IR＋保存済み layout から全表示要素（トリガ擬似ノード含む）の座標を決める。
export function resolvePositions(ir: WorkflowIr, layout: EditorLayout): {
  nodes: Positions;
  triggers: Positions;
} {
  const saved = layout.positions ?? {};
  const savedTriggers = layout.triggers ?? {};
  const missing = ir.nodes.some((n) => !saved[n.id]);

  let computed: Positions = {};
  if (missing && ir.nodes.length > 0) {
    const g = new dagre.graphlib.Graph();
    g.setGraph({ rankdir: "LR", nodesep: 48, ranksep: 96 });
    g.setDefaultEdgeLabel(() => ({}));
    for (const n of ir.nodes) {
      g.setNode(n.id, { width: NODE_W, height: NODE_H });
    }
    for (const e of ir.edges) {
      g.setEdge(e.from, e.to);
    }
    dagre.layout(g);
    for (const n of ir.nodes) {
      const pos = g.node(n.id);
      if (pos) computed[n.id] = { x: pos.x - NODE_W / 2, y: pos.y - NODE_H / 2 };
    }
    // 保存済みが一部あるなら、その平均オフセットへ寄せる（部分追加でも既存の近くに出す）。
    const anchors = ir.nodes.filter((n) => saved[n.id] && computed[n.id]);
    if (anchors.length > 0) {
      const dx =
        anchors.reduce((a, n) => a + (saved[n.id].x - computed[n.id].x), 0) / anchors.length;
      const dy =
        anchors.reduce((a, n) => a + (saved[n.id].y - computed[n.id].y), 0) / anchors.length;
      computed = Object.fromEntries(
        Object.entries(computed).map(([id, p]) => [id, { x: p.x + dx, y: p.y + dy }]),
      );
    }
  }

  const nodes: Positions = {};
  for (const n of ir.nodes) {
    nodes[n.id] = saved[n.id] ?? computed[n.id] ?? { x: 0, y: 0 };
  }

  // トリガ擬似ノード: 保存があれば従い、無ければ本体の左上に縦積み。
  const triggers: Positions = {};
  const minX = Math.min(0, ...Object.values(nodes).map((p) => p.x));
  const minY = Math.min(0, ...Object.values(nodes).map((p) => p.y));
  ir.triggers.forEach((_, i) => {
    const key = `trigger:${i}`;
    triggers[key] =
      savedTriggers[key] ?? { x: minX - NODE_W - 80, y: minY + i * (TRIGGER_H + 24) };
  });
  return { nodes, triggers };
}

/// 既存ノードの右隣（同じ高さ）の空き座標（プラスボタンでの追加先）。
export function nextPosition(from: { x: number; y: number }, taken: Positions): {
  x: number;
  y: number;
} {
  const base = { x: from.x + NODE_W + 96, y: from.y };
  let candidate = { ...base };
  let bump = 0;
  const collides = (p: { x: number; y: number }) =>
    Object.values(taken).some(
      (q) => Math.abs(q.x - p.x) < NODE_W * 0.8 && Math.abs(q.y - p.y) < NODE_H * 1.2,
    );
  while (collides(candidate) && bump < 20) {
    bump += 1;
    candidate = { x: base.x, y: base.y + bump * (NODE_H + 32) };
  }
  return candidate;
}
