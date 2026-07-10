/// エディタの単一情報源（IR＋レイアウト＋選択＋検証エラー）の reducer。
///
/// **IR が正**: React Flow の nodes/edges は IR＋layout からの導出値（canvas.tsx の useMemo）。
/// 全編集はここの action を通り、undo（≤50）と dirty 追跡が一元化される。
/// 座標変更は IR を汚さず layout のみ更新する（undo 対象外・自動保存）。

import * as React from "react";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { Edge, Node, Trigger, ValidationError, WorkflowIr } from "@/generated/workflow-ir";
import type { EditorLayout } from "@/lib/workflow-api";

export type Selection =
  | { kind: "none" }
  | { kind: "node"; id: string }
  | { kind: "trigger"; index: number };

export type EditorState = {
  ir: WorkflowIr;
  /// サーバ保存済みバージョン（楽観ロックに使う）。
  savedVersion: number;
  layout: EditorLayout;
  selection: Selection;
  /// 保存されていない IR 変更があるか。
  dirty: boolean;
  /// レイアウトの未保存変更（自動保存の対象）。
  layoutDirty: boolean;
  undoStack: WorkflowIr[];
  redoStack: WorkflowIr[];
  serverErrors: ValidationError[];
};

export type EditorAction =
  | { type: "add_node"; nodeType: string; position: { x: number; y: number }; from?: { id: string; port: string } }
  | { type: "insert_on_edge"; nodeType: string; edge: Edge; position: { x: number; y: number } }
  | { type: "connect"; from: string; fromPort: string; to: string }
  | { type: "delete_nodes"; ids: string[] }
  | { type: "delete_edges"; edges: Edge[] }
  | { type: "move"; positions: Record<string, { x: number; y: number }> }
  | { type: "move_trigger"; key: string; position: { x: number; y: number } }
  | { type: "update_node"; id: string; patch: Partial<Node> }
  | { type: "rename_node"; id: string; nextId: string }
  | { type: "update_meta"; patch: Partial<Pick<WorkflowIr, "display_name" | "description" | "policies" | "declared_scopes">> }
  | { type: "set_triggers"; triggers: Trigger[] }
  | { type: "select"; selection: Selection }
  | { type: "set_errors"; errors: ValidationError[] }
  | { type: "saved"; version: number; ir?: WorkflowIr }
  | { type: "layout_saved" }
  | { type: "undo" }
  | { type: "redo" };

const MAX_UNDO = 50;

/// ノード id の採番（種別接頭辞＋連番・`^[a-z][a-z0-9_]{0,63}$` 準拠）。
export function newNodeId(ir: WorkflowIr, nodeType: string): string {
  const base = nodeType.split(".").pop()?.replace(/[^a-z0-9]/g, "_") ?? "node";
  let n = 1;
  const ids = new Set(ir.nodes.map((node) => node.id));
  while (ids.has(`${base}_${n}`)) n += 1;
  return `${base}_${n}`;
}

/// カタログの既定 params（必須フィールドの雛形・右パネルで埋める前提の最小形）。
export function defaultParams(nodeType: string): unknown {
  switch (nodeType) {
    case "control.branch":
      return { condition: { cmp: { left: { $from: "input", path: "/" }, op: "exists" } } };
    case "control.switch":
      return { value: { $from: "input", path: "/" }, cases: [] };
    case "control.map":
      return { items: { $from: "input", path: "/" } };
    case "control.wait":
      return { kind: "duration", duration_sec: 60 };
    case "storage.read":
      return { file: { $from: "input", path: "/file_id" } };
    case "storage.write":
      return { name: "output.txt", content: { $from: "input", path: "/" } };
    case "rag.search":
      return { query: { $from: "input", path: "/" } };
    case "llm.invoke":
      return { prompt: { $from: "input", path: "/" } };
    case "agent.invoke":
      return { instruction: { $from: "input", path: "/" } };
    case "http.request":
      return { url: "https://" };
    case "script.run":
      return { source: { inline: "function main(input) {\n  return input;\n}" } };
    case "workflow.start":
      return { name: "" };
    default:
      return {};
  }
}

function makeNode(ir: WorkflowIr, nodeType: string): Node {
  const entry = NODE_CATALOG.find((c) => c.type === nodeType);
  return {
    id: newNodeId(ir, nodeType),
    type: nodeType,
    label: entry?.label_ja ?? null,
    parent: null,
    params: defaultParams(nodeType),
    retry: { max_attempts: 1, backoff: { base_sec: 2, max_sec: 300, jitter: true } },
    timeout_sec: null,
    on_error: "fail_run",
  } as unknown as Node;
}

function pushUndo(state: EditorState): Pick<EditorState, "undoStack" | "redoStack"> {
  return {
    undoStack: [...state.undoStack.slice(-(MAX_UNDO - 1)), state.ir],
    redoStack: [],
  };
}

function withIr(state: EditorState, ir: WorkflowIr): EditorState {
  return { ...state, ...pushUndo(state), ir, dirty: true };
}

export function editorReducer(state: EditorState, action: EditorAction): EditorState {
  switch (action.type) {
    case "add_node": {
      const node = makeNode(state.ir, action.nodeType);
      const edges = action.from
        ? [
            ...state.ir.edges,
            { from: action.from.id, from_port: action.from.port, to: node.id } as Edge,
          ]
        : state.ir.edges;
      const next = withIr(state, {
        ...state.ir,
        nodes: [...state.ir.nodes, node],
        edges,
      });
      return {
        ...next,
        layout: {
          ...state.layout,
          positions: { ...(state.layout.positions ?? {}), [node.id]: action.position },
        },
        layoutDirty: true,
        selection: { kind: "node", id: node.id },
      };
    }
    case "insert_on_edge": {
      const node = makeNode(state.ir, action.nodeType);
      const edges = state.ir.edges.flatMap((e) =>
        e.from === action.edge.from &&
        e.to === action.edge.to &&
        (e.from_port ?? "out") === (action.edge.from_port ?? "out")
          ? [
              { from: e.from, from_port: e.from_port, to: node.id } as Edge,
              { from: node.id, from_port: "out", to: e.to } as Edge,
            ]
          : [e],
      );
      const next = withIr(state, {
        ...state.ir,
        nodes: [...state.ir.nodes, node],
        edges,
      });
      return {
        ...next,
        layout: {
          ...state.layout,
          positions: { ...(state.layout.positions ?? {}), [node.id]: action.position },
        },
        layoutDirty: true,
        selection: { kind: "node", id: node.id },
      };
    }
    case "connect": {
      const exists = state.ir.edges.some(
        (e) =>
          e.from === action.from &&
          (e.from_port ?? "out") === action.fromPort &&
          e.to === action.to,
      );
      if (exists || action.from === action.to) return state;
      return withIr(state, {
        ...state.ir,
        edges: [
          ...state.ir.edges,
          { from: action.from, from_port: action.fromPort, to: action.to } as Edge,
        ],
      });
    }
    case "delete_nodes": {
      const ids = new Set(action.ids);
      if (ids.size === 0) return state;
      const next = withIr(state, {
        ...state.ir,
        nodes: state.ir.nodes.filter((n) => !ids.has(n.id)),
        edges: state.ir.edges.filter((e) => !ids.has(e.from) && !ids.has(e.to)),
      });
      const positions = { ...(state.layout.positions ?? {}) };
      for (const id of ids) delete positions[id];
      return {
        ...next,
        layout: { ...state.layout, positions },
        layoutDirty: true,
        selection: { kind: "none" },
      };
    }
    case "delete_edges": {
      if (action.edges.length === 0) return state;
      const keys = new Set(
        action.edges.map((e) => `${e.from}|${e.from_port ?? "out"}|${e.to}`),
      );
      return withIr(state, {
        ...state.ir,
        edges: state.ir.edges.filter(
          (e) => !keys.has(`${e.from}|${e.from_port ?? "out"}|${e.to}`),
        ),
      });
    }
    case "move": {
      return {
        ...state,
        layout: {
          ...state.layout,
          positions: { ...(state.layout.positions ?? {}), ...action.positions },
        },
        layoutDirty: true,
      };
    }
    case "move_trigger": {
      return {
        ...state,
        layout: {
          ...state.layout,
          triggers: { ...(state.layout.triggers ?? {}), [action.key]: action.position },
        },
        layoutDirty: true,
      };
    }
    case "update_node": {
      return withIr(state, {
        ...state.ir,
        nodes: state.ir.nodes.map((n) =>
          n.id === action.id ? ({ ...n, ...action.patch } as Node) : n,
        ),
      });
    }
    case "rename_node": {
      const nextId = action.nextId;
      if (
        nextId === action.id ||
        !/^[a-z][a-z0-9_]{0,63}$/.test(nextId) ||
        state.ir.nodes.some((n) => n.id === nextId)
      ) {
        return state;
      }
      const positions = { ...(state.layout.positions ?? {}) };
      if (positions[action.id]) {
        positions[nextId] = positions[action.id];
        delete positions[action.id];
      }
      const renamed = withIr(state, {
        ...state.ir,
        nodes: state.ir.nodes.map((n) =>
          n.id === action.id ? ({ ...n, id: nextId } as Node) : n,
        ),
        edges: state.ir.edges.map((e) => ({
          ...e,
          from: e.from === action.id ? nextId : e.from,
          to: e.to === action.id ? nextId : e.to,
        })),
      });
      return {
        ...renamed,
        layout: { ...state.layout, positions },
        layoutDirty: true,
        selection: { kind: "node", id: nextId },
      };
    }
    case "update_meta": {
      return withIr(state, { ...state.ir, ...action.patch } as WorkflowIr);
    }
    case "set_triggers": {
      return withIr(state, { ...state.ir, triggers: action.triggers });
    }
    case "select":
      return { ...state, selection: action.selection };
    case "set_errors":
      return { ...state, serverErrors: action.errors };
    case "saved":
      return {
        ...state,
        savedVersion: action.version,
        dirty: false,
        ...(action.ir ? { ir: action.ir } : {}),
      };
    case "layout_saved":
      return { ...state, layoutDirty: false };
    case "undo": {
      const prev = state.undoStack.at(-1);
      if (!prev) return state;
      return {
        ...state,
        ir: prev,
        undoStack: state.undoStack.slice(0, -1),
        redoStack: [...state.redoStack, state.ir],
        dirty: true,
      };
    }
    case "redo": {
      const next = state.redoStack.at(-1);
      if (!next) return state;
      return {
        ...state,
        ir: next,
        redoStack: state.redoStack.slice(0, -1),
        undoStack: [...state.undoStack, state.ir],
        dirty: true,
      };
    }
    default:
      return state;
  }
}

export function useEditorState(initial: {
  ir: WorkflowIr;
  version: number;
  layout: EditorLayout;
}) {
  return React.useReducer(editorReducer, {
    ir: initial.ir,
    savedVersion: initial.version,
    layout: initial.layout,
    selection: { kind: "none" },
    dirty: false,
    layoutDirty: false,
    undoStack: [],
    redoStack: [],
    serverErrors: [],
  } satisfies EditorState);
}
