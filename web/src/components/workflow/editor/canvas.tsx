"use client";

/// キャンバス（React Flow）: ドットグリッド・自由 dnd・右下コントロール・クリックでズームフィット。
///
/// **IR が唯一の情報源**: nodes/edges は IR＋layout＋選択＋エラー写像からの導出値。位置変更は layout、
/// 構造変更は reducer action として親（workflow-editor.tsx）へ返す。
///
/// ノードは React Flow の制御パターンで持つ: 導出値を手元の state へ合流させ、ドラッグ中の位置・
/// 計測した大きさ・選択は `applyNodeChanges` で手元に反映する（制御モードの React Flow は自分では
/// ノードを動かさないので、手元に反映しないとドラッグ中に止まり、離した瞬間に飛ぶ・BUG-2）。
/// reducer へは確定した移動（離したとき）だけを送る。
///
/// 繰り返し（control.map）は枠のノード（map-frame.tsx）で描き、中のブロックはその子ノード（`parentId`）にする
/// （BUG-103・D-85）。layout の座標はどれも絶対座標で持ち、子ノードへは枠の左上からの位置に直して渡す。枠ごと動かすと
/// 中の座標も同じだけ動かして送る。ブロックを枠の中で離すと中に入り、枠の外で離すと外へ出る（中と外をまたぐ線は
/// 外す）。中と外をまたぐ線はつながせない。

import * as React from "react";
import {
  Background,
  BackgroundVariant,
  Controls,
  MiniMap,
  ReactFlow,
  applyNodeChanges,
  useReactFlow,
  type Connection,
  type Edge as RfEdge,
  type EdgeChange,
  type EdgeTypes,
  type Node as RfNode,
  type NodeChange,
  type NodeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";

import type { Edge as IrEdge, Node as IrNode, Trigger, ValidationError, WorkflowIr } from "@/generated/workflow-ir";
import { toast } from "@/components/ui/use-toast";
import type { EditorLayout } from "@/lib/workflow-api";
import { edgeKey, type EditorAction, type Selection } from "./ir-state";
import { mapErrors, triggerErrorsOf } from "./validation";
import { resolvePositions, nextPosition, triggerKeys, NODE_W } from "./auto-layout";
import { addAction, regionEndTarget } from "./add-target";
import { contentMinSize, frameAt, frameRects, insideOrigin, NODE_H, type Point, type Rect } from "./frames";
import { EACH_HANDLE, MapFrame, type MapFrameData } from "./map-frame";
import { NodeCard, type NodeCardData } from "./node-card";
import { TriggerNode, type TriggerNodeData } from "./trigger-node";
import { PlusEdge, type PlusEdgeData } from "./plus-edge";
import { PALETTE_DND_TYPE } from "./palette";
import { edgePortsOf, type ServerPorts } from "./ports";
import {
  blockName as nameOfNode,
  childrenOf,
  descendantsOf,
  isMap,
  moveIntoRegion,
  parentsFirst,
  placeRefusal,
  refusedTypesIn,
} from "./regions";
import { nodePortLabel } from "../port-labels";

/// ブロック名（付けた名前・無ければカタログの名前）。
function blockName(node: IrNode | undefined, fallback: string): string {
  return node ? nameOfNode(node) : fallback;
}

const nodeTypes: NodeTypes = {
  irNode: NodeCard as never,
  mapFrame: MapFrame as never,
  trigger: TriggerNode as never,
};
const edgeTypes: EdgeTypes = {
  plus: PlusEdge as never,
};

/// エラー・警告の無いノードに渡す配列（毎回新しい [] を渡すとノードが毎回作り直される）。
const NO_ERRORS: ValidationError[] = [];
const EDGE_PREFIX = "e:";

type Props = {
  ir: WorkflowIr;
  layout: EditorLayout;
  selection: Selection;
  serverErrors: ValidationError[];
  /// 保存を止めない警告（ノードに注意の印を出す）。
  serverWarnings?: ValidationError[];
  /// サーバが導いた出口（判定のノードのハンドル・検証 API の `node_ports`）。
  serverPorts?: ServerPorts;
  /// `serverPorts` を導いたときの各ノードの params（今の設定の出口かを見る）。
  serverPortsParams?: Readonly<Record<string, unknown>>;
  dispatch: React.Dispatch<EditorAction>;
};

function isTrigger(id: string): boolean {
  return id.startsWith("trigger:");
}

/// 導出したノードを手元の state へ合流させる。
/// - 同じ id の既存ノードは `measured` を引き継ぐ（捨てると毎回 visibility:hidden で描いて測り直す）。
/// - ドラッグ中のノードは手元の位置を優先する（ドラッグ中に検証結果などで導出し直しても掴んだ位置に留める）。
/// - 見た目に関わる値が変わっていないノードは同じオブジェクトを使い回す（React Flow が同一参照なら
///   内部ノードを作り直さない）。何も変わっていなければ配列ごと使い回す。
function mergeNodes(prev: RfNode[], next: RfNode[]): RfNode[] {
  const byId = new Map(prev.map((n) => [n.id, n]));
  let changed = prev.length !== next.length;
  const merged = next.map((n, i) => {
    const p = byId.get(n.id);
    if (!p) {
      changed = true;
      return n;
    }
    const position = p.dragging ? p.position : n.position;
    if (
      p.type === n.type &&
      p.position.x === position.x &&
      p.position.y === position.y &&
      Boolean(p.selected) === Boolean(n.selected) &&
      p.deletable === n.deletable &&
      p.parentId === n.parentId &&
      p.width === n.width &&
      p.height === n.height &&
      p.ariaLabel === n.ariaLabel &&
      shallowEqual(p.data, n.data)
    ) {
      if (prev[i] !== p) changed = true;
      return p;
    }
    changed = true;
    return { ...n, position, measured: p.measured, dragging: p.dragging };
  });
  return changed ? merged : prev;
}

function shallowEqual(a: Record<string, unknown>, b: Record<string, unknown>): boolean {
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((k) => a[k] === b[k]);
}

const NO_PORTS: ServerPorts = {};
const NO_PARAMS: Readonly<Record<string, unknown>> = {};

/// 枠（`frames`）の中の子ノードの位置（枠の左上から）と、子ノードの位置から絶対座標へ。
function relativeTo(p: Point, frame: Rect | undefined): Point {
  return frame ? { x: p.x - frame.x, y: p.y - frame.y } : p;
}

function absoluteFrom(p: Point, frame: Rect | undefined): Point {
  return frame ? { x: p.x + frame.x, y: p.y + frame.y } : p;
}

export function Canvas({
  ir,
  layout,
  selection,
  serverErrors,
  serverWarnings,
  serverPorts = NO_PORTS,
  serverPortsParams = NO_PARAMS,
  dispatch,
}: Props) {
  const { fitView, screenToFlowPosition } = useReactFlow();
  const errors = React.useMemo(() => mapErrors(serverErrors), [serverErrors]);
  const warnings = React.useMemo(() => mapErrors(serverWarnings ?? []), [serverWarnings]);
  const positions = React.useMemo(() => resolvePositions(ir, layout), [ir, layout]);
  const tKeys = React.useMemo(() => triggerKeys(ir.triggers), [ir.triggers]);
  // 繰り返しの枠（絶対座標・中身と広げた大きさから決まる）。
  const frames = React.useMemo(
    () => frameRects(ir, positions.nodes, layout.sizes ?? {}),
    [ir, positions.nodes, layout.sizes],
  );
  // ドラッグ中のブロックを離すと入る枠（強調する）。
  const [dropTarget, setDropTarget] = React.useState<string | null>(null);

  // 尻尾の＋の追加先は押した時点の配置から決める（ノードの data に毎回新しい関数を入れると、
  // 配置が変わるたびに全ノードが作り直されるため、参照の変わらない関数にして座標は ref で読む）。
  const positionsRef = React.useRef(positions);
  const latest = React.useRef({ ir, layout, frames, serverPorts });
  React.useLayoutEffect(() => {
    positionsRef.current = positions;
    latest.current = { ir, layout, frames, serverPorts };
  }, [positions, ir, layout, frames, serverPorts]);
  const onAddFrom = React.useCallback(
    (fromId: string, port: string, nodeType: string) => {
      const current = positionsRef.current.nodes;
      const from = current[fromId] ?? { x: 0, y: 0 };
      // 繰り返しの後ろは枠の右に置く（枠の座標は左上なので、枠の幅の分ずらす）。
      const frame = latest.current.frames.get(fromId);
      const base = frame ? { x: frame.x + frame.width - NODE_W, y: frame.y } : from;
      dispatch({
        type: "add_node",
        nodeType,
        position: nextPosition(base, current),
        from: { id: fromId, port },
      });
    },
    [dispatch],
  );
  // 枠の中の「中にブロックを追加」: 中の流れの最後（中が空なら中の最初のブロック）に足す。
  const onAddInside = React.useCallback(
    (mapId: string, nodeType: string) => {
      const { ir: cur, layout: lay, serverPorts: ports } = latest.current;
      dispatch(addAction(regionEndTarget(cur, mapId, ports), nodeType, cur, lay));
    },
    [dispatch],
  );
  const onResizeEnd = React.useCallback(
    (mapId: string, size: { width: number; height: number }) => dispatch({ type: "resize_frame", id: mapId, size }),
    [dispatch],
  );

  // 繰り返しの中で後ろ・間に置けない種類（繰り返しごとに同じオブジェクトを使い回す）。
  const refusals = React.useMemo(() => {
    const out = new Map<string, Readonly<Record<string, string>>>();
    for (const n of ir.nodes) if (isMap(n)) out.set(n.id, refusedTypesIn(ir, n.id));
    return out;
  }, [ir]);

  // ── IR → React Flow nodes（導出値）────────────────────────────────────
  const derivedNodes = React.useMemo<RfNode[]>(() => {
    const byId = new Map(ir.nodes.map((n) => [n.id, n]));
    const nodes: RfNode[] = parentsFirst(ir).map((n) => {
      const parentFrame = n.parent ? frames.get(n.parent) : undefined;
      const parentId = parentFrame ? n.parent! : undefined;
      const common: NodeCardData = {
        irNode: n,
        errors: errors.byNode.get(n.id) ?? NO_ERRORS,
        warnings: warnings.byNode.get(n.id) ?? NO_ERRORS,
        knownPorts: serverPorts[n.id],
        portsFresh: serverPortsParams[n.id] === n.params,
        // 出口の一覧は文字列にして渡す（毎回新しい配列だと、変わらないノードまで作り直される）。
        edgePortsKey: JSON.stringify(edgePortsOf(n.id, ir.edges)),
        onAddFrom,
        refusedTypes: parentId ? refusals.get(parentId) : undefined,
      };
      const inside = parentId ? `（「${blockName(byId.get(parentId), parentId)}」の中）` : "";
      const frame = frames.get(n.id);
      if (isMap(n) && frame) {
        const childCount = childrenOf(ir, n.id).length;
        const min = contentMinSize(ir, n.id, positions.nodes);
        return {
          id: n.id,
          type: "mapFrame",
          position: relativeTo(frame, parentFrame),
          parentId,
          width: frame.width,
          height: frame.height,
          // 枠は通常のブロックより下に描く（外のブロックを枠の上へ運んでいる間も見える）。中のブロックは枠より上。
          zIndex: -1,
          selected: selection.kind === "node" && selection.id === n.id,
          ariaLabel: `「${blockName(n, n.id)}」の枠。中のブロック ${childCount} 個${inside}`,
          data: {
            ...common,
            childCount,
            dropTarget: dropTarget === n.id,
            minWidth: min.width,
            minHeight: min.height,
            onAddInside,
            onResizeEnd,
            refusedInside: refusals.get(n.id) ?? {},
          } satisfies MapFrameData,
        };
      }
      return {
        id: n.id,
        type: "irNode",
        position: relativeTo(positions.nodes[n.id], parentFrame),
        parentId,
        selected: selection.kind === "node" && selection.id === n.id,
        ariaLabel: parentId ? `「${blockName(n, n.id)}」${inside}` : undefined,
        data: common,
      };
    });
    ir.triggers.forEach((t, i) => {
      nodes.push({
        id: tKeys[i],
        type: "trigger",
        position: positions.triggers[tKeys[i]],
        selected: selection.kind === "trigger" && selection.index === i,
        // きっかけは IR の triggers[] で、キャンバスからは消さない（設定パネルの「このきっかけを削除」）。
        deletable: false,
        data: { trigger: t, index: i, errors: triggerErrorsOf(errors, t as Trigger, i) } satisfies TriggerNodeData,
      });
    });
    return nodes;
  }, [
    ir,
    tKeys,
    positions,
    frames,
    selection,
    errors,
    warnings,
    serverPorts,
    serverPortsParams,
    onAddFrom,
    onAddInside,
    onResizeEnd,
    refusals,
    dropTarget,
  ]);

  // 導出値が変わったら手元の state へ合流させる（描画中に前回の導出値と比べて更新する）。
  const [nodes, setNodes] = React.useState<RfNode[]>(derivedNodes);
  const [mergedFrom, setMergedFrom] = React.useState(derivedNodes);
  if (mergedFrom !== derivedNodes) {
    setMergedFrom(derivedNodes);
    setNodes((prev) => mergeNodes(prev, derivedNodes));
  }

  // ── IR → React Flow edges（トリガ→エントリの視覚接続を含む）────────────
  const rfEdges = React.useMemo<RfEdge[]>(() => {
    const nodeById = new Map(ir.nodes.map((n) => [n.id, n]));
    const edges: RfEdge[] = ir.edges.map((e) => {
      const key = edgeKey(e);
      const errKey = `${e.from} -> ${e.to}`;
      const region = nodeById.get(e.from)?.parent ?? null;
      return {
        id: `${EDGE_PREFIX}${key}`,
        source: e.from,
        sourceHandle: e.from_port ?? "out",
        target: e.to,
        type: "plus",
        selected: selection.kind === "edge" && selection.key === key,
        data: {
          irEdge: e,
          portName: nodePortLabel(nodeById.get(e.from), e.from_port ?? "out"),
          fromName: blockName(nodeById.get(e.from), e.from),
          toName: blockName(nodeById.get(e.to), e.to),
          errorMessages: (errors.byEdge.get(errKey) ?? []).map((x) => x.message),
          refusedTypes: region ? refusals.get(region) : undefined,
          onInsert: (nodeType: string, position: { x: number; y: number }) =>
            dispatch({
              type: "insert_on_edge",
              nodeType,
              edge: e,
              position: { x: position.x - NODE_W / 2, y: position.y },
            }),
          onDelete: () => dispatch({ type: "delete_elements", nodes: [], edges: [e] }),
        } satisfies PlusEdgeData,
      };
    });
    // トリガ → エントリノード（入エッジ 0 本）への破線（視覚のみ・選択/削除不可）。繰り返しの中のブロックは
    // 実行の始めには動かないので引かない（中の入口へは枠の左の点から引く）。
    const hasIncoming = new Set(ir.edges.map((e) => e.to));
    const entries = ir.nodes.filter((n) => !hasIncoming.has(n.id) && !n.parent);
    for (const key of tKeys) {
      for (const entry of entries) {
        edges.push({
          id: `t:${key}:${entry.id}`,
          source: key,
          target: entry.id,
          type: "default",
          selectable: false,
          deletable: false,
          focusable: false,
          animated: true,
          style: { strokeDasharray: "4 4", opacity: 0.5 },
        });
      }
    }
    // 繰り返しの枠の左の点 → 中の入口（1 件ずつここから入る・視覚のみ）。
    for (const n of ir.nodes) {
      if (!n.parent || hasIncoming.has(n.id) || !frames.has(n.parent)) continue;
      edges.push({
        id: `each:${n.parent}:${n.id}`,
        source: n.parent,
        sourceHandle: EACH_HANDLE,
        target: n.id,
        type: "default",
        selectable: false,
        deletable: false,
        focusable: false,
        style: { strokeDasharray: "4 4", opacity: 0.6 },
      });
    }
    return edges;
  }, [ir.edges, ir.nodes, tKeys, selection, errors, dispatch, refusals, frames]);

  const nodeSelection = React.useCallback(
    (id: string): Selection | null => {
      if (!isTrigger(id)) return { kind: "node", id };
      const index = tKeys.indexOf(id);
      return index >= 0 ? { kind: "trigger", index } : null;
    },
    [tKeys],
  );

  // ── React Flow からの変更を手元と reducer に写像 ───────────────────────
  const onNodesChange = React.useCallback(
    (changes: NodeChange[]) => {
      // 削除は IR から決める（onDelete → reducer）。手元で先に消すと IR と食い違う。
      const local = changes.filter((c) => c.type !== "remove");
      if (local.length > 0) setNodes((ns) => applyNodeChanges(local, ns));

      const { ir: cur, frames: rects } = latest.current;
      const byId = new Map(cur.nodes.map((n) => [n.id, n]));
      const moving = new Set(
        changes.flatMap((c) => (c.type === "position" && c.position && !c.dragging ? [c.id] : [])),
      );
      const moved: Record<string, { x: number; y: number }> = {};
      for (const c of changes) {
        // 確定した移動（離したとき・矢印キー）だけを layout に送る。ドラッグ中は手元だけで動かす。
        if (c.type === "position" && c.position && !c.dragging) {
          if (isTrigger(c.id)) {
            dispatch({ type: "move_trigger", key: c.id, position: c.position });
            continue;
          }
          const node = byId.get(c.id);
          // 枠ごと動かした中のブロックは、枠の分で動かす（二重に足さない）。
          if (node?.parent && moving.has(node.parent)) continue;
          const parentFrame = node?.parent ? rects.get(node.parent) : undefined;
          const abs = absoluteFrom(c.position, parentFrame);
          const frame = rects.get(c.id);
          if (node && frame) {
            // 枠を動かした: 繰り返しの座標と中のブロックすべてを同じだけ動かす。
            const dx = abs.x - frame.x;
            const dy = abs.y - frame.y;
            for (const id of [c.id, ...descendantsOf(cur, c.id)]) {
              const p = positionsRef.current.nodes[id];
              if (p) moved[id] = { x: p.x + dx, y: p.y + dy };
            }
          } else {
            moved[c.id] = abs;
          }
        }
        if (c.type === "select") {
          const target = nodeSelection(c.id);
          if (!target) continue;
          // 選択解除は「今それが選ばれているときだけ」外す（エッジを選んだ通知の後にノードの解除が来る）。
          dispatch(
            c.selected
              ? { type: "select", selection: target }
              : { type: "deselect", selection: target },
          );
        }
      }
      if (Object.keys(moved).length > 0) dispatch({ type: "move", positions: moved });
    },
    [dispatch, nodeSelection],
  );

  // ── 枠への出し入れ（ドラッグで離した位置の枠に入れる）───────────────────
  // ドラッグを始めたときの座標（入れられないときに戻す・Undo で戻す座標）。
  const dragStart = React.useRef<Record<string, Point>>({});
  /// ドラッグ中のブロック `node` の中心（絶対座標）と、それを離すと入る枠（null は本体）。
  const dropInfo = React.useCallback((node: RfNode): { parent: string | null; exclude: Set<string> } | null => {
    const { ir: cur, frames: rects } = latest.current;
    const found = cur.nodes.find((n) => n.id === node.id);
    if (!found) return null;
    const parentFrame = found.parent ? rects.get(found.parent) : undefined;
    const abs = absoluteFrom(node.position, parentFrame);
    const own = rects.get(node.id);
    const w = own?.width ?? node.measured?.width ?? NODE_W;
    const h = own?.height ?? node.measured?.height ?? NODE_H;
    const exclude = new Set([node.id, ...descendantsOf(cur, node.id)]);
    return { parent: frameAt(cur, rects, { x: abs.x + w / 2, y: abs.y + h / 2 }, exclude), exclude };
  }, []);
  const onNodeDragStart = React.useCallback((_: React.MouseEvent, node: RfNode) => {
    const { ir: cur } = latest.current;
    const start: Record<string, Point> = {};
    for (const id of [node.id, ...descendantsOf(cur, node.id)]) {
      const p = positionsRef.current.nodes[id];
      if (p) start[id] = p;
    }
    dragStart.current = start;
  }, []);
  const onNodeDrag = React.useCallback(
    (_: React.MouseEvent, node: RfNode, dragged: RfNode[]) => {
      if (isTrigger(node.id) || dragged.length !== 1) return;
      const info = dropInfo(node);
      const current = latest.current.ir.nodes.find((n) => n.id === node.id)?.parent ?? null;
      const next = info && info.parent !== current ? info.parent : null;
      setDropTarget((prev) => (prev === next ? prev : next));
    },
    [dropInfo],
  );
  const onNodeDragStop = React.useCallback(
    (_: React.MouseEvent, node: RfNode, dragged: RfNode[]) => {
      setDropTarget(null);
      if (isTrigger(node.id) || dragged.length !== 1) return;
      const { ir: cur, frames: rects } = latest.current;
      const irNode = cur.nodes.find((n) => n.id === node.id);
      const info = dropInfo(node);
      if (!irNode || !info) return;
      const current = irNode.parent ?? null;
      if (info.parent === current) return;
      const before = dragStart.current;
      const refusal = placeRefusal(cur, irNode.type, info.parent, isMap(irNode) ? irNode.id : undefined);
      if (refusal) {
        // 入れられない: 元の位置に戻して理由を言う。
        dispatch({ type: "move", positions: before });
        toast({ variant: "destructive", title: "ここには置けません", description: refusal });
        return;
      }
      // 離した位置（枠を動かしたなら中身ごと）。onNodesChange が送った移動と同じ値。
      const parentFrame = current ? rects.get(current) : undefined;
      const abs = absoluteFrom(node.position, parentFrame);
      const own = rects.get(node.id);
      const dx = abs.x - (own?.x ?? positionsRef.current.nodes[node.id]?.x ?? abs.x);
      const dy = abs.y - (own?.y ?? positionsRef.current.nodes[node.id]?.y ?? abs.y);
      const after: Record<string, Point> = {};
      for (const [id, p] of Object.entries(before)) after[id] = { x: p.x + dx, y: p.y + dy };
      if (!own) after[node.id] = abs;
      const { dropped } = moveIntoRegion(cur, irNode.id, info.parent);
      dispatch({ type: "set_parent", id: irNode.id, parent: info.parent, positions: after, before });
      const name = blockName(irNode, irNode.id);
      const where = info.parent
        ? `「${blockName(cur.nodes.find((n) => n.id === info.parent), info.parent)}」の中に入れました。`
        : "繰り返しの外に出しました。";
      toast({
        title: `「${name}」を${where}`,
        description:
          dropped > 0
            ? `中と外をまたぐ線は引けないので、線を ${dropped} 本外しました。必要なら線をつなぎ直してください。`
            : undefined,
      });
    },
    [dispatch, dropInfo],
  );

  const onEdgesChange = React.useCallback(
    (changes: EdgeChange[]) => {
      for (const c of changes) {
        if (c.type !== "select" || !c.id.startsWith(EDGE_PREFIX)) continue;
        const target: Selection = { kind: "edge", key: c.id.slice(EDGE_PREFIX.length) };
        dispatch(
          c.selected
            ? { type: "select", selection: target }
            : { type: "deselect", selection: target },
        );
      }
    },
    [dispatch],
  );

  // Delete キーで消す要素（選んだノードとそのエッジ、または選んだエッジ）を 1 手で消す（undo 1 段）。
  // 繰り返しの枠を消すと中のブロックも消える（reducer が中ごと消す）。
  const onDelete = React.useCallback(
    ({ nodes: removed, edges }: { nodes: RfNode[]; edges: RfEdge[] }) => {
      const ids = removed.filter((n) => !isTrigger(n.id)).map((n) => n.id);
      const irEdges = edges
        .map((e) => (e.data as PlusEdgeData | undefined)?.irEdge)
        .filter((e): e is IrEdge => Boolean(e));
      if (ids.length > 0 || irEdges.length > 0) {
        dispatch({ type: "delete_elements", nodes: ids, edges: irEdges });
      }
    },
    [dispatch],
  );

  // 中と外をまたぐ線・枠の入口の点からの線はつながせない（保存できない線を引かせない）。
  const isValidConnection = React.useCallback((conn: RfEdge | Connection) => {
    if (!conn.source || !conn.target || isTrigger(conn.source) || isTrigger(conn.target)) return false;
    if (conn.sourceHandle === EACH_HANDLE) return false;
    const byId = new Map(latest.current.ir.nodes.map((n) => [n.id, n]));
    return (byId.get(conn.source)?.parent ?? null) === (byId.get(conn.target)?.parent ?? null);
  }, []);

  const onConnect = React.useCallback(
    (conn: Connection) => {
      if (!conn.source || !conn.target) return;
      if (isTrigger(conn.source) || isTrigger(conn.target)) return;
      if (!isValidConnection(conn)) return;
      dispatch({
        type: "connect",
        from: conn.source,
        fromPort: conn.sourceHandle ?? "out",
        to: conn.target,
      });
    },
    [dispatch, isValidConnection],
  );

  // クリックで対象ノードへ気持ちよくズームイン/アウトする（要求仕様）。
  const onNodeClick = React.useCallback(
    (_: React.MouseEvent, node: RfNode) => {
      void fitView({
        nodes: [{ id: node.id }],
        duration: 450,
        padding: 2.2,
        maxZoom: 1.15,
        minZoom: 0.5,
      });
    },
    [fitView],
  );

  // パレットから枠の中に落としたら、その繰り返しの中に置く（置けない種類は理由を言って置かない）。
  const onDrop = React.useCallback(
    (event: React.DragEvent) => {
      const nodeType = event.dataTransfer.getData(PALETTE_DND_TYPE);
      if (!nodeType) return;
      event.preventDefault();
      const position = screenToFlowPosition({ x: event.clientX, y: event.clientY });
      const { ir: cur, frames: rects } = latest.current;
      const parent = frameAt(cur, rects, position);
      const refusal = placeRefusal(cur, nodeType, parent);
      if (refusal) {
        toast({ variant: "destructive", title: "ここには置けません", description: refusal });
        return;
      }
      let at = { x: position.x - NODE_W / 2, y: position.y - 30 };
      const map = parent ? cur.nodes.find((n) => n.id === parent) : undefined;
      const frame = parent ? rects.get(parent) : undefined;
      if (map && frame) {
        // 見出しに重ねない（中のブロックは見出しの下から）。
        const origin = insideOrigin(map, frame);
        at = { x: Math.max(at.x, origin.x), y: Math.max(at.y, origin.y) };
      }
      dispatch({ type: "add_node", nodeType, position: at, parent });
    },
    [dispatch, screenToFlowPosition],
  );

  return (
    <ReactFlow
      nodes={nodes}
      edges={rfEdges}
      nodeTypes={nodeTypes}
      edgeTypes={edgeTypes}
      onNodesChange={onNodesChange}
      onEdgesChange={onEdgesChange}
      onConnect={onConnect}
      isValidConnection={isValidConnection}
      onDelete={onDelete}
      onNodeClick={onNodeClick}
      onNodeDragStart={onNodeDragStart}
      onNodeDrag={onNodeDrag}
      onNodeDragStop={onNodeDragStop}
      // ドラッグでは選ばない（選ぶと設定のパネルが開いてキャンバスが狭まり、運んでいるブロックが端へ流れる）。
      selectNodesOnDrag={false}
      onPaneClick={() => dispatch({ type: "select", selection: { kind: "none" } })}
      onDrop={onDrop}
      onDragOver={(e) => {
        if (e.dataTransfer.types.includes(PALETTE_DND_TYPE)) {
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
        }
      }}
      fitView
      fitViewOptions={{ padding: 0.25, maxZoom: 1 }}
      minZoom={0.2}
      maxZoom={1.75}
      deleteKeyCode={["Delete", "Backspace"]}
      proOptions={{ hideAttribution: true }}
      className="bg-background"
    >
      <Background
        variant={BackgroundVariant.Dots}
        gap={20}
        size={1.6}
        color="color-mix(in oklch, var(--muted-foreground) 35%, transparent)"
      />
      <Controls
        position="bottom-right"
        showInteractive={false}
        className="!rounded-lg !border !bg-background !shadow-sm [&_button]:!border-border [&_button]:!bg-background [&_button]:!text-foreground [&_button:hover]:!bg-accent pointer-coarse:[&_button]:!size-10"
      />
      <MiniMap
        position="bottom-left"
        pannable
        zoomable
        style={{ width: 160, height: 104 }}
        // スマホでは画面の 1/4 を占めて邪魔になるため出さない。
        className="!m-3 !rounded-lg !border !bg-background/90 !shadow-sm max-md:!hidden"
        maskColor="color-mix(in oklch, var(--muted) 60%, transparent)"
        nodeColor="color-mix(in oklch, var(--primary) 70%, transparent)"
        nodeStrokeWidth={3}
      />
    </ReactFlow>
  );
}
