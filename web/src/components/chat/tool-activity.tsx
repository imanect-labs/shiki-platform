"use client";

/// ツール実行の可視化（issue #386）。
///
/// 旧実装は「〜しています」1 行＋展開時のフラットな一覧で、ツール名の辞書は 4 語彙しか無く
/// URL やファイル名は一切出ていなかった。ここでは 3 段階に丸めて見せる:
///
///   1. **フェーズ行**  … いま何をしているか（検索 / 閲覧 / 書き込み …）
///   2. **ローリング**  … 直近 3 件の具体的な操作（新着が下から入り、古い行が上へ抜ける）
///   3. **インライン展開** … 全件のタイムライン。ステップ境界で区切り、
///                          同一ステップ 2 件以上は「並行して N 件」。所要時間・成否・結果要約つき。
///
/// 生成が終わったら 1 行要約（「12 件の操作 ・ web 8 ・ 社内 3」）に畳む。
///
/// モーション方針（`ui/motion-primitives.tsx` 参照）: 同時にマウントされる要素は 3〜4 個だけなので
/// AnimatePresence を使ってよい「点在する少数の見せ場」に当たる。動かすのは transform / opacity のみ。

import * as React from "react";

import { AnimatePresence, motion } from "motion/react";
import { AlertTriangle, Check, ChevronDown, Loader2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { DURATION_NORMAL, EASE_STANDARD } from "@/components/ui/motion-primitives";
import { useNodeNames } from "@/lib/node-name-cache";
import {
  describeTool,
  nodeIdOf,
  phaseLabel,
  seasonIndexFor,
  summarizeTools,
  withTense,
} from "@/lib/tool-display";

export type ToolActivityItem = {
  /// 描画上の一意キー。**呼び出し ID はループで再利用され得る**（stub の `loop:` は毎ステップ
  /// `stubtool_1` を出す）ため、生成時に採番した「この出現」のキーを使う。
  ///
  /// 配列 index をキーにしてはいけない: ローリングは `slice(-3)` で窓を切るので、
  /// 新着のたびに同じ項目の index がずれ、AnimatePresence が別要素とみなして
  /// 再マウントする（古い行がスライドして抜ける動きが壊れる）。
  key: string;
  id: string;
  name: string;
  running: boolean;
  /// ツール入力（`web_fetch` なら `{ url }`）。対象名の表示に使う。
  input?: unknown;
  /// 成否（tool_result の ok）。実行中は undefined。
  ok?: boolean;
  /// 結果の要約（tool_result の content 先頭）。展開時のみ表示する。
  result?: string;
  /// ループステップの通し番号。同じ値＝同一ステップ＝並行実行（backend `StreamEventKind::ToolCall`）。
  step?: number;
  /// skill ツールで実際に読み込まれた版（skill_invoked イベント・#344）。ライブ限定の付加情報。
  skillVersion?: number;
};

/// ローリング表示に同時に見せる件数（human 指定: 縦に 3 つずつ）。
const ROLLING_WINDOW = 3;
/// 展開時に出す結果要約の最大文字数。
const RESULT_CLIP = 160;

/// 同一ステップでまとめる（step が無い古いイベントは単独グループに落とす）。
function groupBySteps(items: ToolActivityItem[]): ToolActivityItem[][] {
  const groups: ToolActivityItem[][] = [];
  for (const it of items) {
    const last = groups[groups.length - 1];
    // step 不明（フィールド追加前の履歴）は必ず単独グループ＝逐次実行として扱う。
    if (last && last[0].step !== undefined && it.step !== undefined && last[0].step === it.step)
      last.push(it);
    else groups.push([it]);
  }
  return groups;
}

export function ToolActivity({
  items,
  streaming = false,
  /// 計画のサブタスク（`plan` の doing）。あればフェーズ行に優先して出す。
  phaseOverride = null,
}: {
  items: ToolActivityItem[];
  streaming?: boolean;
  phaseOverride?: string | null;
}) {
  const [open, setOpen] = React.useState(false);

  // node_id しか持たないツール（office/document/slide/csv）のファイル名を解決する。
  const nodeIds = React.useMemo(
    () => items.map((it) => nodeIdOf(it)).filter((v): v is string => v !== null),
    [items],
  );
  const nodeNames = useNodeNames(nodeIds);

  if (items.length === 0) return null;

  const running = streaming && items.some((it) => it.running);
  const rolling = items.slice(-ROLLING_WINDOW);
  const lastCategory = describeTool(items[items.length - 1]).category;
  const season = seasonVar(seasonIndexFor(lastCategory, running));
  const summary = summarizeTools(items);

  return (
    <div
      className={cn(
        "relative mb-2.5 overflow-hidden rounded-xl border bg-card/40",
        // 実行中は枠をわずかに締めるだけにする。`.shiki-running-border`（回る光の弧）は
        // --primary が濃紺のため、幅のあるカードでは「黒い弧」として重く出る
        // （globals.css の「選択/アクティブを黒枠で示さない」方針に反する）。
        // 実行中であることは季節色のスピナーとフェーズ行が担う。
        "transition-colors duration-[var(--duration-normal)] ease-[var(--ease-standard)]",
        running ? "border-border" : "border-border/60",
      )}
      data-testid="tool-activity"
    >
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 px-3 py-2 text-left text-[13px] transition-colors hover:bg-accent/40"
      >
        {running ? (
          <Loader2 className="size-3.5 shrink-0 animate-spin" style={{ color: season }} aria-hidden />
        ) : (
          <Check className="size-3.5 shrink-0" style={{ color: season }} aria-hidden />
        )}
        <span className="min-w-0 flex-1 truncate font-medium text-foreground/85">
          {running ? (phaseOverride ?? phaseLabel(items, true)) : `${items.length} 件の操作`}
        </span>
        {summary.length > 0 ? (
          <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
            {running ? `${items.length} 件の操作` : summary.join(" ・ ")}
          </span>
        ) : null}
        <ChevronDown
          className={cn(
            "size-3.5 shrink-0 text-muted-foreground transition-transform",
            "duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
            open && "rotate-180",
          )}
          aria-hidden
        />
      </button>

      {open ? (
        <ExpandedTimeline items={items} nodeNames={nodeNames} />
      ) : running ? (
        <RollingList items={rolling} nodeNames={nodeNames} />
      ) : null}
    </div>
  );
}

/// 直近 N 件のローリング。新着は下から入り、押し出された行は上へ抜ける。
function RollingList({
  items,
  nodeNames,
}: {
  items: ToolActivityItem[];
  nodeNames: Record<string, string>;
}) {
  return (
    <div className="shiki-dash-top px-3 pb-2 pt-1.5">
      <AnimatePresence initial={false}>
        {items.map((it) => (
          <motion.div
            key={it.key}
            layout
            initial={{ opacity: 0, y: 8 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -8 }}
            transition={{ duration: DURATION_NORMAL, ease: EASE_STANDARD }}
          >
            <ActivityLine item={it} nodeNames={nodeNames} />
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}

/// 展開時: ステップ境界で区切った全件タイムライン。
function ExpandedTimeline({
  items,
  nodeNames,
}: {
  items: ToolActivityItem[];
  nodeNames: Record<string, string>;
}) {
  const groups = groupBySteps(items);
  return (
    <div className="shiki-dash-top px-3 pb-2.5 pt-1.5" data-testid="tool-activity-expanded">
      {groups.map((group, gi) => (
        <div key={group[0].key} className={cn(gi > 0 && "shiki-dash-top mt-1.5 pt-1.5")}>
          {group.length > 1 ? (
            <div className="mb-0.5 text-[11px] font-medium uppercase tracking-[0.06em] text-muted-foreground/70">
              並行して {group.length} 件
            </div>
          ) : null}
          {group.map((it) => (
            <ActivityLine key={it.key} item={it} nodeNames={nodeNames} showResult />
          ))}
        </div>
      ))}
    </div>
  );
}

function ActivityLine({
  item,
  nodeNames,
  showResult = false,
}: {
  item: ToolActivityItem;
  nodeNames: Record<string, string>;
  showResult?: boolean;
}) {
  const nodeId = nodeIdOf(item);
  const described = describeTool(item, nodeId ? nodeNames[nodeId] : null);
  const Icon = described.icon;
  const failed = item.ok === false;
  const result = showResult && item.result ? item.result.trim().slice(0, RESULT_CLIP) : null;
  return (
    <div className="flex items-start gap-2 py-0.5 text-[13px]">
      <StatusIcon running={item.running} failed={failed} />
      <div className="min-w-0 flex-1">
        <span className="flex min-w-0 items-center gap-1.5">
          <Icon className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
          <span
            className={cn("min-w-0 truncate", failed ? "text-destructive" : "text-foreground/90")}
          >
            {withTense(described, item.running, failed)}
          </span>
          {item.skillVersion !== undefined ? (
            <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
              v{item.skillVersion}
            </span>
          ) : null}
        </span>
        {result ? (
          <p className="mt-0.5 line-clamp-2 whitespace-pre-wrap text-[12px] leading-relaxed text-muted-foreground">
            {result}
          </p>
        ) : null}
      </div>
    </div>
  );
}

function StatusIcon({ running, failed }: { running: boolean; failed: boolean }) {
  if (running) {
    return (
      <Loader2
        className="mt-[3px] size-3.5 shrink-0 animate-spin text-muted-foreground"
        aria-hidden
      />
    );
  }
  if (failed) {
    return <AlertTriangle className="mt-[3px] size-3.5 shrink-0 text-destructive" aria-hidden />;
  }
  return <Check className="mt-[3px] size-3.5 shrink-0 text-primary" aria-hidden />;
}
