/// dagre による自動レイアウト（座標未保存ノードの初期配置・AI 生成 IR の表示）。
///
/// 座標は IR に入れない（deny-unknown・ir_version 据え置き）。保存済み layout を優先し、
/// 欠けているノードだけ dagre の層状配置で埋める（既存配置を壊さない）。
/// エディタは開いた時点で `seedLayout` により全座標を手元に持つので、ここの dagre と平均オフセットが
/// 効くのは、開いた後に座標なしで現れたノード（AI が後から足したものなど）だけになる。
/// 繰り返し（control.map）の中は、中のブロックだけで並べてから繰り返しの枠の中に置き、外の並びでは繰り返しを枠の
/// 大きさのブロックとして扱う（枠がほかのブロックに重ならない・frames.ts）。

import dagre from "dagre";
import type { Trigger, WorkflowIr } from "@/generated/workflow-ir";
import type { EditorLayout } from "@/lib/workflow-api";
import { FRAME, NODE_H, NODE_W, cardHeight, frameHeaderHeight, insideOrigin, type FrameSizes } from "./frames";
import { isMap, nestingOf } from "./regions";

export { NODE_H, NODE_W };
const TRIGGER_H = 56;

export type Positions = Record<string, { x: number; y: number }>;

/// 旧形式のきっかけの座標キー（`trigger:<添字>`）。前のきっかけを消すと別のきっかけに付け替わるため廃止した。
const LEGACY_TRIGGER_KEY = /^trigger:\d+$/;

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
    computed = layoutLevel(ir, null, layout.sizes ?? {}).positions;
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
  // 座標の無い中のブロックは、自動配置での繰り返しからの位置を、繰り返しの今の座標に足して置く（枠の外に出さない）。
  // 入れ子は外の繰り返しから順に決める。
  if (missing) {
    const byId = new Map(ir.nodes.map((n) => [n.id, n]));
    for (const n of [...ir.nodes].sort((a, b) => nestingOf(ir, a.id) - nestingOf(ir, b.id))) {
      const map = n.parent ? byId.get(n.parent) : undefined;
      if (saved[n.id] || !map || !isMap(map) || !computed[n.id] || !computed[map.id]) continue;
      nodes[n.id] = {
        x: nodes[map.id].x + computed[n.id].x - computed[map.id].x,
        y: nodes[map.id].y + computed[n.id].y - computed[map.id].y,
      };
    }
  }

  // トリガ擬似ノード: 保存があれば従い、無ければ本体の左上に縦積み。
  const triggers: Positions = {};
  const minX = Math.min(0, ...Object.values(nodes).map((p) => p.x));
  const minY = Math.min(0, ...Object.values(nodes).map((p) => p.y));
  triggerKeys(ir.triggers).forEach((key, i) => {
    triggers[key] =
      savedTriggers[key] ?? { x: minX - NODE_W - 80, y: minY + i * (TRIGGER_H + 24) };
  });
  return { nodes, triggers };
}

/// 1 つの段（本体、または 1 つの繰り返しの中）を dagre で並べる。繰り返しは中を先に並べて枠の大きさのブロックにし、
/// 中のブロックの座標を枠の中へ写す。中の段は左上を (0, 0) にそろえる（枠の中の位置は呼び出し側が足す）。本体の段は
/// dagre の座標のまま（繰り返しの無い定義は前と同じ配置になる）。
function layoutLevel(
  ir: WorkflowIr,
  parent: string | null,
  sizes: FrameSizes,
  visiting: ReadonlySet<string> = new Set(),
): { positions: Positions; width: number; height: number } {
  // 入れ先の無い（消えた・繰り返しでない）parent のブロックは本体に並べる（保存の検証が誤りを言う）。
  const mapIds = new Set(ir.nodes.filter((n) => isMap(n)).map((n) => n.id));
  const levelOf = (n: WorkflowIr["nodes"][number]) => (n.parent && mapIds.has(n.parent) ? n.parent : null);
  const members = ir.nodes.filter((n) => levelOf(n) === parent);
  if (members.length === 0) return { positions: {}, width: 0, height: 0 };
  const ids = new Set(members.map((n) => n.id));
  const inner = new Map<string, Positions>();
  const dims = new Map<string, { width: number; height: number }>();
  for (const m of members) {
    if (isMap(m) && !visiting.has(m.id)) {
      const sub = layoutLevel(ir, m.id, sizes, new Set([...visiting, m.id]));
      inner.set(m.id, sub.positions);
      const header = frameHeaderHeight(m);
      dims.set(m.id, {
        width: Math.max(FRAME.minW, sizes[m.id]?.width ?? 0, sub.width + 2 * FRAME.padX),
        height: Math.max(
          FRAME.minH,
          sizes[m.id]?.height ?? 0,
          sub.width > 0 ? header + FRAME.padTop + sub.height + FRAME.padBottom : 0,
        ),
      });
    } else {
      dims.set(m.id, { width: NODE_W, height: parent === null ? NODE_H : cardHeight(m) });
    }
  }
  const g = new dagre.graphlib.Graph();
  g.setGraph({ rankdir: "LR", nodesep: 48, ranksep: 96 });
  g.setDefaultEdgeLabel(() => ({}));
  for (const m of members) g.setNode(m.id, dims.get(m.id)!);
  for (const e of ir.edges) {
    if (ids.has(e.from) && ids.has(e.to)) g.setEdge(e.from, e.to);
  }
  dagre.layout(g);
  const positions: Positions = {};
  for (const m of members) {
    const pos = g.node(m.id);
    const d = dims.get(m.id)!;
    if (pos) positions[m.id] = { x: pos.x - d.width / 2, y: pos.y - d.height / 2 };
  }
  let width = 0;
  let height = 0;
  if (parent !== null) {
    const minX = Math.min(...Object.values(positions).map((p) => p.x));
    const minY = Math.min(...Object.values(positions).map((p) => p.y));
    for (const id of Object.keys(positions)) {
      positions[id] = { x: positions[id].x - minX, y: positions[id].y - minY };
      width = Math.max(width, positions[id].x + dims.get(id)!.width);
      height = Math.max(height, positions[id].y + dims.get(id)!.height);
    }
  }
  for (const m of members) {
    const sub = inner.get(m.id);
    const at = positions[m.id];
    if (!sub || !at) continue;
    const origin = insideOrigin(m, at);
    for (const [id, p] of Object.entries(sub)) positions[id] = { x: origin.x + p.x, y: origin.y + p.y };
  }
  return { positions, width, height };
}

/// 繰り返しの枠を描く前の版で保存した配置か（中にブロックのある繰り返しがあり、保存した配置に枠の版の印が無い）。
///
/// 前の版は中のブロックを本体と同じ並びに置いていた（チャットで作った定義をエディタで動かして保存したもの）。その配置
/// のまま枠で囲むと、枠が外のブロックまで囲んでしまうので、開いたときに一度だけ自動で並べ直す（印は次の配置の保存で
/// 付く・[`layoutForSave`]）。
function savedBeforeFrames(ir: WorkflowIr, layout: EditorLayout): boolean {
  if (layout.frames || Object.keys(layout.positions ?? {}).length === 0) return false;
  return ir.nodes.some((n) => n.parent && isMap(ir.nodes.find((m) => m.id === n.parent)));
}

/// 開いた時点の layout を、全ノード・全きっかけの座標で埋めた手元の初期値にする（これ自体は保存しない）。
///
/// 座標の無いノードが混ざったままだと、描画のたびに dagre で全体を計算し直し、保存済みのノードとの
/// 平均のずれだけ全体を平行移動する。AI が作ったワークフロー（layout なし）では 1 つ動かすと全体が
/// 同じだけ動いてしまう（BUG-2）。最初に一度だけ決めて固定し、利用者が何かを動かしたら全座標が保存される。
/// 旧形式のきっかけの座標（`trigger:<添字>`）は、開いた時点の並びで新しいキーへ読み替えて捨てる。
export function seedLayout(ir: WorkflowIr, given: EditorLayout): EditorLayout {
  const layout: EditorLayout = savedBeforeFrames(ir, given) ? { sizes: given.sizes } : given;
  const saved = layout.triggers ?? {};
  const triggers: Positions = {};
  for (const [key, p] of Object.entries(saved)) {
    if (!LEGACY_TRIGGER_KEY.test(key)) triggers[key] = p;
  }
  triggerKeys(ir.triggers).forEach((key, i) => {
    const legacy = saved[`trigger:${i}`];
    if (!triggers[key] && legacy) triggers[key] = legacy;
  });
  const resolved = resolvePositions(ir, { ...layout, triggers });
  return {
    ...layout,
    positions: { ...(layout.positions ?? {}), ...resolved.nodes },
    triggers: { ...triggers, ...resolved.triggers },
  };
}

/// 保存する layout（今の IR にあるノード・きっかけの座標と、繰り返しの枠の大きさだけ）。
/// 手元では消したノードの座標も残し（Undo で同じ位置に戻すため）、保存のときに落とす。
export function layoutForSave(ir: WorkflowIr, layout: EditorLayout): EditorLayout {
  const ids = new Set(ir.nodes.map((n) => n.id));
  const maps = new Set(ir.nodes.filter((n) => isMap(n)).map((n) => n.id));
  const keys = new Set(triggerKeys(ir.triggers));
  const pick = <T>(from: Record<string, T> | undefined, keep: Set<string>) =>
    Object.fromEntries(Object.entries(from ?? {}).filter(([k]) => keep.has(k)));
  const out: EditorLayout = {
    ...layout,
    positions: pick(layout.positions, ids),
    triggers: pick(layout.triggers, keys),
    // 繰り返しの枠を描く版で保存した印（開くたびに前の版の配置として並べ直さない・[`savedBeforeFrames`]）。
    frames: 1,
  };
  if (layout.sizes) out.sizes = pick(layout.sizes, maps);
  return out;
}

/// きっかけの座標キー（キャンバスの擬似ノード id を兼ねる）。
///
/// IR のきっかけには id が無いため、種類と内容（保存の往復で形が変わらない項目だけ）から決める。
/// 添字だと、前のきっかけを消したときに座標が別のきっかけへ付け替わる（BUG-25）。
/// 内容がまったく同じきっかけが複数あるときだけ、2 つ目以降に出現順の番号を付ける。
/// 内容を編集してキーが変わるときは `carryTriggerPositions` で座標を引き継ぐ。
export function triggerKeys(triggers: readonly Trigger[]): string[] {
  const seen = new Map<string, number>();
  return triggers.map((t) => {
    const base = `trigger:${t.kind}:${hash(canonical(triggerIdentity(t)))}`;
    const n = (seen.get(base) ?? 0) + 1;
    seen.set(base, n);
    return n === 1 ? base : `${base}:${n}`;
  });
}

/// きっかけを置き換えたとき（種類・内容の編集、追加、削除）に、座標を新しいキーへ引き継ぐ。
/// 同じオブジェクトのまま残ったきっかけはそのキーから、置き換わったものは数が同じなら同じ位置のものから写す。
/// 古いキーの座標は消さない（Undo で元の内容に戻したときに同じ位置へ出すため）。
export function carryTriggerPositions(
  prev: readonly Trigger[],
  next: readonly Trigger[],
  saved: Positions,
): Positions {
  const prevKeys = triggerKeys(prev);
  const nextKeys = triggerKeys(next);
  let out = saved;
  next.forEach((t, j) => {
    const i = prev.indexOf(t);
    const from = i >= 0 ? prevKeys[i] : prev.length === next.length ? prevKeys[j] : undefined;
    const to = nextKeys[j];
    if (!from || from === to || !saved[from]) return;
    if (out === saved) out = { ...saved };
    out[to] = saved[from];
  });
  return out;
}

/// キーに使うきっかけの内容。サーバが既定値を補う項目（catchup）や省略可の条件（filter）は入れない
/// （保存して開き直したときにキーが変わって座標を失わないようにする）。
function triggerIdentity(t: Trigger): unknown {
  switch (t.kind) {
    case "schedule":
      return { cron: t.cron, tz: t.tz };
    case "event":
      return { source: t.source, scope: t.scope };
    default:
      return { label: (t as { label?: string | null }).label ?? null };
  }
}

/// キーの順序と null の有無に依らない JSON 表現。
function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
      .filter(([, v]) => v !== null && v !== undefined)
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(",")}}`;
  }
  return JSON.stringify(value ?? null);
}

/// FNV-1a（32bit）。座標キーを短くするためだけに使う（衝突しても座標が重なるだけ）。
function hash(s: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i += 1) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(36);
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
