"use client";

/// ツール実行の可視化（issue #386）。
///
/// 旧実装は「〜しています」1 行＋展開時のフラットな一覧で、ツール名の辞書は 4 語彙しか無く
/// URL やファイル名は一切出ていなかった。ここでは 3 段階に丸めて見せる:
///
///   1. **フェーズ行**  … いま何をしているか（検索 / 閲覧 / 書き込み …）
///   2. **ローリング**  … 直近 3 件（＝いま走っているステップ）だけを出す。委譲込みの調査は
///                          200 件を超えるので、全件を出すとツールの実況が画面を占領する。
///   3. **展開**       … ヘッダを押すと全件のタイムライン。ステップ境界で区切り、同一ステップ
///                          2 件以上は「並行して N 件」。成否・結果要約つき。
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
  /// 委譲の要約（subagent_run イベント・#391）。担当範囲とステップ数を展開時に出す。
  /// 子の生イベントは親へ流れないため、UI が出せるのはこの要約だけ。ライブ限定の付加情報。
  subagent?: { boundary: string; steps: number; toolCalls: number };
};

/// 走行中にロールさせる件数（human 指定: 縦に 3 つ）。全件を出すとツールの実況が
/// 画面を占領して、肝心の応答が読めなくなる。全部見たい時はヘッダを押して展開する。
const LIVE_MAX = 3;
/// 展開時に出す結果要約の最大文字数。1 行に収まる範囲へ切る。
const RESULT_CLIP = 160;
/// 詳細行の固定高さ（1 行ぶん）。結果の有無で行数が変わらないようにするための予約。
const DETAIL_LINE = "h-[1.4rem]";

/// ロールに出す範囲＝**最後のステップに属するもの**（最大 [`LIVE_MAX`] 件）。
///
/// 単純な末尾 N 件だと並列バッチが途中で割れ、「並行して 6 件」と言いながら 3 件しか
/// 出ない。ステップ境界で切れば、中身は常に「いま同時に走っているもの」になる。
/// step が無い履歴（フィールド追加前）は境界が分からないので末尾から件数で切る。
function liveWindow(items: ToolActivityItem[]): ToolActivityItem[] {
  const last = items[items.length - 1];
  if (last?.step === undefined) return items.slice(-LIVE_MAX);
  const sameStep = items.filter((it) => it.step === last.step);
  return sameStep.slice(-LIVE_MAX);
}

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
  // 開閉は**自動で閉じない**（#386 の当初実装は完了時に 1 行へ畳んでいたが、見ている最中に
  // 勝手に閉じるのが不快だった）。既定は「実行中はローリング／完了したら全件を開いたまま」で、
  // ユーザーが 1 度でも操作したらその選択を最後まで尊重する（null = 未操作）。
  const [manualOpen, setManualOpen] = React.useState<boolean | null>(null);

  // node_id しか持たないツール（office/document/slide/csv）のファイル名を解決する。
  const nodeIds = React.useMemo(
    () => items.map((it) => nodeIdOf(it)).filter((v): v is string => v !== null),
    [items],
  );
  const nodeNames = useNodeNames(nodeIds);

  if (items.length === 0) return null;

  const running = streaming && items.some((it) => it.running);
  // **常に開いたまま**。実行状態から開閉を導くと、`running` がステップ境界で false↔true に
  // 振れる（次のツールが始まるまでの一瞬、実行中の項目がゼロになる）たびに開閉が起き、
  // 走行中ずっとパカパカする。開閉はユーザーの操作だけで変わる。
  // 既定は**畳んだまま**（直近 3 件がロールするだけ）。展開すると全件のタイムラインになる。
  // **実行状態から開閉を導かない**のが要点で、`running` はステップ境界で false↔true に振れる
  // ため、そこから導くと走行中ずっとパカパカする。開閉はユーザーの操作だけで変わる。
  const open = manualOpen ?? false;
  const rolling = liveWindow(items);
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
        onClick={() => setManualOpen(!open)}
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

/// 展開時: ステップ境界で区切った全件タイムライン。
/// 走行中のロール表示。新着は下から入り、押し出された行は上へ抜ける。
function RollingList({
  items,
  nodeNames,
}: {
  items: ToolActivityItem[];
  nodeNames: Record<string, string>;
}) {
  return (
    <div className="shiki-dash-top px-3 pb-2 pt-1.5" data-testid="tool-activity-rolling">
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
  const subagent = showResult ? item.subagent : undefined;
  // 詳細行は**行数を固定**する（`DETAIL_LINE`）。結果が返った瞬間に行が生えると、
  // 走行中ずっと下の行が押し下げられてリストがガクガク動く（並列 fetch では数行が同時に動く）。
  // 高さは最初から確保し、埋まるのを待つ間はスケルトンを置く。
  const detail = subagent
    ? `担当範囲: ${subagent.boundary}（${subagent.steps} ステップ・ツール ${subagent.toolCalls} 回）`
    : result;
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
        {showResult ? (
          <div className={cn("mt-0.5 overflow-hidden", DETAIL_LINE)}>
            {detail ? (
              <p className="truncate text-[12px] leading-[1.4rem] text-muted-foreground">{detail}</p>
            ) : (
              // 結果待ち。中身が来る場所を先に見せる（幅は固定＝進捗を偽装しない）。
              <span
                className="block h-2 w-40 max-w-full animate-pulse rounded-full bg-muted-foreground/15"
                aria-hidden
              />
            )}
          </div>
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
