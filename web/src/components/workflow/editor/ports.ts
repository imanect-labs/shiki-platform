/// ノードの出力ポート（描画を持たない・キャンバスのハンドル・追加先・エディタの reducer が同じ規則で使う）。
///
/// 出口の決まり方はカタログの `port_rule`（Rust の単一定義から生成・ir.md §5.2）が決める。
/// - `decide_route`（AI で判定）: 出口はサーバが params から導いたもの（検証 API の `node_ports`）を使い、
///   TS には導出を書かない（H15）。応答が来るまではカタログの静的な出口（無い）だけ。
/// - それ以外（静的・振り分け・承認）は従来どおり手元で導く（設定を変えた同じ 1 手で消えた出口のエッジを外す・
///   BUG-30）。

import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { Edge, Node as IrNode } from "@/generated/workflow-ir";

/// 検証 API が導いた出口（ノード id → 出口の名前）。応答に無いノードは載らない。
export type ServerPorts = Readonly<Record<string, readonly string[]>>;

/// 出口をサーバの導出（`node_ports`）から描くノードか。
export function portsFromServer(nodeType: string): boolean {
  return NODE_CATALOG.find((c) => c.type === nodeType)?.port_rule === "decide_route";
}

/// ノードの出力ポート（カタログ＋設定から導出・ir.md §5.2）。
///
/// `known` はこのノードについてサーバが導いた出口（`ServerPorts[node.id]`）。`decide_route` のノードだけが使う。
export function outputPorts(irNode: IrNode, known?: readonly string[]): string[] {
  const entry = NODE_CATALOG.find((c) => c.type === irNode.type);
  let ports: string[];
  if (entry?.port_rule === "decide_route") {
    // 出口の集合はサーバの導出に従う。エラー時の出口（どのノードにも共通の構造ポート）だけは、
    // 「失敗したとき」を変えた同じ 1 手で出し入れする（応答を待たずにハンドルとエッジを揃える）。
    ports = (known ?? entry.output_ports).filter((p) => p !== "error");
  } else if (irNode.type === "control.switch") {
    const cases = (irNode.params as { cases?: { port: string }[] } | null)?.cases ?? [];
    // どの case にも一致しない値は実行エンジンが `default` へ流す（control/mod.rs）。
    // フォールバック経路を配線できるよう常に default ハンドルを出す（`out` は発しない）。
    ports = [...new Set([...cases.map((c) => c.port), "default"])];
  } else if (irNode.type === "human.approval") {
    // 却下・差し戻しは指定したときだけ。取り下げは常に・時間切れは continue のとき（vocab::approval_ports と同じ規則）。
    const p = (irNode.params ?? {}) as { reject?: unknown; return?: unknown; on_timeout?: string };
    ports = ["approved"];
    if (p.reject != null) ports.push("rejected");
    if (p.return != null) ports.push("returned");
    ports.push("withdrawn");
    if (p.on_timeout === "continue") ports.push("timeout");
    if (irNode.on_error === "continue") ports.push("error");
    return ports;
  } else {
    ports = [...(entry?.output_ports ?? ["out"])];
  }
  if (
    irNode.type === "control.wait" &&
    (irNode.params as { on_timeout?: string } | null)?.on_timeout === "continue"
  ) {
    ports.push("timeout");
  }
  if (irNode.on_error === "continue") ports.push("error");
  return ports;
}

/// 構造ポート（どのノードにも付き得る、失敗・時間切れのときの出口）。続きの処理の出口ではないので、尻尾の＋・
/// 流れの末尾への追加ではつながない。
const STRUCTURAL_PORTS = new Set(["error", "timeout"]);

/// ノード種の既定の出力ポート（挿入・尻尾追加でエッジを張るときの継続側・置いたばかりの既定の設定での出口）。
/// switch は cases から動的導出のため、常に存在するフォールバック `default` を使う
/// （実行エンジンは `out` を発しないので `out` エッジは不発になる）。
/// 判定（`decide_route`）は置いたときに振り分けを持たない（`defaultParams`）ので `out`（ir.md §7.11）。
export function defaultOutPort(nodeType: string): string {
  const entry = NODE_CATALOG.find((c) => c.type === nodeType);
  if (entry?.port_rule === "switch_cases") return "default";
  return entry?.output_ports[0] ?? "out";
}

/// 続きの処理をつなぐ出口（尻尾の＋・流れの末尾への追加）: 構造ポートを除いた最初の出口。無ければ既定の出口
/// （判定で出口がまだ決まらない間に「失敗したとき: 続ける」にしても、error へはつながない）。
/// `ports` は描く出口（今の設定に無い出口は除いて渡す）か、規則の出口。
export function continuationPort(nodeType: string, ports: readonly string[]): string {
  return ports.find((p) => !STRUCTURAL_PORTS.has(p)) ?? defaultOutPort(nodeType);
}

/// 構造ポート（error・timeout）か。
export function isStructuralPort(port: string): boolean {
  return STRUCTURAL_PORTS.has(port);
}

/// 判定のノードで、サーバの出口（続きの処理の出口）がまだ無いか（置いたばかり・設定が契約に合わない間）。
/// 構造ポート（error）だけでは出口が決まったことにしない。
export function exitsPending(irNode: IrNode, known: readonly string[] | undefined): boolean {
  return portsFromServer(irNode.type) && !outputPorts(irNode, known).some((p) => !STRUCTURAL_PORTS.has(p));
}

/// 描く出口の 1 つ。`missing` は今の設定に無い出口（そこから出るエッジがある・保存は `ir.unknown_port` で止まる）。
export type DrawnPort = { port: string; missing: boolean };

/// 描く出口: 規則の出口に、規則に無い出口から出るエッジの出口を足したもの。
///
/// 無い出口のエッジもハンドルを描かないとキャンバスに線が出ず、選んで消せない（読み込んだ定義の誤り・判定の
/// 振り分けを変えた後のエッジ）。判定のノードで、サーバの導出がまだ今の設定のものでない間（開いた直後・
/// 設定を変えてから次の検証の応答まで・設定が契約に合わない間）は、エッジの出口を誤りと決めつけずにそのまま描く。
///
/// `edgePorts` はこのノードから出るエッジの出口（[`edgePortsOf`]）。`fresh` は `known` が今の設定から
/// 導いたものか（エディタの `serverPortsParams` と今の params が同じ）。
export function drawnPorts(
  irNode: IrNode,
  known: readonly string[] | undefined,
  edgePorts: readonly string[],
  fresh = true,
): DrawnPort[] {
  const ports = outputPorts(irNode, known);
  const pending = portsFromServer(irNode.type) && (known === undefined || !fresh);
  const out: DrawnPort[] = ports.map((port) => ({ port, missing: false }));
  for (const port of edgePorts) {
    if (out.some((p) => p.port === port)) continue;
    out.push({ port, missing: !pending });
  }
  return out;
}

/// ノードから出るエッジの出口（重なりなし・エッジの順）。
export function edgePortsOf(nodeId: string, edges: readonly Edge[]): string[] {
  const out: string[] = [];
  for (const e of edges) {
    if (e.from !== nodeId) continue;
    const port = e.from_port ?? "out";
    if (!out.includes(port)) out.push(port);
  }
  return out;
}
