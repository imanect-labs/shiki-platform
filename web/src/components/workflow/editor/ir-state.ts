/// エディタの単一情報源（IR＋レイアウト＋選択＋検証エラー）の reducer。
///
/// **IR が正**: React Flow の nodes/edges は IR＋layout からの導出値（canvas.tsx）。
/// 全編集はここの action を通り、undo（≤50）と dirty 追跡が一元化される。
/// 座標変更は IR を汚さず layout のみ更新する（undo 対象外・自動保存）。
/// layout は開いた時点で全座標を埋めて持ち（`seedLayout`）、消したノードの座標も手元では残す
/// （Undo で同じ位置に戻すため。保存のときに今の IR に無いものを落とす）。

import * as React from "react";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { Edge, Node, Trigger, ValidationError, WorkflowIr } from "@/generated/workflow-ir";
import type { EditorLayout } from "@/lib/workflow-api";
import { carryTriggerPositions, seedLayout } from "./auto-layout";
import { rewriteInputRefs } from "./config-panel/value-refs";
import { defaultOutPort, outputPorts, type ServerPorts } from "./ports";

export type Selection =
  | { kind: "none" }
  | { kind: "node"; id: string }
  | { kind: "trigger"; index: number }
  /// つながり（`edgeKey` の `from|from_port|to`）。選ぶとノードの選択は外れる（選択は常に 1 つ）。
  | { kind: "edge"; key: string };

/// つながりの識別子（同じ出口から同じ先へのエッジは 1 本だけ・`connect` が重複を作らない）。
export function edgeKey(e: Edge): string {
  return `${e.from}|${e.from_port ?? "out"}|${e.to}`;
}

function sameSelection(a: Selection, b: Selection): boolean {
  switch (a.kind) {
    case "none":
      return b.kind === "none";
    case "node":
      return b.kind === "node" && b.id === a.id;
    case "trigger":
      return b.kind === "trigger" && b.index === a.index;
    case "edge":
      return b.kind === "edge" && b.key === a.key;
  }
}

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
  /// `serverErrors` を返した検証の対象の IR（無ければ `null`）。誤りの path の添字（`/params/show_fields/1` など）を
  /// この IR で引けば、検証の後に並びが変わっても今の値に取り違えない（次の検証の応答が来るまでの間）。
  serverErrorsIr: WorkflowIr | null;
  /// 保存を止めない警告（`ir.condition_semantics_changed` など・保存/検証の応答の `warnings`）。
  serverWarnings: ValidationError[];
  /// サーバが導いたノードの出口（検証 API の `node_ports`・判定のノードのハンドルを描く・H15）。
  /// 直近の応答に載らなかったノード（設定が契約に合わない間）は、前に受け取った出口を残す（直している間に
  /// ハンドルが消えたり出たりしない）。今の IR に無いノードの分は落とす。
  serverPorts: ServerPorts;
  /// `serverPorts` を導いたときのノードの params（参照）。今の params と同じなら、その出口は今の設定のもの
  /// （無い出口のエッジを「無い出口」と言い切れる）。違えば次の応答までの古い出口。
  serverPortsParams: Readonly<Record<string, unknown>>;
  /// 直前の `update_node`・`update_meta`（同じノード・ワークフローの同じ項目への連続入力を undo 1 段に
  /// まとめる。`update_meta` は id が空）。他の編集・選択の変更・undo/redo で切れる。
  lastEdit: { id: string; field: string; at: number } | null;
};

export type EditorAction =
  | { type: "add_node"; nodeType: string; position: { x: number; y: number }; from?: { id: string; port: string } }
  | { type: "insert_on_edge"; nodeType: string; edge: Edge; position: { x: number; y: number } }
  /// 入口ノード（入エッジ 0 本）の手前に挿入する（スマホの縦一覧で「きっかけ」直後の＋）。
  | { type: "insert_before"; nodeType: string; before: string; position: { x: number; y: number } }
  | { type: "connect"; from: string; fromPort: string; to: string }
  | { type: "delete_nodes"; ids: string[] }
  /// ノードとつながりをまとめて消す（キャンバスの Delete キー・つながりの削除ボタン）。undo は 1 段。
  | { type: "delete_elements"; nodes: string[]; edges: Edge[] }
  | { type: "move"; positions: Record<string, { x: number; y: number }> }
  /// `key` は `triggerKeys` のきっかけの座標キー。
  | { type: "move_trigger"; key: string; position: { x: number; y: number } }
  /// `at` は dispatch した時刻（`useEditorState` が付ける・連続入力をまとめる判定に使う）。
  | { type: "update_node"; id: string; patch: Partial<Node>; at?: number }
  /// switch の case 出口名変更（params 更新と当該ポートのエッジ from_port 追随を 1 手で行う）。
  /// `noMerge` は、新しい名前の出口からもうエッジが出ていれば付け替えない（判定の選択肢のキーの入力中に、
  /// 別の出口の名前を通っても線を混ぜない）。
  | { type: "rename_out_port"; id: string; fromPort: string; toPort: string; params: unknown; noMerge?: boolean }
  | { type: "rename_node"; id: string; nextId: string }
  /// `at` は `update_node` と同じ（入力項目の連続入力を undo 1 段にまとめる）。
  /// `inputRenames` は入力項目のキーの変更（旧キー → 新キー）。ブロックの参照（`$from: "input"` の path）を
  /// 同じ 1 手で書き換える（残すと保存は通り、実行で「解決できません」になる）。
  | {
      type: "update_meta";
      patch: Partial<Pick<WorkflowIr, "display_name" | "description" | "policies" | "declared_scopes" | "input_schema">>;
      inputRenames?: Record<string, string>;
      at?: number;
    }
  | { type: "set_triggers"; triggers: Trigger[] }
  | { type: "select"; selection: Selection }
  /// 今の選択が `selection` のときだけ外す（React Flow の選択解除の通知用。先に別の要素を選んだ通知が
  /// 来ていれば何もしない）。
  | { type: "deselect"; selection: Selection }
  /// `ir` は検証した IR（誤りの添字をその時点の値で引く）。誤りを消すだけなら省く。
  | { type: "set_errors"; errors: ValidationError[]; ir?: WorkflowIr }
  | { type: "set_warnings"; warnings: ValidationError[] }
  /// 検証 API の `node_ports` を受け取る（[`EditorState::serverPorts`] へ合わせる）。`ir` は検証した IR。
  | { type: "set_ports"; ports: Record<string, string[]>; ir: WorkflowIr }
  | { type: "saved"; version: number; savedIr: WorkflowIr; ir?: WorkflowIr; declaredScopes?: string[] }
  | { type: "layout_saved"; layout: EditorLayout }
  | { type: "undo" }
  | { type: "redo" };

const MAX_UNDO = 50;
/// 同じ項目への入力がこれ以上あいたら、まとめずに undo を 1 段積む。
const COALESCE_MS = 1500;

/// ノード id の採番（種別接頭辞＋連番・`^[a-z][a-z0-9_]{0,63}$` 準拠）。
export function newNodeId(ir: WorkflowIr, nodeType: string): string {
  const base = nodeType.split(".").pop()?.replace(/[^a-z0-9]/g, "_") ?? "node";
  let n = 1;
  const ids = new Set(ir.nodes.map((node) => node.id));
  while (ids.has(`${base}_${n}`)) n += 1;
  return `${base}_${n}`;
}

// ノード種の既定の出力ポートは ports.ts（尻尾の＋・末尾への追加と同じ規則）。
export { defaultOutPort };

/// カタログの既定 params（必須フィールドの雛形・右パネルで埋める前提の最小形）。
/// **入力全体を指す `$from` は `path` を持たない**（JSON Pointer の `"/"` は
/// 「空文字キーのプロパティ」であり全体ではない・実行時に解決不能になる）。
/// 利用者が書くしかない必須の文字の欄は `""` で入れる（右パネルが空のままなら「入力してください」と出す）。
/// 文字を求める欄に入力全体（オブジェクト）を入れると、置いたまま実行したとき必ず失敗する（BUG-15）。
/// 一覧から選ばせる欄（storage.read・csv.* のファイル・data.* のテーブル）は入れない（保存時の検証の
/// 「項目がありません」が欄の下に出る）。前の結果を指す既定を入れると、欄が「前の結果から」になり、
/// スケジュールで有効化すると「入力が無いので失敗します」と出る（BUG-32）。
export function defaultParams(nodeType: string): unknown {
  switch (nodeType) {
    case "control.branch":
      return { condition: { cmp: { left: { $from: "input" }, op: "exists" } } };
    case "control.switch":
      return { value: { $from: "input" }, cases: [] };
    case "control.map":
      return { items: { $from: "input" } };
    case "control.wait":
      return { kind: "duration", duration_sec: 60 };
    case "storage.read":
      // ファイルはドライブから選ばせる（前の結果から読むなら欄で切り替える）。
      return {};
    case "storage.write":
      // 同じ名前があれば番号を付けて保存する（省略は失敗にする＝既存の定義の挙動・2 回目から失敗しない・D-97）。
      return { name: "output.txt", content: { $from: "input" }, on_name_taken: "add_number" };
    case "rag.search":
      return { query: "" };
    case "llm.invoke":
      return { prompt: "" };
    case "agent.invoke":
      // 道具は社内を読むものだけを選んだ状態で置く（省略は道具なし・社外へ出る道具は明示して選ぶ・BUG-6）。
      return { instruction: "", allowed_tools: ["rag.search", "doc.read", "storage.list"] };
    case "http.request":
      // 相手が誤り（400 以上）を返したら失敗にする（省略は成功として次へ渡す＝既存の定義の挙動・D-95）。
      return { url: "https://", fail_on_error_status: true };
    case "script.run":
      return { source: { inline: "function main(input) {\n  return input;\n}" } };
    case "skill.invoke":
      return { skill: "" };
    case "workflow.start":
      return { name: "" };
    case "workflow.reply":
      // 返す内容は利用者が書く（空のままなら欄の下で入力を促す）。
      return { format: "text", value: "" };
    case "csv.query":
      return { sql: "SELECT * FROM data LIMIT 100" };
    case "csv.patch":
      return { ops: [] };
    case "csv.write":
      // 入力全体（オブジェクト）を入れると JSON の文字がそのまま .csv に保存される（失敗もしない）。
      return { name: "output.csv", content: "", on_name_taken: "add_number" };
    case "ai.decide":
      // モデル・判定の材料・問いは利用者が書く（空のままなら欄の下で入力を促す・V1 が保存を止める）。
      // 質問は選択肢 2 つの 1 件から始める。振り分けは置かない（出口は out だけ）。
      return {
        model: "",
        state: "",
        questions: [
          { key: "q1", type: "choice", instructions: "", options: [{ key: "option_1" }, { key: "option_2" }] },
        ],
      };
    case "human.approval":
      // テーブル・判断の行き先・担当者は選ばせる（テーブルの列と状態から選ぶ）。行は既定でトリガの行。
      return {
        title: "",
        record: { table: "", id: { $from: "trigger", path: "/record_id" } },
        assignees: [],
        approve: { to: "" },
      };
    default:
      return {};
  }
}

/// 新規ノードを作る。`near` を渡すとそのノードと同じ領域（`parent`）に置く。
/// 繋ぐ相手と領域が違うとエッジが領域境界を跨ぎ、サーバが `ir.region_leak` で拒否するため
/// （control.map の本体内へ挿入する場合など）、エッジで繋ぐ挿入は必ず相手の parent を継ぐ。
function makeNode(ir: WorkflowIr, nodeType: string, near?: string): Node {
  const entry = NODE_CATALOG.find((c) => c.type === nodeType);
  const parent = near ? (ir.nodes.find((n) => n.id === near)?.parent ?? null) : null;
  return {
    id: newNodeId(ir, nodeType),
    type: nodeType,
    label: entry?.label_ja ?? null,
    parent,
    params: defaultParams(nodeType),
    retry: { max_attempts: 1, backoff: { base_sec: 2, max_sec: 300, jitter: true } },
    timeout_sec: null,
    on_error: "fail_run",
  } as unknown as Node;
}

/// params JSON 内の `$from` 参照（`"<id>"` / `"nodes.<id>.output"`）を新 id へ書き換える。
function rewriteFromRefs(value: unknown, oldId: string, nextId: string): unknown {
  if (Array.isArray(value)) {
    return value.map((v) => rewriteFromRefs(v, oldId, nextId));
  }
  if (value && typeof value === "object") {
    const obj = value as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(obj)) {
      if (k === "$from" && typeof v === "string") {
        if (v === oldId) {
          out[k] = nextId;
          continue;
        }
        if (v === `nodes.${oldId}.output`) {
          out[k] = `nodes.${nextId}.output`;
          continue;
        }
      }
      out[k] = rewriteFromRefs(v, oldId, nextId);
    }
    return out;
  }
  return value;
}

function pushUndo(state: EditorState): Pick<EditorState, "undoStack" | "redoStack"> {
  return {
    undoStack: [...state.undoStack.slice(-(MAX_UNDO - 1)), state.ir],
    redoStack: [],
  };
}

function withIr(state: EditorState, ir: WorkflowIr): EditorState {
  return { ...state, ...pushUndo(state), ir, dirty: true, lastEdit: null };
}

/// ノードとつながりを消す（つながっているエッジも一緒に消える）。undo は 1 段。
/// 座標（layout）は消さない: Undo で戻したノードを同じ位置に出すため（保存時に今の IR に無いものを落とす）。
function deleteElements(state: EditorState, nodeIds: string[], edges: Edge[]): EditorState {
  const ids = new Set(nodeIds);
  const keys = new Set(edges.map(edgeKey));
  const nodes = state.ir.nodes.filter((n) => !ids.has(n.id));
  const nextEdges = state.ir.edges.filter(
    (e) => !ids.has(e.from) && !ids.has(e.to) && !keys.has(edgeKey(e)),
  );
  if (nodes.length === state.ir.nodes.length && nextEdges.length === state.ir.edges.length) {
    return state;
  }
  return {
    ...withIr(state, { ...state.ir, nodes, edges: nextEdges }),
    selection: { kind: "none" },
  };
}

/// 設定の変更で消えた出口（switch の case を消す・on_error を fail_run に戻す・時間切れを「失敗にする」に
/// 戻す・却下や差し戻しを外す）から出るエッジを外す（BUG-30）。残すと保存は `ir.unknown_port` で止まり、
/// キャンバスには描けないので選んで消せない。同じ 1 手で外すので Undo で出口とエッジが一緒に戻る。
/// 前から無い出口のエッジ（読み込んだ IR の誤り）は触らない（保存時の検証が示す）。
/// 判定のノードの出口はサーバの導出（`known`）なので、ここで外れるのはエラー時の出口だけ。振り分けを変えて
/// 無くなった出口のエッジは、検証の応答の後に「無い出口」として描き、選んで消せる（`drawnPorts`）。
function dropVanishedPortEdges(edges: Edge[], before: Node, after: Node, known?: readonly string[]): Edge[] {
  const now = new Set(outputPorts(after, known));
  const gone = new Set(outputPorts(before, known).filter((p) => !now.has(p)));
  if (gone.size === 0) return edges;
  const next = edges.filter((e) => !(e.from === before.id && gone.has(e.from_port ?? "out")));
  return next.length === edges.length ? edges : next;
}

/// 選んでいたつながりが消えたら選択を外す。
function keepSelection(selection: Selection, edges: Edge[]): Selection {
  if (selection.kind !== "edge") return selection;
  return edges.some((e) => edgeKey(e) === selection.key) ? selection : { kind: "none" };
}

/// `update_node` が変えた項目（params は変わった最上位のキーまで見る。`patchParams` は変えないキーの値を
/// 使い回すので参照比較で足りる）。
function editedField(prev: Node | undefined, patch: Partial<Node>): string {
  const keys = Object.keys(patch).sort();
  if (keys.length === 1 && keys[0] === "params") {
    const before = prev?.params;
    const after = patch.params;
    if (isRecord(before) && isRecord(after)) {
      const changed = [...new Set([...Object.keys(before), ...Object.keys(after)])]
        .filter((k) => before[k] !== after[k])
        .sort();
      return `params:${changed.join(",")}`;
    }
  }
  return keys.join(",");
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

export function editorReducer(state: EditorState, action: EditorAction): EditorState {
  switch (action.type) {
    case "add_node": {
      const node = makeNode(state.ir, action.nodeType, action.from?.id);
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
      const node = makeNode(state.ir, action.nodeType, action.edge.from);
      const edges = state.ir.edges.flatMap((e) =>
        e.from === action.edge.from &&
        e.to === action.edge.to &&
        (e.from_port ?? "out") === (action.edge.from_port ?? "out")
          ? [
              { from: e.from, from_port: e.from_port, to: node.id } as Edge,
              // 挿入ノードの継続はノード種の実ポートで張る（branch=true / switch=default 等。
              // "out" 固定だと制御ノード挿入時に下流が実行時に不発になる）。
              { from: node.id, from_port: defaultOutPort(action.nodeType), to: e.to } as Edge,
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
    case "insert_before": {
      const node = makeNode(state.ir, action.nodeType, action.before);
      const next = withIr(state, {
        ...state.ir,
        nodes: [...state.ir.nodes, node],
        edges: [
          ...state.ir.edges,
          { from: node.id, from_port: defaultOutPort(action.nodeType), to: action.before } as Edge,
        ],
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
    case "delete_nodes":
      return deleteElements(state, action.ids, []);
    case "delete_elements":
      return deleteElements(state, action.nodes, action.edges);
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
      const prev = state.ir.nodes.find((n) => n.id === action.id);
      const updated = prev ? ({ ...prev, ...action.patch } as Node) : undefined;
      const edges =
        prev && updated
          ? dropVanishedPortEdges(state.ir.edges, prev, updated, state.serverPorts[prev.id])
          : state.ir.edges;
      const ir = {
        ...state.ir,
        nodes: state.ir.nodes.map((n) => (n.id === action.id && updated ? updated : n)),
        edges,
      };
      const selection = keepSelection(state.selection, edges);
      const field = editedField(prev, action.patch);
      const last = state.lastEdit;
      // 同じノードの同じ項目への連続入力は、直前の undo の段に含める（1 文字ごとに積むと
      // 上限 50 段がすぐ埋まり、Undo も 1 文字ずつしか戻らない）。
      if (
        action.at !== undefined &&
        last &&
        last.id === action.id &&
        last.field === field &&
        action.at - last.at < COALESCE_MS &&
        state.undoStack.length > 0
      ) {
        return { ...state, ir, selection, dirty: true, redoStack: [], lastEdit: { ...last, at: action.at } };
      }
      return {
        ...withIr(state, ir),
        selection,
        lastEdit: action.at === undefined ? null : { id: action.id, field, at: action.at },
      };
    }
    case "rename_out_port": {
      const taken =
        action.noMerge &&
        state.ir.edges.some((e) => e.from === action.id && (e.from_port ?? "out") === action.toPort);
      return withIr(state, {
        ...state.ir,
        nodes: state.ir.nodes.map((n) =>
          n.id === action.id ? ({ ...n, params: action.params } as Node) : n,
        ),
        edges: taken
          ? state.ir.edges
          : state.ir.edges.map((e) =>
              e.from === action.id && (e.from_port ?? "out") === action.fromPort
                ? ({ ...e, from_port: action.toPort } as Edge)
                : e,
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
      // 元の id の座標も残す（Undo で元の id に戻したときに同じ位置へ出すため）。
      const positions = { ...(state.layout.positions ?? {}) };
      if (positions[action.id]) positions[nextId] = positions[action.id];
      const renamed = withIr(state, {
        ...state.ir,
        nodes: state.ir.nodes.map((n) => {
          const node =
            n.id === action.id ? ({ ...n, id: nextId } as Node) : n;
          return {
            ...node,
            // map 領域の親リンクと、params 内の $from 参照も追随させる
            //（残すと保存時検証で落ちるか、map 関係が壊れる）。
            parent: node.parent === action.id ? nextId : node.parent,
            params: rewriteFromRefs(node.params, action.id, nextId),
          } as Node;
        }),
        edges: state.ir.edges.map((e) => ({
          ...e,
          from: e.from === action.id ? nextId : e.from,
          to: e.to === action.id ? nextId : e.to,
        })),
      });
      // サーバが導いた出口も新しい id へ写す（次の検証の応答までハンドルを消さない）。
      const known = state.serverPorts[action.id];
      return {
        ...renamed,
        layout: { ...state.layout, positions },
        layoutDirty: true,
        selection: { kind: "node", id: nextId },
        ...(known
          ? {
              serverPorts: { ...state.serverPorts, [nextId]: known },
              serverPortsParams: { ...state.serverPortsParams, [nextId]: state.serverPortsParams[action.id] },
            }
          : {}),
      };
    }
    case "update_meta": {
      const renames = action.inputRenames ?? {};
      const ir = { ...state.ir, ...action.patch } as WorkflowIr;
      if (Object.keys(renames).length > 0) {
        ir.nodes = state.ir.nodes.map((n) => {
          const params = rewriteInputRefs(n.params, renames);
          return params === n.params ? n : ({ ...n, params } as Node);
        });
      }
      // ノードの id は空にならないので、空の id をワークフロー自体の項目の印にする。
      const field = `meta:${Object.keys(action.patch).sort().join(",")}`;
      const last = state.lastEdit;
      if (
        action.at !== undefined &&
        last &&
        last.id === "" &&
        last.field === field &&
        action.at - last.at < COALESCE_MS &&
        state.undoStack.length > 0
      ) {
        return { ...state, ir, dirty: true, redoStack: [], lastEdit: { ...last, at: action.at } };
      }
      return {
        ...withIr(state, ir),
        lastEdit: action.at === undefined ? null : { id: "", field, at: action.at },
      };
    }
    case "set_triggers": {
      const next = withIr(state, { ...state.ir, triggers: action.triggers });
      // きっかけの座標キーは内容から決まるので、編集で変わったキーへ座標を写す（BUG-25）。
      // 写すものが無くても保存し直す: 旧形式（添字のキー）の layout が残っていると、きっかけを消した後に
      // 開き直したとき、添字の読み替えで別のきっかけの座標が付いてしまう。
      const saved = state.layout.triggers ?? {};
      const triggers = carryTriggerPositions(state.ir.triggers, action.triggers, saved);
      return {
        ...next,
        layout: triggers === saved ? state.layout : { ...state.layout, triggers },
        layoutDirty: true,
      };
    }
    case "select":
      return { ...state, selection: action.selection, lastEdit: null };
    case "deselect":
      return sameSelection(state.selection, action.selection)
        ? { ...state, selection: { kind: "none" }, lastEdit: null }
        : state;
    case "set_errors":
      return { ...state, serverErrors: action.errors, serverErrorsIr: action.ir ?? null };
    case "set_warnings":
      return { ...state, serverWarnings: action.warnings };
    case "set_ports": {
      const next = mergePorts(state, action.ports, action.ir);
      return next.serverPorts === state.serverPorts && next.serverPortsParams === state.serverPortsParams
        ? state
        : { ...state, ...next };
    }
    case "saved": {
      // 保存中に続けた編集が無ければ、サーバが補完した宣言スコープ（保存した版の値）を手元の IR に入れる
      // （履歴には積まない・保存済みの内容と同じなので dirty にしない）。
      const completed =
        !action.ir && action.declaredScopes && state.ir === action.savedIr
          ? ({ ...state.ir, declared_scopes: action.declaredScopes } as WorkflowIr)
          : null;
      return {
        ...state,
        savedVersion: action.version,
        // 保存中に続けた編集を「保存済み」と偽らない: PUT へ送ったスナップショットと
        // 現在の IR が同一（reducer は毎編集で新オブジェクトを作る＝参照比較で足りる）の
        // ときだけ dirty を下ろす。
        dirty: action.ir || completed ? false : state.ir !== action.savedIr,
        ...(action.ir ? { ir: action.ir } : completed ? { ir: completed } : {}),
      };
    }
    case "layout_saved":
      // 古い PUT の完了が新しいドラッグの未保存分を握り潰さないよう、送った layout と
      // 現在の layout が同一参照のときだけクリーンにする。
      return state.layout === action.layout ? { ...state, layoutDirty: false } : state;
    // undo/redo は layout も保存し直す: 保存する layout は今の IR にあるものだけに絞るため、
    // 戻したノードの座標が前の保存で落ちていることがある（手元には残っている）。
    case "undo": {
      const prev = state.undoStack.at(-1);
      if (!prev) return state;
      return {
        ...state,
        ir: prev,
        undoStack: state.undoStack.slice(0, -1),
        redoStack: [...state.redoStack, state.ir],
        dirty: true,
        layoutDirty: true,
        lastEdit: null,
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
        layoutDirty: true,
        lastEdit: null,
      };
    }
    default:
      return state;
  }
}

type EditorInit = { ir: WorkflowIr; version: number; layout: EditorLayout };

function initEditorState(initial: EditorInit): EditorState {
  return {
    ir: initial.ir,
    savedVersion: initial.version,
    // 全ノード・全きっかけの座標を手元で決めて持つ（開いただけでは保存しない。何かを動かしたら
    // 全座標が保存される）。座標の無いノードが混ざると 1 つ動かすだけで全体が平行移動するため（BUG-2）。
    layout: seedLayout(initial.ir, initial.layout),
    selection: { kind: "none" },
    dirty: false,
    layoutDirty: false,
    undoStack: [],
    redoStack: [],
    serverErrors: [],
    serverErrorsIr: null,
    serverWarnings: [],
    serverPorts: {},
    serverPortsParams: {},
    lastEdit: null,
  };
}

/// 検証の応答の出口を手元へ合わせる。応答に載ったノードはその出口に、載らなかったノードは前の出口のまま
/// （設定を直している間）、今の IR に無いノードは落とす。出口を導いたときの params（検証した IR の
/// ノードのもの）も一緒に持つ。変わらなければ同じオブジェクトを返す（ノードの描画を作り直さない）。
function mergePorts(
  state: Pick<EditorState, "ir" | "serverPorts" | "serverPortsParams">,
  ports: Record<string, string[]>,
  checked: WorkflowIr,
): Pick<EditorState, "serverPorts" | "serverPortsParams"> {
  const prev = state.serverPorts;
  const prevParams = state.serverPortsParams;
  const nextPorts: Record<string, readonly string[]> = {};
  const nextParams: Record<string, unknown> = {};
  let portsChanged = false;
  let paramsChanged = false;
  const checkedById = new Map(checked.nodes.map((n) => [n.id, n]));
  for (const n of state.ir.nodes) {
    const before = prev[n.id];
    const after = ports[n.id];
    if (after) {
      const reuse = before && sameList(before, after);
      nextPorts[n.id] = reuse ? before : after;
      if (!reuse) portsChanged = true;
      nextParams[n.id] = checkedById.get(n.id)?.params;
      if (nextParams[n.id] !== prevParams[n.id]) paramsChanged = true;
    } else if (before) {
      nextPorts[n.id] = before;
      nextParams[n.id] = prevParams[n.id];
    }
  }
  if (Object.keys(prev).length !== Object.keys(nextPorts).length) {
    portsChanged = true;
    paramsChanged = true;
  }
  return {
    serverPorts: portsChanged ? nextPorts : prev,
    serverPortsParams: paramsChanged ? nextParams : prevParams,
  };
}

function sameList(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

export function useEditorState(
  initial: EditorInit,
): [EditorState, React.Dispatch<EditorAction>] {
  const [state, rawDispatch] = React.useReducer(editorReducer, initial, initEditorState);
  // 連続入力をまとめる判定の時刻は dispatch 時に付ける（reducer を純粋に保つ）。
  const dispatch = React.useCallback(
    (action: EditorAction) =>
      rawDispatch(
        action.type === "update_node" || action.type === "update_meta" ? { ...action, at: Date.now() } : action,
      ),
    [],
  );
  return [state, dispatch];
}
