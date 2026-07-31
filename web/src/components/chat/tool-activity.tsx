"use client";

/// ツール実行の可視化（issue #386）。
///
/// 旧実装は「〜しています」1 行＋展開時のフラットな一覧で、ツール名の辞書は 4 語彙しか無く
/// URL やファイル名は一切出ていなかった。ここでは 3 段階に丸めて見せる:
///
///   1. **フェーズ行**  … いま何をしているか＋経過時間（実況の「見出し」）
///   2. **ローリング**  … 直近 3 件が下から上へ流れる。委譲込みの調査は 200 件を超えるので、
///                          全件を出すとツールの実況が画面を占領する。
///   3. **展開**       … ヘッダを押すと全件のタイムライン。ステップ境界で区切り、同一ステップ
///                          2 件以上は「並行して N 件」。成否・結果要約つき。
///
/// 生成が終わったら 1 行要約（「12 件の操作 ・ web 8 ・ 社内 3」）に畳む。
///
/// **ローリングはベルトコンベア方式**（human 指定の参照 UI と同じ動き）。行を出し入れして
/// 高さを変えるのではなく、**固定高の窓の中で列全体を 1 行ぶん上へ送る**。押し出された行は
/// 送られながら消える。高さが動かないので、隣の本文が上下に揺れない。
///
/// モーション方針（`ui/motion-primitives.tsx` 参照）: 動かすのは transform / opacity のみ。

import * as React from "react";

import { motion } from "motion/react";
import { AlertTriangle, Check, ChevronDown, Loader2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { EASE_STANDARD } from "@/components/ui/motion-primitives";
import { useNodeNames } from "@/lib/node-name-cache";
import { toolFacts } from "@/lib/tool-facts";
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
  /// 配列 index をキーにしてはいけない: ローリングは末尾で窓を切るので、新着のたびに同じ
  /// 項目の index がずれ、motion が別要素とみなして位置アニメーションが壊れる。
  key: string;
  id: string;
  name: string;
  running: boolean;
  /// ツール入力（`web_fetch` なら `{ url }`）。対象名と出典チップの表示に使う。
  input?: unknown;
  /// 成否（tool_result の ok）。実行中は undefined。
  ok?: boolean;
  /// 結果の要約（tool_result の content 先頭）。件数・出典ホストの抽出元でもある。
  result?: string;
  /// ループステップの通し番号。同じ値＝同一ステップ＝並行実行（backend `StreamEventKind::ToolCall`）。
  step?: number;
  /// サブエージェントが中継した呼び出し（#391）。**親の step 空間には属さない**ので、
  /// 束ねる時に親のステップと混ぜてはいけない。
  viaSubagent?: boolean;
  /// skill ツールで実際に読み込まれた版（skill_invoked イベント・#344）。ライブ限定の付加情報。
  skillVersion?: number;
  /// 委譲の要約（subagent_run イベント・#391）。担当範囲とステップ数を展開時に出す。
  /// 子の生イベントは親へ流れないため、UI が出せるのはこの要約だけ。ライブ限定の付加情報。
  subagent?: { boundary: string; steps: number; toolCalls: number };
};

/// 走行中にロールさせる件数（human 指定: 縦に 3 つ）。全件を出すとツールの実況が
/// 画面を占領して、肝心の応答が読めなくなる。全部見たい時はヘッダを押して展開する。
const LIVE_MAX = 3;
/// 1 件の**固定高さ**（rem・2 行ぶん）。ベルトコンベアの送り量でもあるので、行の中身が
/// 何であってもこの高さから外れてはいけない（外れると送り先がずれて列が崩れる）。
///
/// 中身（見出し＋事実行）は約 2.4rem。**余りは行と行のあいだに置く**。見出し→事実行の間隔と
/// 行間が同じだと、事実行が下の行の見出しにくっついて見え、どれがどれの結果か読めなくなる。
const ROW_REM = 3;
/// 送りの尺。参照 UI の実測（1 行ぶん送るのに約 0.27 秒）に合わせる。
const ROLL_SEC = 0.28;
/// 展開時に出す結果要約の最大文字数。1 行に収まる範囲へ切る。
const RESULT_CLIP = 160;

/// ローリングの窓。**末尾 N 件＋押し出される 1 件**を返す。
///
/// 押し出される 1 件を残すのは、消える行が「上へ送られながら薄くなる」ためで、
/// 即座に unmount すると行がパッと消えて流れが途切れる。
///
/// 窓は step ではなく**到着順**で切る。子（サブエージェント）の呼び出しは自分のループ番号を
/// 持つため step で束ねると親のステップと混ざる（#391 の実測バグ）。並行実行の件数は
/// `hiddenInStep` として別に数え、隠れている数を明示する。
function rollingWindow(items: ToolActivityItem[]): {
  slots: ToolActivityItem[];
  ghosts: number;
  hidden: number;
} {
  const slots = items.slice(-(LIVE_MAX + 1));
  const ghosts = Math.max(0, slots.length - LIVE_MAX);
  const last = items[items.length - 1];
  // いま走っている並行バッチのうち、窓に入り切らなかった数（親のステップのみで数える）。
  const batch =
    last && !last.viaSubagent && last.step !== undefined
      ? items.filter((it) => !it.viaSubagent && it.step === last.step).length
      : 0;
  return { slots, ghosts, hidden: Math.max(0, batch - LIVE_MAX) };
}

/// 同一ステップでまとめる（step が無い／子の中継は必ず単独グループに落とす）。
function groupBySteps(items: ToolActivityItem[]): ToolActivityItem[][] {
  const groups: ToolActivityItem[][] = [];
  for (const it of items) {
    const last = groups[groups.length - 1];
    const groupable =
      last &&
      !last[0].viaSubagent &&
      !it.viaSubagent &&
      last[0].step !== undefined &&
      it.step !== undefined &&
      last[0].step === it.step;
    if (groupable) last.push(it);
    else groups.push([it]);
  }
  return groups;
}

/// 経過秒。`active` が真になった時刻を起点に 1 秒ごとに進み、止まったらそこで固定する。
///
/// 長い委譲では新着イベントが数十秒来ない。数字が進んでいることだけが「生きている」証拠になる。
function useElapsed(active: boolean): number {
  const startedAt = React.useRef<number | null>(null);
  const [secs, setSecs] = React.useState(0);
  React.useEffect(() => {
    if (!active) return;
    startedAt.current ??= Date.now();
    const started = startedAt.current;
    setSecs(Math.floor((Date.now() - started) / 1000));
    const t = setInterval(() => setSecs(Math.floor((Date.now() - started) / 1000)), 1000);
    return () => clearInterval(t);
  }, [active]);
  return secs;
}

function formatElapsed(secs: number): string {
  if (secs < 60) return `${secs}秒`;
  return `${Math.floor(secs / 60)}分${String(secs % 60).padStart(2, "0")}秒`;
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
  // 開閉は**自動で閉じない**。既定は畳んだまま（直近 3 件がロールするだけ）で、ヘッダを押すと
  // 全件のタイムラインになる。**実行状態から開閉を導かない**のが要点で、`running` はステップ
  // 境界で false↔true に振れるため、そこから導くと走行中ずっとパカパカする。
  const [manualOpen, setManualOpen] = React.useState<boolean | null>(null);

  // node_id しか持たないツール（office/document/slide/csv）のファイル名を解決する。
  const nodeIds = React.useMemo(
    () => items.map((it) => nodeIdOf(it)).filter((v): v is string => v !== null),
    [items],
  );
  const nodeNames = useNodeNames(nodeIds);
  const running = streaming && items.some((it) => it.running);
  const elapsed = useElapsed(running);

  if (items.length === 0) return null;

  const open = manualOpen ?? false;
  const lastCategory = describeTool(items[items.length - 1]).category;
  const season = seasonVar(seasonIndexFor(lastCategory, running));
  const summary = summarizeTools(items);

  return (
    <div className="mb-2.5" data-testid="tool-activity">
      <button
        type="button"
        onClick={() => setManualOpen(!open)}
        aria-expanded={open}
        className="group flex w-full items-center gap-2 rounded-lg py-1 text-left text-[13px] transition-colors hover:bg-accent/30"
      >
        {running ? (
          <Loader2 className="size-3.5 shrink-0 animate-spin" style={{ color: season }} aria-hidden />
        ) : (
          <Check className="size-3.5 shrink-0" style={{ color: season }} aria-hidden />
        )}
        <span
          className={cn(
            "min-w-0 truncate font-medium",
            // 実行中は文字を光が舐める（止まって見える時間帯に「生きている」ことを伝える）。
            running ? "shiki-text-shimmer" : "text-foreground/85",
          )}
        >
          {running ? (phaseOverride ?? phaseLabel(items, true)) : `${items.length} 件の操作`}
        </span>
        {running ? (
          <span className="shrink-0 tabular-nums text-muted-foreground">
            ・ {formatElapsed(elapsed)}
          </span>
        ) : null}
        <span className="min-w-0 flex-1" />
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
        <RollingBelt items={items} nodeNames={nodeNames} />
      ) : null}
    </div>
  );
}

/// 走行中のロール（ベルトコンベア）。
///
/// 各行は**絶対配置**で `y = 行番号 × ROW_REM` に置き、新着のたびに全行の y を 1 行ぶん
/// 詰める。motion が前回の y から補間するので、列全体が上へ送られて見える。窓の高さは
/// 固定なので、新着が来ても隣の本文は 1px も動かない。
function RollingBelt({
  items,
  nodeNames,
}: {
  items: ToolActivityItem[];
  nodeNames: Record<string, string>;
}) {
  const { slots, ghosts, hidden } = rollingWindow(items);
  return (
    <div className="pl-[1.375rem]" data-testid="tool-activity-rolling">
      <div className="relative" style={{ height: `${LIVE_MAX * ROW_REM}rem` }}>
        {slots.map((it, i) => {
          const row = i - ghosts; // 0 = 窓の最上段、-1 = 押し出された行
          return (
            <motion.div
              key={it.key}
              className="absolute inset-x-0 top-0"
              // 新着は窓の下から入ってくる（初回は下端の外に置いてから定位置へ送る）。
              initial={{ y: `${LIVE_MAX * ROW_REM}rem`, opacity: 0 }}
              animate={{ y: `${row * ROW_REM}rem`, opacity: row < 0 ? 0 : 1 }}
              transition={{ duration: ROLL_SEC, ease: EASE_STANDARD }}
            >
              <ActivityLine item={it} nodeNames={nodeNames} />
            </motion.div>
          );
        })}
      </div>
      {hidden > 0 ? (
        // 並行バッチが窓に入り切らない時だけ出す。黙って切ると「3 件しか走っていない」と
        // 読まれる（実測では 6 件並行のうち 3 件しか見えていなかった）。
        <p className="-mt-1 text-[11px] tabular-nums text-muted-foreground/80">
          ほか {hidden} 件を並行実行中
        </p>
      ) : null}
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
    <div className="pl-[1.375rem]" data-testid="tool-activity-expanded">
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

/// 1 件の行（見出し＋事実行の 2 段・**高さは常に [`ROW_REM`]**）。
///
/// 事実行は結果が来ても**行が生えない**ように、最初からスケルトンで高さを取っておく。
/// 結果が返った瞬間に行が生えると、並列取得では数行が同時に動いてリスト全体がガクつく。
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
  const facts = toolFacts(item.input, item.result);
  const detail =
    showResult && item.subagent
      ? `担当範囲: ${item.subagent.boundary}（${item.subagent.steps} ステップ・ツール ${item.subagent.toolCalls} 回）`
      : showResult && item.result
        ? item.result.trim().slice(0, RESULT_CLIP)
        : null;
  // 事実行に出すものが決まったか（＝スケルトンを畳んでよいか）。
  const known = detail !== null || facts.count !== null || facts.hosts.length > 0;
  // **最初から中身がある行にはスケルトンを出さない**。出すと初回描画の透明度遷移が走り、
  // 出典チップの隣に灰色の棒が一瞬見える（`web_fetch` は取得先を呼んだ瞬間に持っている）。
  const everUnknown = React.useRef(!known);
  if (!known) everUnknown.current = true;
  return (
    <div className="flex items-start gap-2 text-[13px]" style={{ height: `${ROW_REM}rem` }}>
      <StatusIcon running={item.running} failed={failed} />
      <div className="min-w-0 flex-1">
        <span className="flex min-w-0 items-center gap-1.5">
          <Icon className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
          <span
            // 行の主役は「何を調べているか」。ここが弱いと実況が読めない（参照 UI も
            // クエリだけは本文と同じ濃さで出している）。
            className={cn("min-w-0 truncate", failed ? "text-destructive" : "text-foreground")}
          >
            {withTense(described, item.running, failed)}
          </span>
          {item.viaSubagent ? (
            <span className="shrink-0 text-[11px] text-muted-foreground/70">委譲先</span>
          ) : null}
          {item.skillVersion !== undefined ? (
            <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
              v{item.skillVersion}
            </span>
          ) : null}
        </span>
        {/* 事実行。スケルトンと中身を同じ場所に重ね、透明度だけで入れ替える（＝高さが動かない）。 */}
        <div className="relative mt-0.5 h-[1.15rem]">
          {everUnknown.current ? (
            <span
              className={cn(
                "absolute inset-y-0 left-0 flex max-w-full translate-y-[0.15rem] items-center gap-1",
                "transition-opacity duration-[var(--duration-normal)]",
                // 脈打たせてよいのは「待っている」あいだだけ。終わったのに出すものが無い
                // ツール（`fs_write` 等）で光り続けると、永久に待っているように見える。
                !known && item.running ? "opacity-100" : "opacity-0",
              )}
              aria-hidden
            >
              {/* チップと同じ形・同じ位置のプレースホルダ。結果が来ると同じ場所で入れ替わる
                  ので、行の中で何も動かない（参照 UI と同じ作法）。 */}
              {[3.5, 4.5, 3].map((w, i) => (
                <span
                  key={i}
                  className="block h-[0.9rem] animate-pulse rounded-full bg-muted-foreground/12"
                  style={{ width: `${w}rem` }}
                />
              ))}
            </span>
          ) : null}
          <div
            className={cn(
              "absolute inset-0 flex min-w-0 items-center gap-1.5 overflow-hidden",
              "transition-opacity duration-[var(--duration-normal)]",
              known ? "opacity-100" : "opacity-0",
            )}
          >
            {detail ? (
              <p className="truncate text-[12px] text-muted-foreground">{detail}</p>
            ) : (
              <>
                {facts.count !== null ? (
                  <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
                    {facts.count} 件
                  </span>
                ) : null}
                <span className="shiki-fade-r flex min-w-0 items-center gap-1">
                  {facts.hosts.map((h) => (
                    <span
                      key={h}
                      className="shrink-0 rounded-full bg-muted px-1.5 py-[1px] text-[11px] text-foreground/65"
                    >
                      {h}
                    </span>
                  ))}
                </span>
              </>
            )}
          </div>
        </div>
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
