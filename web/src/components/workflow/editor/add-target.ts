/// ブロックの追加先（スマホの縦一覧の「＋」・⋯ の「ブロックを追加」・パレットのタップ）と、
/// 追加先 → reducer action の写像。キャンバスの座標は既存配置の近くへ置く（座標は化粧品）。

import type { Edge, Node as IrNode, WorkflowIr } from "@/generated/workflow-ir";
import type { EditorLayout } from "@/lib/workflow-api";
import { nodePortLabel } from "../port-labels";
import { NODE_W, nextPosition, resolvePositions } from "./auto-layout";
import { frameRects, insideOrigin, NODE_H, type Point, type Rect } from "./frames";
import type { EditorAction } from "./ir-state";
import { continuationPort, isStructuralPort, outputPorts, type ServerPorts } from "./ports";
import { blockName as nameOf, childrenOf } from "./regions";

export type AddTarget =
  /// ノードが 1 つも無い（最初のブロック）。
  | { kind: "first" }
  /// 中にブロックの無い繰り返し `map` の中の最初のブロック。
  | { kind: "inside"; map: string }
  /// `from` の `port` の後ろにつないで追加する（下流がまだ無い出口）。
  | { kind: "after"; from: string; port: string }
  /// 既存のつながりの間に割り込ませる。
  | { kind: "edge"; edge: Edge }
  /// 入口ノードの手前（きっかけの直後）に挿入する。
  | { kind: "before"; before: string };

/// 「流れの最後」: 先頭の入口から既定の出口を辿った最後のノードの後ろ（無ければ最初のブロック）。分岐の後では
/// 最初の出口（「はい」など）の側の最後になるので、どこに入るかは [`targetPlace`] で言う（BUG-106）。
/// `serverPorts` はサーバが導いた出口（判定のノードの出口・検証 API の `node_ports`）。
/// 繰り返しの中のブロックは数えない（本体の流れの最後・繰り返しの中の最後は [`regionEndTarget`]）。
export function endTarget(ir: WorkflowIr, serverPorts: ServerPorts = {}): AddTarget {
  const body = ir.nodes.filter((n) => !n.parent);
  if (body.length === 0) return { kind: "first" };
  return flowEnd(ir, body, serverPorts);
}

/// 繰り返し `mapId` の中の流れの最後（中が空なら中の最初のブロック）。
export function regionEndTarget(ir: WorkflowIr, mapId: string, serverPorts: ServerPorts = {}): AddTarget {
  const inside = childrenOf(ir, mapId);
  if (inside.length === 0) return { kind: "inside", map: mapId };
  return flowEnd(ir, inside, serverPorts);
}

/// 同じ中（本体・1 つの繰り返しの中）のブロックのうち、先頭の入口から既定の出口を辿った最後のブロックの後ろ。
function flowEnd(ir: WorkflowIr, members: IrNode[], serverPorts: ServerPorts): AddTarget {
  const incoming = new Set(ir.edges.map((e) => e.to));
  let current = members.find((n) => !incoming.has(n.id)) ?? members[0];
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

/// 枠の右に空ける間。
const FRAME_GAP = 48;

function overlaps(a: Rect, b: Rect): boolean {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

/// 新しいブロックの座標を、追加先の外の繰り返しの枠と重ならない位置へずらす（枠の右へ送る）。枠に重なって置くと、
/// 本体のブロックなのに繰り返しの中にあるように見え、少し動かすと枠に入ってしまう（キャンバスのドラッグの判定）。
/// 追加先の繰り返しとその外側の枠は避けない（中に足したブロックで枠が広がる）。
function clearOfFrames(p: Point, target: AddTarget, ir: WorkflowIr, frames: Map<string, Rect>): Point {
  const keep = new Set<string>();
  for (let id = targetRegion(target, ir); id; id = ir.nodes.find((n) => n.id === id)?.parent ?? null) keep.add(id);
  const obstacles = [...frames].filter(([id]) => !keep.has(id)).map(([, r]) => r);
  let q = { ...p };
  for (let i = 0; i < obstacles.length; i += 1) {
    const hit = obstacles.find((r) => overlaps({ ...q, width: NODE_W, height: NODE_H }, r));
    if (!hit) break;
    q = { x: hit.x + hit.width + FRAME_GAP, y: q.y };
  }
  return q;
}

/// 追加先＋ノード種 → reducer action（座標は現在の配置から決める）。
export function addAction(
  target: AddTarget,
  nodeType: string,
  ir: WorkflowIr,
  layout: EditorLayout,
): EditorAction {
  const positions = resolvePositions(ir, layout).nodes;
  const frames = frameRects(ir, positions, layout.sizes ?? {});
  // 線の元の座標（繰り返しは枠の右端に揃える。枠の座標は左上なので、枠の幅の分ずらす・キャンバスの尻尾の＋と同じ）。
  const tail = (id: string): Point => {
    const frame = frames.get(id);
    return frame ? { x: frame.x + frame.width - NODE_W, y: frame.y } : (positions[id] ?? { x: 0, y: 0 });
  };
  const place = (p: Point) => clearOfFrames(p, target, ir, frames);
  switch (target.kind) {
    case "after": {
      return {
        type: "add_node",
        nodeType,
        position: place(nextPosition(tail(target.from), positions)),
        from: { id: target.from, port: target.port },
      };
    }
    case "edge": {
      const a = tail(target.edge.from);
      const b = positions[target.edge.to] ?? a;
      return {
        type: "insert_on_edge",
        nodeType,
        edge: target.edge,
        position: place({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 + 40 }),
      };
    }
    case "before": {
      const b = positions[target.before] ?? { x: 0, y: 0 };
      return {
        type: "insert_before",
        nodeType,
        before: target.before,
        position: place({ x: b.x - NODE_W - 96, y: b.y }),
      };
    }
    case "inside": {
      // 枠の左上（見出しの下）に置く。
      const map = ir.nodes.find((n) => n.id === target.map);
      const frame = frameRects(ir, positions, layout.sizes ?? {}).get(target.map);
      const anchor = frame ?? positions[target.map] ?? { x: 0, y: 0 };
      return {
        type: "add_node",
        nodeType,
        position: map ? insideOrigin(map, anchor) : anchor,
        parent: target.map,
      };
    }
    default:
      return { type: "add_node", nodeType, position: { x: 0, y: 0 } };
  }
}

function blockName(node: IrNode | undefined, fallback: string): string {
  return node ? nameOf(node) : fallback;
}

/// 追加先が繰り返しの中なら、その繰り返しの id（本体なら null）。ブロックを選ぶメニューで、中に置けない種類を
/// 灰色にするのに使う。
export function targetRegion(target: AddTarget, ir: WorkflowIr): string | null {
  const parentOf = (id: string) => ir.nodes.find((n) => n.id === id)?.parent ?? null;
  switch (target.kind) {
    case "inside":
      return target.map;
    case "after":
      return parentOf(target.from);
    case "edge":
      return parentOf(target.edge.from);
    case "before":
      return parentOf(target.before);
    default:
      return null;
  }
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
    case "inside":
      return `「${blockName(byId(target.map), target.map)}」の中`;
    default:
      return null;
  }
}

/// ブロックを選ぶシートの文脈の文（「{場所}に追加」・最初のブロックは「最初のブロックを追加」）。
export function targetLabel(target: AddTarget, ir: WorkflowIr, serverPorts: ServerPorts = {}): string {
  const place = targetPlace(target, ir, serverPorts);
  return place ? `${place}に追加` : "最初のブロックを追加";
}
