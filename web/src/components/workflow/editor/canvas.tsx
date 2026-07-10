"use client";

/// キャンバス（React Flow）: ドットグリッド・自由 dnd・右下コントロール・クリックでズームフィット。
///
/// **IR が唯一の情報源**: nodes/edges は IR＋layout＋エラー写像からの導出値。位置変更は layout、
/// 構造変更は reducer action として親（workflow-editor.tsx）へ返す。

import * as React from "react";
import {
  Background,
  BackgroundVariant,
  Controls,
  MiniMap,
  ReactFlow,
  useReactFlow,
  type Connection,
  type Edge as RfEdge,
  type EdgeTypes,
  type Node as RfNode,
  type NodeChange,
  type NodeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";

import type { Edge as IrEdge, ValidationError, WorkflowIr } from "@/generated/workflow-ir";
import type { EditorLayout } from "@/lib/workflow-api";
import type { EditorAction, Selection } from "./ir-state";
import { mapErrors } from "./validation";
import { resolvePositions, nextPosition, NODE_W } from "./auto-layout";
import { NodeCard, type NodeCardData } from "./node-card";
import { TriggerNode, type TriggerNodeData } from "./trigger-node";
import { PlusEdge, type PlusEdgeData } from "./plus-edge";
import { PALETTE_DND_TYPE } from "./palette";

const nodeTypes: NodeTypes = {
  irNode: NodeCard as never,
  trigger: TriggerNode as never,
};
const edgeTypes: EdgeTypes = {
  plus: PlusEdge as never,
};

type Props = {
  ir: WorkflowIr;
  layout: EditorLayout;
  selection: Selection;
  serverErrors: ValidationError[];
  dispatch: React.Dispatch<EditorAction>;
};

function edgeKey(e: IrEdge): string {
  return `${e.from}|${e.from_port ?? "out"}|${e.to}`;
}

export function Canvas({ ir, layout, selection, serverErrors, dispatch }: Props) {
  const { fitView, screenToFlowPosition } = useReactFlow();
  const errors = React.useMemo(() => mapErrors(serverErrors), [serverErrors]);
  const positions = React.useMemo(() => resolvePositions(ir, layout), [ir, layout]);

  const onAddFrom = React.useCallback(
    (fromId: string, port: string, nodeType: string) => {
      const from = positions.nodes[fromId] ?? { x: 0, y: 0 };
      dispatch({
        type: "add_node",
        nodeType,
        position: nextPosition(from, positions.nodes),
        from: { id: fromId, port },
      });
    },
    [dispatch, positions],
  );

  // ── IR → React Flow nodes（導出値・useMemo）───────────────────────────
  const rfNodes = React.useMemo<RfNode[]>(() => {
    const entryIds = new Set(ir.nodes.map((n) => n.id));
    for (const e of ir.edges) entryIds.delete(e.to);
    const nodes: RfNode[] = ir.nodes.map((n) => ({
      id: n.id,
      type: "irNode",
      position: positions.nodes[n.id],
      selected: selection.kind === "node" && selection.id === n.id,
      data: {
        irNode: n,
        errors: errors.byNode.get(n.id) ?? [],
        onAddFrom: (port: string, nodeType: string) => onAddFrom(n.id, port, nodeType),
      } satisfies NodeCardData,
    }));
    ir.triggers.forEach((t, i) => {
      nodes.push({
        id: `trigger:${i}`,
        type: "trigger",
        position: positions.triggers[`trigger:${i}`],
        selected: selection.kind === "trigger" && selection.index === i,
        data: { trigger: t, index: i } satisfies TriggerNodeData,
      });
    });
    return nodes;
  }, [ir, positions, selection, errors, onAddFrom]);

  // ── IR → React Flow edges（トリガ→エントリの視覚接続を含む）────────────
  const rfEdges = React.useMemo<RfEdge[]>(() => {
    const edges: RfEdge[] = ir.edges.map((e) => {
      const key = edgeKey(e);
      const errKey = `${e.from} -> ${e.to}`;
      return {
        id: `e:${key}`,
        source: e.from,
        sourceHandle: e.from_port ?? "out",
        target: e.to,
        type: "plus",
        data: {
          irEdge: e,
          errorMessages: (errors.byEdge.get(errKey) ?? []).map((x) => x.message),
          onInsert: (nodeType: string, position: { x: number; y: number }) =>
            dispatch({
              type: "insert_on_edge",
              nodeType,
              edge: e,
              position: { x: position.x - NODE_W / 2, y: position.y },
            }),
        } satisfies PlusEdgeData,
      };
    });
    // トリガ → エントリノード（入エッジ 0 本）への破線（視覚のみ・選択/削除不可）。
    const hasIncoming = new Set(ir.edges.map((e) => e.to));
    const entries = ir.nodes.filter((n) => !hasIncoming.has(n.id));
    ir.triggers.forEach((_, i) => {
      for (const entry of entries) {
        edges.push({
          id: `t:${i}:${entry.id}`,
          source: `trigger:${i}`,
          target: entry.id,
          type: "default",
          selectable: false,
          deletable: false,
          animated: true,
          style: { strokeDasharray: "4 4", opacity: 0.5 },
        });
      }
    });
    return edges;
  }, [ir, errors, dispatch]);

  // ── React Flow からの変更を reducer に写像 ─────────────────────────────
  const onNodesChange = React.useCallback(
    (changes: NodeChange[]) => {
      const moved: Record<string, { x: number; y: number }> = {};
      for (const c of changes) {
        if (c.type === "position" && c.position && !c.dragging) {
          if (c.id.startsWith("trigger:")) {
            dispatch({ type: "move_trigger", key: c.id, position: c.position });
          } else {
            moved[c.id] = c.position;
          }
        }
        if (c.type === "select" && c.selected) {
          if (c.id.startsWith("trigger:")) {
            dispatch({
              type: "select",
              selection: { kind: "trigger", index: Number(c.id.slice("trigger:".length)) },
            });
          } else {
            dispatch({ type: "select", selection: { kind: "node", id: c.id } });
          }
        }
        if (c.type === "remove" && !c.id.startsWith("trigger:")) {
          dispatch({ type: "delete_nodes", ids: [c.id] });
        }
      }
      if (Object.keys(moved).length > 0) dispatch({ type: "move", positions: moved });
    },
    [dispatch],
  );

  const onConnect = React.useCallback(
    (conn: Connection) => {
      if (!conn.source || !conn.target) return;
      if (conn.source.startsWith("trigger:") || conn.target.startsWith("trigger:")) return;
      dispatch({
        type: "connect",
        from: conn.source,
        fromPort: conn.sourceHandle ?? "out",
        to: conn.target,
      });
    },
    [dispatch],
  );

  const onEdgesDelete = React.useCallback(
    (deleted: RfEdge[]) => {
      const irEdges = deleted
        .map((e) => (e.data as PlusEdgeData | undefined)?.irEdge)
        .filter((e): e is IrEdge => Boolean(e));
      if (irEdges.length > 0) dispatch({ type: "delete_edges", edges: irEdges });
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
      nodes={rfNodes}
      edges={rfEdges}
      nodeTypes={nodeTypes}
      edgeTypes={edgeTypes}
      onNodesChange={onNodesChange}
      onConnect={onConnect}
      onEdgesDelete={onEdgesDelete}
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
        className="!rounded-lg !border !bg-background !shadow-sm [&_button]:!border-border [&_button]:!bg-background [&_button]:!text-foreground [&_button:hover]:!bg-accent"
      />
      <MiniMap
        position="bottom-left"
        pannable
        zoomable
        style={{ width: 160, height: 104 }}
        className="!m-3 !rounded-lg !border !bg-background/90 !shadow-sm"
        maskColor="color-mix(in oklch, var(--muted) 60%, transparent)"
        nodeColor="color-mix(in oklch, var(--primary) 70%, transparent)"
        nodeStrokeWidth={3}
      />
    </ReactFlow>
  );
}
