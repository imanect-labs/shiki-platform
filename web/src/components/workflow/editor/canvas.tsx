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

import type { Edge as IrEdge, ValidationError, WorkflowIr } from "@/generated/workflow-ir";
import type { EditorLayout } from "@/lib/workflow-api";
import { edgeKey, type EditorAction, type Selection } from "./ir-state";
import { mapErrors } from "./validation";
import { resolvePositions, nextPosition, triggerKeys, NODE_W } from "./auto-layout";
import { NodeCard, type NodeCardData } from "./node-card";
import { TriggerNode, type TriggerNodeData } from "./trigger-node";
import { PlusEdge, type PlusEdgeData } from "./plus-edge";
import { PALETTE_DND_TYPE } from "./palette";
import { edgePortsOf, type ServerPorts } from "./ports";
import { nodePortLabel } from "../port-labels";

const nodeTypes: NodeTypes = {
  irNode: NodeCard as never,
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

  // 尻尾の＋の追加先は押した時点の配置から決める（ノードの data に毎回新しい関数を入れると、
  // 配置が変わるたびに全ノードが作り直されるため、参照の変わらない関数にして座標は ref で読む）。
  const positionsRef = React.useRef(positions);
  React.useLayoutEffect(() => {
    positionsRef.current = positions;
  }, [positions]);
  const onAddFrom = React.useCallback(
    (fromId: string, port: string, nodeType: string) => {
      const current = positionsRef.current.nodes;
      const from = current[fromId] ?? { x: 0, y: 0 };
      dispatch({
        type: "add_node",
        nodeType,
        position: nextPosition(from, current),
        from: { id: fromId, port },
      });
    },
    [dispatch],
  );

  // ── IR → React Flow nodes（導出値）────────────────────────────────────
  const derivedNodes = React.useMemo<RfNode[]>(() => {
    const nodes: RfNode[] = ir.nodes.map((n) => ({
      id: n.id,
      type: "irNode",
      position: positions.nodes[n.id],
      selected: selection.kind === "node" && selection.id === n.id,
      data: {
        irNode: n,
        errors: errors.byNode.get(n.id) ?? NO_ERRORS,
        warnings: warnings.byNode.get(n.id) ?? NO_ERRORS,
        knownPorts: serverPorts[n.id],
        portsFresh: serverPortsParams[n.id] === n.params,
        // 出口の一覧は文字列にして渡す（毎回新しい配列だと、変わらないノードまで作り直される）。
        edgePortsKey: JSON.stringify(edgePortsOf(n.id, ir.edges)),
        onAddFrom,
      } satisfies NodeCardData,
    }));
    ir.triggers.forEach((t, i) => {
      nodes.push({
        id: tKeys[i],
        type: "trigger",
        position: positions.triggers[tKeys[i]],
        selected: selection.kind === "trigger" && selection.index === i,
        // きっかけは IR の triggers[] で、キャンバスからは消さない（設定パネルの「このきっかけを削除」）。
        deletable: false,
        data: { trigger: t, index: i } satisfies TriggerNodeData,
      });
    });
    return nodes;
  }, [
    ir.nodes,
    ir.edges,
    ir.triggers,
    tKeys,
    positions,
    selection,
    errors,
    warnings,
    serverPorts,
    serverPortsParams,
    onAddFrom,
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
          errorMessages: (errors.byEdge.get(errKey) ?? []).map((x) => x.message),
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
    // トリガ → エントリノード（入エッジ 0 本）への破線（視覚のみ・選択/削除不可）。
    const hasIncoming = new Set(ir.edges.map((e) => e.to));
    const entries = ir.nodes.filter((n) => !hasIncoming.has(n.id));
    for (const key of tKeys) {
      for (const entry of entries) {
        edges.push({
          id: `t:${key}:${entry.id}`,
          source: key,
          target: entry.id,
          type: "default",
          selectable: false,
          deletable: false,
          animated: true,
          style: { strokeDasharray: "4 4", opacity: 0.5 },
        });
      }
    }
    return edges;
  }, [ir.edges, ir.nodes, tKeys, selection, errors, dispatch]);

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

      const moved: Record<string, { x: number; y: number }> = {};
      for (const c of changes) {
        // 確定した移動（離したとき・矢印キー）だけを layout に送る。ドラッグ中は手元だけで動かす。
        if (c.type === "position" && c.position && !c.dragging) {
          if (isTrigger(c.id)) {
            dispatch({ type: "move_trigger", key: c.id, position: c.position });
          } else {
            moved[c.id] = c.position;
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

  const onConnect = React.useCallback(
    (conn: Connection) => {
      if (!conn.source || !conn.target) return;
      if (isTrigger(conn.source) || isTrigger(conn.target)) return;
      dispatch({
        type: "connect",
        from: conn.source,
        fromPort: conn.sourceHandle ?? "out",
        to: conn.target,
      });
    },
    [dispatch],
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

  const onDrop = React.useCallback(
    (event: React.DragEvent) => {
      const nodeType = event.dataTransfer.getData(PALETTE_DND_TYPE);
      if (!nodeType) return;
      event.preventDefault();
      const position = screenToFlowPosition({ x: event.clientX, y: event.clientY });
      dispatch({
        type: "add_node",
        nodeType,
        position: { x: position.x - NODE_W / 2, y: position.y - 30 },
      });
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
      onDelete={onDelete}
      onNodeClick={onNodeClick}
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
