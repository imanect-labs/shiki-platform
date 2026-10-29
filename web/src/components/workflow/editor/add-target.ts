/// ブロックの追加先（スマホの縦一覧の「＋」・⋯ の「ブロックを追加」・パレットのタップ）と、
/// 追加先 → reducer action の写像。キャンバスの座標は既存配置の近くへ置く（座標は化粧品）。

import type { Edge, Node as IrNode, WorkflowIr } from "@/generated/workflow-ir";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { EditorLayout } from "@/lib/workflow-api";
import { nodePortLabel } from "../port-labels";
import { NODE_W, nextPosition, resolvePositions } from "./auto-layout";
import type { EditorAction } from "./ir-state";
import { continuationPort, isStructuralPort, outputPorts, type ServerPorts } from "./ports";

export type AddTarget =
  /// ノードが 1 つも無い（最初のブロック）。
  | { kind: "first" }
  /// `from` の `port` の後ろにつないで追加する（下流がまだ無い出口）。
  | { kind: "after"; from: string; port: string }
  /// 既存のつながりの間に割り込ませる。
  | { kind: "edge"; edge: Edge }
  /// 入口ノードの手前（きっかけの直後）に挿入する。
  | { kind: "before"; before: string };

/// 「流れの最後」: 先頭の入口から既定の出口を辿った最後のノードの後ろ（無ければ最初のブロック）。分岐の後では
/// 最初の出口（「はい」など）の側の最後になるので、どこに入るかは [`targetPlace`] で言う（BUG-106）。
/// `serverPorts` はサーバが導いた出口（判定のノードの出口・検証 API の `node_ports`）。
export function endTarget(ir: WorkflowIr, serverPorts: ServerPorts = {}): AddTarget {
  if (ir.nodes.length === 0) return { kind: "first" };
  const incoming = new Set(ir.edges.map((e) => e.to));
  let current = ir.nodes.find((n) => !incoming.has(n.id)) ?? ir.nodes[0];
  const seen = new Set<string>();
  for (;;) {
    seen.add(current.id);
    // 続きの処理の出口（error・timeout へはつながない・尻尾の＋と同じ）。
    const port = continuationPort(current.type, outputPorts(current, serverPorts[current.id]));
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

function blockName(node: IrNode | undefined, fallback: string): string {
  return node?.label || NODE_CATALOG.find((c) => c.type === node?.type)?.label_ja || fallback;
}

/// 追加先の場所の句（「{A}」の後ろ／「{A}」の「{出口}」の後ろ／「{A}」と「{B}」の間／「{A}」の前・付録 D の
/// N2.10）。続きの処理の出口が 2 つ以上あるブロック（分岐・振り分け・判定）の後ろでは、どの出口かも言う。最初の
/// ブロックは null。
export function targetPlace(target: AddTarget, ir: WorkflowIr, serverPorts: ServerPorts = {}): string | null {
  const byId = (id: string) => ir.nodes.find((n) => n.id === id);
  switch (target.kind) {
    case "after": {
      const node = byId(target.from);
      const name = blockName(node, target.from);
      const exits = node ? outputPorts(node, serverPorts[node.id]).filter((p) => !isStructuralPort(p)) : [];
      return exits.length > 1 || isStructuralPort(target.port)
        ? `「${name}」の「${nodePortLabel(node, target.port)}」の後ろ`
        : `「${name}」の後ろ`;
    }
    case "edge":
      return `「${blockName(byId(target.edge.from), target.edge.from)}」と「${blockName(byId(target.edge.to), target.edge.to)}」の間`;
    case "before":
      return `「${blockName(byId(target.before), target.before)}」の前`;
    default:
      return null;
  }
}

/// ブロックを選ぶシートの文脈の文（「{場所}に追加」・最初のブロックは「最初のブロックを追加」）。
export function targetLabel(target: AddTarget, ir: WorkflowIr, serverPorts: ServerPorts = {}): string {
  const place = targetPlace(target, ir, serverPorts);
  return place ? `${place}に追加` : "最初のブロックを追加";
}
