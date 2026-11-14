/// 繰り返しの枠の大きさと位置（描画を持たない・キャンバス・自動配置・reducer の追加先が同じ規則で使う）。
///
/// 座標は IR に入れず layout に持つ（`positions` はどのノードもキャンバスの絶対座標・`sizes` は枠を広げた大きさ）。
/// 枠は、繰り返しのブロックの座標（左上の目安）と中のブロックをすべて囲む矩形で、利用者が広げた大きさ（`sizes`）より
/// 小さくならない。中のブロックを枠の端へ動かせば枠が広がり、枠ごと動かせば中も同じだけ動く（キャンバスが中の座標も
/// 一緒に送る）。

import type { Node as IrNode, WorkflowIr } from "@/generated/workflow-ir";
import { outputPorts } from "./ports";
import { childrenOf, isMap, nestingOf } from "./regions";

/// 通常のブロック（NodeCard）の幅と高さの目安（自動配置・追加先の座標も同じ値を使う）。
export const NODE_W = 240;
export const NODE_H = 88;

export const FRAME = {
  /// 中のブロックと枠の左右の間（右は中のブロックの尻尾の＋が枠の中に入る幅）。
  padX: 52,
  /// 見出しと中のブロックの間。
  padTop: 24,
  /// 中のブロックの下（「中にブロックを追加」のボタンの分）。
  padBottom: 60,
  /// 枠の最小の大きさ（中が空のときに「中にブロックを追加」と案内が入る大きさ）。
  minW: 440,
  minH: 220,
} as const;

export type Rect = { x: number; y: number; width: number; height: number };
export type Point = { x: number; y: number };
export type FrameSizes = Record<string, { width: number; height: number }>;

/// 枠の見出しの高さ（出口が 2 つ以上なら、出口の行の分だけ高い・NodeCard と同じ並び）。
export function frameHeaderHeight(node: IrNode): number {
  const ports = outputPorts(node).length;
  return ports > 1 ? 64 + 13 + 24 * ports : 64;
}

/// 通常のブロックの高さの目安（名前付きの出口の行が増えると高くなる・NodeCard の並び）。
export function cardHeight(node: IrNode): number {
  const ports = outputPorts(node).length;
  return ports > 1 ? 62 + 13 + 24 * ports : NODE_H;
}

/// 繰り返しの中の最初のブロックを置く位置（枠の座標 `anchor` から）。
export function insideOrigin(node: IrNode, anchor: Point): Point {
  return { x: anchor.x + FRAME.padX, y: anchor.y + frameHeaderHeight(node) + FRAME.padTop };
}

/// 繰り返しの枠（絶対座標）。入れ子の枠は中の枠から先に決める。座標の無い繰り返しは載せない。
export function frameRects(ir: WorkflowIr, positions: Record<string, Point>, sizes: FrameSizes = {}): Map<string, Rect> {
  const out = new Map<string, Rect>();
  const maps = ir.nodes.filter((n) => isMap(n) && positions[n.id]);
  // 深いものから（中の枠の大きさが外の枠に効く）。
  maps.sort((a, b) => nestingOf(ir, b.id) - nestingOf(ir, a.id));
  for (const m of maps) {
    const anchor = positions[m.id];
    const stored = sizes[m.id];
    const minW = Math.max(FRAME.minW, stored?.width ?? 0);
    const minH = Math.max(FRAME.minH, stored?.height ?? 0);
    const kids = childrenOf(ir, m.id)
      .map((c) => out.get(c.id) ?? (positions[c.id] ? { ...positions[c.id], width: NODE_W, height: cardHeight(c) } : null))
      .filter((r): r is Rect => r !== null);
    if (kids.length === 0) {
      out.set(m.id, { x: anchor.x, y: anchor.y, width: minW, height: minH });
      continue;
    }
    const header = frameHeaderHeight(m);
    const left = Math.min(anchor.x, ...kids.map((k) => k.x - FRAME.padX));
    const top = Math.min(anchor.y, ...kids.map((k) => k.y - header - FRAME.padTop));
    const right = Math.max(left + minW, ...kids.map((k) => k.x + k.width + FRAME.padX));
    const bottom = Math.max(top + minH, ...kids.map((k) => k.y + k.height + FRAME.padBottom));
    out.set(m.id, { x: left, y: top, width: right - left, height: bottom - top });
  }
  return out;
}

/// 中身を囲むのに要る枠の最小の大きさ（利用者が縮められる下限・`sizes` を除いて決める）。
export function contentMinSize(
  ir: WorkflowIr,
  mapId: string,
  positions: Record<string, Point>,
): { width: number; height: number } {
  const rect = frameRects(ir, positions, {}).get(mapId);
  return rect ? { width: rect.width, height: rect.height } : { width: FRAME.minW, height: FRAME.minH };
}

/// ノードの矩形（繰り返しは枠・ほかはブロックの目安の大きさ）。
export function nodeRect(node: IrNode, positions: Record<string, Point>, frames: Map<string, Rect>): Rect | null {
  const frame = frames.get(node.id);
  if (frame) return frame;
  const p = positions[node.id];
  return p ? { ...p, width: NODE_W, height: cardHeight(node) } : null;
}

export function contains(rect: Rect, p: Point): boolean {
  return p.x >= rect.x && p.x <= rect.x + rect.width && p.y >= rect.y && p.y <= rect.y + rect.height;
}

/// 点を含む枠のうち、いちばん深いもの（無ければ null）。`exclude` の枠（動かしているブロックとその中）は数えない。
export function frameAt(ir: WorkflowIr, frames: Map<string, Rect>, p: Point, exclude: ReadonlySet<string> = new Set()): string | null {
  let best: string | null = null;
  let bestDepth = -1;
  for (const [id, rect] of frames) {
    if (exclude.has(id) || !contains(rect, p)) continue;
    const depth = nestingOf(ir, id);
    if (depth > bestDepth) {
      best = id;
      bestDepth = depth;
    }
  }
  return best;
}
