"use client";

/// キャンバス上のノードカード（アイコン＋日本語ラベル＋ポート＋末尾プラスボタン＋エラーバッジ）。
///
/// - 出力ポートはカタログ＋設定から導出（branch=true/false・switch=cases・wait の timeout・
///   on_error=continue の error）。判定（ai.decide）の出口はサーバが導いたもの（検証 API の `node_ports`）を描く。
///   ポートが複数のときは右辺に縦に並べ、名前を添える。今の設定に無い出口から出るエッジがあれば、
///   その出口も「無い出口」として描く（線を選んで消せるように）。
/// - **末尾のプラスボタン**が主導線: クリックで AddNodeMenu を開き、選んだブロックを
///   このノードの既定ポートへつないで追加する（ノードの「尻尾」に生える）。

import * as React from "react";
import { Handle, Position, useUpdateNodeInternals, type NodeProps } from "@xyflow/react";
import { AlertCircle, Plus, TriangleAlert } from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type { Node as IrNode, ValidationError } from "@/generated/workflow-ir";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import { AddNodeMenu } from "./add-node-menu";
import { nodeIcon } from "./icons";
import { drawnPorts, portsFromServer, type DrawnPort } from "./ports";
import { categoryVar } from "../category-accent";

export type NodeCardData = {
  irNode: IrNode;
  errors: ValidationError[];
  /// 保存を止めない警告（条件の意味論の切替で結果が変わり得る等）。
  warnings?: ValidationError[];
  /// サーバが導いたこのノードの出口（判定のノード・検証 API の `node_ports`。まだ無ければ undefined）。
  knownPorts?: readonly string[];
  /// `knownPorts` が今の設定から導いたものか（違えば無い出口の印を付けない）。
  portsFresh?: boolean;
  /// このノードから出るエッジの出口（`edgePortsOf` の JSON）。
  edgePortsKey?: string;
  /// `fromId` の `port` から新ノードをつないで追加する（全ノード共通の参照の変わらない関数）。
  onAddFrom: (fromId: string, port: string, nodeType: string) => void;
  [key: string]: unknown;
};

const PORT_LABELS: Record<string, string> = {
  out: "",
  true: "はい",
  false: "いいえ",
  default: "その他",
  error: "エラー時",
  timeout: "時間切れ",
  approved: "承認",
  rejected: "却下",
  returned: "差し戻し",
  withdrawn: "取り下げ",
  uncertain: "要確認",
};

export function portLabel(port: string): string {
  return PORT_LABELS[port] ?? port;
}

function portColor(port: string): string {
  if (port === "error" || port === "rejected") return "!bg-destructive";
  if (port === "timeout" || port === "returned" || port === "withdrawn" || port === "uncertain") {
    return "!bg-[var(--season-autumn)]";
  }
  return "!bg-primary";
}

/// 出口の表示名（無い出口は印を添える）。
function drawnLabel(p: DrawnPort): string {
  const name = portLabel(p.port) || p.port;
  return p.missing ? `${name}（無い出口）` : name;
}

function parsePorts(key: string | undefined): string[] {
  if (!key) return [];
  try {
    const v = JSON.parse(key) as unknown;
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function NodeCard({ id, data, selected }: NodeProps & { data: NodeCardData }) {
  const { irNode, errors, onAddFrom } = data;
  const warnings = data.warnings ?? [];
  const entry = NODE_CATALOG.find((c) => c.type === irNode.type);
  const Icon = nodeIcon(irNode.type);
  const ports = React.useMemo(
    () => drawnPorts(irNode, data.knownPorts, parsePorts(data.edgePortsKey), data.portsFresh ?? true),
    [irNode, data.knownPorts, data.edgePortsKey, data.portsFresh],
  );
  // 尻尾の＋でつなぐ出口（今の設定にある最初の出口）。
  const tailPort = ports.find((p) => !p.missing)?.port ?? "out";
  // 出口が変わったら（振り分けの出口名の変更・エラー時の出口の追加など）ハンドルの位置を測り直す。
  // キャンバスは計測結果を引き継ぐので、大きさが変わらない変更では React Flow が自分では測り直さない。
  const updateNodeInternals = useUpdateNodeInternals();
  const portsKey = JSON.stringify(ports.map((p) => [p.port, p.missing]));
  const measuredPorts = React.useRef(portsKey);
  React.useEffect(() => {
    if (measuredPorts.current === portsKey) return;
    measuredPorts.current = portsKey;
    updateNodeInternals(id);
  }, [id, portsKey, updateNodeInternals]);
  const hasError = errors.length > 0;
  const [menuOpen, setMenuOpen] = React.useState(false);
  // カテゴリ由来の季節色（アイコンチップの tint で示す）。エラー時は破壊的色を優先。
  const accent = categoryVar(entry?.category);

  return (
    <div
      style={{ ["--accent" as string]: accent }}
      // ⚠️ overflow-hidden は付けない（尻尾の＋が -right-9 でカード外に出るため一緒に切れる）。
      className={cn(
        "group/node relative w-60 rounded-xl border bg-card text-card-foreground shadow-sm",
        // ⚠️ xyflow ノードは motion layout を使わず CSS transition のみ（transform/shadow/border）。
        "transition-[transform,box-shadow,border-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
        "hover:-translate-y-0.5 hover:shadow-md",
        selected && "shadow-lg ring-2 ring-primary",
        hasError && "border-destructive",
        !hasError && warnings.length > 0 && "border-[var(--season-autumn)]",
      )}
    >
      {/* 入力ポート（join は複数エッジを受けるが handle は 1 つ）。 */}
      <Handle
        type="target"
        position={Position.Left}
        className="!size-2.5 !border-2 !border-background !bg-muted-foreground transition-transform duration-[var(--duration-fast)] hover:!scale-125 pointer-coarse:!size-4"
      />

      <div className="flex items-start gap-2.5 px-3.5 py-3">
        <span
          className={cn(
            "mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg",
            hasError ? "bg-destructive/10 text-destructive" : "",
          )}
          style={
            hasError
              ? undefined
              : {
                  backgroundColor: "color-mix(in oklab, var(--accent) 14%, transparent)",
                  color: "var(--accent)",
                }
          }
        >
          <Icon className="size-4" aria-hidden />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block truncate text-sm font-medium leading-5">
            {irNode.label || entry?.label_ja || irNode.type}
          </span>
          <span className="block truncate text-xs text-muted-foreground">
            {entry?.description_ja ?? irNode.type}
          </span>
        </span>
        {hasError ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <span
                className="mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
                aria-label={`検証エラー ${errors.length} 件`}
              >
                <AlertCircle className="size-3.5" aria-hidden />
              </span>
            </TooltipTrigger>
            <TooltipContent side="top" className="max-w-72">
              <ul className="list-disc space-y-1 pl-4 text-xs">
                {errors.slice(0, 5).map((e, i) => (
                  <li key={i}>{e.message}</li>
                ))}
                {errors.length > 5 ? <li>ほか {errors.length - 5} 件</li> : null}
              </ul>
            </TooltipContent>
          </Tooltip>
        ) : null}
        {!hasError && warnings.length > 0 ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <span
                tabIndex={0}
                className="mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full text-[var(--season-autumn)] outline-none focus-visible:ring-2 focus-visible:ring-ring"
                style={{ backgroundColor: "color-mix(in oklab, var(--season-autumn) 16%, transparent)" }}
                aria-label={`注意 ${warnings.length} 件`}
              >
                <TriangleAlert className="size-3.5" aria-hidden />
              </span>
            </TooltipTrigger>
            <TooltipContent side="top" className="max-w-80">
              <ul className="list-disc space-y-1 pl-4 text-xs">
                {warnings.slice(0, 3).map((e, i) => (
                  <li key={i}>{e.message}</li>
                ))}
                {warnings.length > 3 ? <li>ほか {warnings.length - 3} 件</li> : null}
              </ul>
            </TooltipContent>
          </Tooltip>
        ) : null}
      </div>

      {/* 出力ポート（複数のときはラベル付きで下部に列挙）。 */}
      {ports.length === 0 && portsFromServer(irNode.type) ? (
        // 判定の出口はサーバが設定から導く（設定が契約に合うまでは決まらない）。
        <p className="border-t px-3.5 py-1.5 text-[11px] text-muted-foreground" data-testid="node-ports-pending">
          出口は設定を直すと出ます
        </p>
      ) : ports.length === 1 && !ports[0].missing ? (
        <Handle
          id={ports[0].port}
          type="source"
          position={Position.Right}
          className={cn(
            "!size-2.5 !border-2 !border-background transition-transform duration-[var(--duration-fast)] hover:!scale-125 pointer-coarse:!size-4",
            portColor(ports[0].port),
          )}
        />
      ) : ports.length > 0 ? (
        <div className="border-t px-3.5 py-1.5">
          {ports.map((p) => (
            <div key={p.port} className="relative flex h-6 items-center justify-end" data-port={p.port}>
              <span
                className={cn(
                  "min-w-0 truncate text-[11px]",
                  p.missing ? "font-medium text-destructive" : "text-muted-foreground",
                )}
              >
                {drawnLabel(p)}
              </span>
              <Handle
                id={p.port}
                type="source"
                position={Position.Right}
                // 行（relative）の縦の中央に置く。行ごとに `i * 24px` ずらすと、行の中でさらにずれて
                // 2 つ目以降のハンドルが下の行の名前の横に出る（線を別の出口からつなぐ取り違えになる）。
                style={{ top: "50%" }}
                isConnectableStart={!p.missing}
                className={cn(
                  "!absolute !-right-[13px] !size-2.5 !border-2 !border-background pointer-coarse:!size-4",
                  p.missing ? "!bg-destructive/40 !border-destructive" : portColor(p.port),
                )}
              />
            </div>
          ))}
        </div>
      ) : null}

      {/* 尻尾のプラスボタン（主導線・ノードの右側に生える）。 */}
      <Popover open={menuOpen} onOpenChange={setMenuOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            aria-label="次のブロックを追加"
            className={cn(
              "nodrag absolute -right-9 top-1/2 flex size-6 -translate-y-1/2 items-center justify-center pointer-coarse:-right-11 pointer-coarse:size-8",
              "rounded-full border bg-background text-muted-foreground shadow-sm",
              // マウスでは常時うっすら表示し（発見性）、ホバー/フォーカスで全表示＋プライマリ強調。
              // タッチ端末には hover が無いので最初から全表示にする。
              "transition-all duration-fast pointer-fine:opacity-60 pointer-fine:group-hover/node:opacity-100 focus-visible:opacity-100",
              "hover:border-primary hover:bg-primary hover:text-primary-foreground hover:scale-110",
              "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
              menuOpen && "opacity-100 border-primary bg-primary text-primary-foreground",
            )}
          >
            <Plus className="size-4" aria-hidden />
          </button>
        </PopoverTrigger>
        <PopoverContent side="right" align="start" className="w-auto p-3">
          <AddNodeMenu
            contextLabel={`「${irNode.label || entry?.label_ja || irNode.id}」の後ろに追加`}
            onPick={(nodeType) => {
              setMenuOpen(false);
              onAddFrom(irNode.id, tailPort, nodeType);
            }}
          />
        </PopoverContent>
      </Popover>
    </div>
  );
}
