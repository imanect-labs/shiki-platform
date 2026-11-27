/// 繰り返し（`control.map`）の中のブロック（IR の `parent`）の扱い（描画を持たない・キャンバスの枠・スマホの字下げ・
/// reducer・追加先が同じ規則で使う・ir.md §5.3）。
///
/// - 中のブロックは `parent` がその繰り返しの id のノード。中の線は中のブロックどうしだけをつなぐ（中と外をまたぐ線は
///   保存で `ir.region_leak` になる）。繰り返しから中へ線は引かない（中の入口は前から線が来ないブロック）。
/// - 繰り返しの入れ子は 2 段まで（`ir.region_too_deep`）。「結果を画面に返す」は中に置けない（`ir.reply_in_map`）。
///   置けないものは、置く前にその理由を言う（追加のメニュー・ドロップ）。

import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { Edge, Node as IrNode, WorkflowIr } from "@/generated/workflow-ir";

export const MAP_TYPE = "control.map";
/// 繰り返しを入れ子にできる段数（ir.md §5.3）。
export const MAX_MAP_DEPTH = 2;
const REPLY_TYPE = "workflow.reply";

export function isMap(node: { type: string } | undefined | null): boolean {
  return node?.type === MAP_TYPE;
}

/// ブロックの種類の名前（カタログの単一定義）。
export function typeLabel(nodeType: string): string {
  return NODE_CATALOG.find((c) => c.type === nodeType)?.label_ja ?? nodeType;
}

/// ブロック名（付けた名前・無ければ種類の名前・それも無ければ id）。
export function blockName(node: IrNode | undefined, fallback = ""): string {
  if (!node) return fallback;
  return node.label || NODE_CATALOG.find((c) => c.type === node.type)?.label_ja || node.id;
}

/// 中のブロック（`parent` が `mapId` のノード・入れ子の中は含まない）。
export function childrenOf(ir: WorkflowIr, mapId: string): IrNode[] {
  return ir.nodes.filter((n) => n.parent === mapId);
}

/// `id` の中にあるブロックすべて（入れ子の中も含む・`id` 自身は含まない）。
export function descendantsOf(ir: WorkflowIr, id: string): Set<string> {
  const out = new Set<string>();
  const queue = [id];
  while (queue.length > 0) {
    const cur = queue.shift()!;
    for (const n of ir.nodes) {
      if (n.parent === cur && !out.has(n.id)) {
        out.add(n.id);
        queue.push(n.id);
      }
    }
  }
  return out;
}

/// ノードの入れ子の深さ（本体は 0・繰り返しの中は 1・その中の繰り返しの中は 2）。輪になった parent は打ち切る。
export function nestingOf(ir: WorkflowIr, id: string): number {
  const byId = new Map(ir.nodes.map((n) => [n.id, n]));
  let depth = 0;
  let cur = byId.get(id)?.parent ?? null;
  const seen = new Set<string>();
  while (cur && byId.has(cur) && !seen.has(cur)) {
    seen.add(cur);
    depth += 1;
    cur = byId.get(cur)?.parent ?? null;
  }
  return depth;
}

/// `id` から下の繰り返しの段数（繰り返しでなければ 0・中に繰り返しの無い繰り返しは 1）。
function mapLevels(ir: WorkflowIr, id: string): number {
  const node = ir.nodes.find((n) => n.id === id);
  if (!isMap(node)) return 0;
  let inner = 0;
  for (const c of childrenOf(ir, id)) inner = Math.max(inner, mapLevels(ir, c.id));
  return 1 + inner;
}

/// 種類 `nodeType` のブロック（`moving` は動かす既存のブロック・その中身ごと）を `parent` の中に置けないときの理由
/// （置けるなら null）。本体（`parent` が null）にはいつでも置ける。
export function placeRefusal(
  ir: WorkflowIr,
  nodeType: string,
  parent: string | null,
  moving?: string,
): string | null {
  if (!parent) return null;
  const map = typeLabel(MAP_TYPE);
  if (nodeType === REPLY_TYPE) {
    return `「${typeLabel(REPLY_TYPE)}」は「${map}」の中に置けません。繰り返しの後ろに置いてください。`;
  }
  // 置いた後の繰り返しの段数（入れ先の深さ＋動かすブロックの中の段数）が 2 段を超えるなら置けない。
  const levels = moving ? mapLevels(ir, moving) : nodeType === MAP_TYPE ? 1 : 0;
  if (levels > 0 && nestingOf(ir, parent) + 1 + levels > MAX_MAP_DEPTH) {
    return `「${map}」の中に置けるのは 2 段までです。`;
  }
  return null;
}

/// `parent` の中に追加できないブロックの種類と理由（ブロックを選ぶメニューで灰色にする）。中でなければ空。
export function refusedTypesIn(ir: WorkflowIr, parent: string | null): Readonly<Record<string, string>> {
  if (!parent) return NO_REFUSALS;
  const out: Record<string, string> = {};
  for (const t of [REPLY_TYPE, MAP_TYPE]) {
    const reason = placeRefusal(ir, t, parent);
    if (reason) out[t] = reason;
  }
  return out;
}

const NO_REFUSALS: Readonly<Record<string, string>> = Object.freeze({});

/// ブロック `id` を `parent` の中（null は本体）へ移した IR。中と外をまたぐことになる線（`id` と、移した先の外の
/// ブロックの間の線）は外す（残すと保存が `ir.region_leak` で止まる）。外した線の数も返す。
export function moveIntoRegion(
  ir: WorkflowIr,
  id: string,
  parent: string | null,
): { ir: WorkflowIr; dropped: number } {
  const parentOf = new Map(ir.nodes.map((n) => [n.id, n.parent ?? null]));
  parentOf.set(id, parent);
  const crosses = (e: Edge) =>
    (e.from === id || e.to === id) && (parentOf.get(e.from) ?? null) !== (parentOf.get(e.to) ?? null);
  const edges = ir.edges.filter((e) => !crosses(e));
  return {
    ir: {
      ...ir,
      nodes: ir.nodes.map((n) => (n.id === id ? ({ ...n, parent } as IrNode) : n)),
      edges,
    },
    dropped: ir.edges.length - edges.length,
  };
}

/// ノードを親（繰り返し）が先に来る順に並べる（キャンバスは親の枠を子より先に渡す必要がある）。同じ深さは IR の順。
export function parentsFirst(ir: WorkflowIr): IrNode[] {
  const depth = new Map(ir.nodes.map((n) => [n.id, nestingOf(ir, n.id)]));
  return [...ir.nodes].sort((a, b) => (depth.get(a.id) ?? 0) - (depth.get(b.id) ?? 0));
}
