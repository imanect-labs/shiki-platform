"use client";

/// ツール実行の可視化（issue #386）。
///
/// 回答の中では「操作のまとまり」（連続するツール呼び出し）ごとに本文と交互に並ぶ
/// （`message-timeline.tsx`）。1 つのまとまりは**常に 1 行**で、高さが変わらない:
///
///   - **実行中**   … いま何をしているか（「〜を web で検索中」）＋経過時間。ツールが変わると
///                    1 行の窓の中で文言が下から上へ送られる（ベルトコンベアを 1 行に縮めたもの）。
///   - **合間**     … 結果が返って次の呼び出しを待つ間は、待った時間に応じた文言（親が進める）。
///   - **完了後**   … 1 件ならその操作（「〜を検索しました」）、複数なら「N 件の操作 ・ web 3 ・ 社内 1」。
///   - **展開**     … 行を押すと全件のタイムライン。ステップ境界で区切り、同一ステップ 2 件以上は
///                    「並行して N 件」。成否・結果要約つき。自動では開閉しない。
///
/// 旧実装は直近 3 件の窓（3 行）を実行中だけ出していたため、使った瞬間に開いて終わると閉じ
/// （パカパカする）、出しっぱなしにすると往復する回答では縦に長くなった。1 行に収めれば、
/// 開閉も伸び縮みも起きない。
///
/// モーション方針（`ui/motion-primitives.tsx` 参照）: 動かすのは transform / opacity のみ。

import * as React from "react";

import { AnimatePresence, motion } from "motion/react";
import { AlertTriangle, Check, ChevronDown, Loader2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { EASE_STANDARD } from "@/components/ui/motion-primitives";
import { useNodeNames } from "@/lib/node-name-cache";
import { toolFacts } from "@/lib/tool-facts";
import type { Citation } from "@/lib/chat-api";
import { isWebCitation } from "@/lib/citation";
import {
  describeTool,
  nodeIdOf,
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

/// 展開時の 1 件の**固定高さ**（rem・2 行ぶん）。中身（見出し＋事実行）は約 2.4rem。
/// **余りは行と行のあいだに置く**。見出し→事実行の間隔と行間が同じだと、事実行が下の行の
/// 見出しにくっついて見え、どれがどれの結果か読めなくなる。
const ROW_REM = 3;
/// 1 行の窓で文言を送る尺。参照 UI の実測（1 行ぶん送るのに約 0.27 秒）に合わせる。
const ROLL_SEC = 0.28;
/// 展開時に出す結果要約の最大文字数。1 行に収まる範囲へ切る。
const RESULT_CLIP = 160;

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

/// 参照ドキュメントのチップ（最大 MAX_CITATION_CHIPS 文書）。フェーズ行の右端に並べる。
const MAX_CITATION_CHIPS = 3;

function clipLabel(s: string, max: number): string {
  return s.length <= max ? s : `${s.slice(0, max)}…`;
}

/// 参照文書のチップ名。ファイル名を優先し、未解決（読み込み中・権限なし）の間は見出しで代える。
function citationChipLabel(c: Citation, nodeNames: Record<string, string>): string {
  const name = nodeNames[c.node_id];
  if (name) return name;
  const path = c.heading_path;
  if (path && path.length > 0) return path[path.length - 1];
  const snippet = c.snippet?.trim();
  if (snippet) return clipLabel(snippet, 20);
  return "ドキュメント";
}

/// 参照文書のチップ。引用はチャンク単位で届くため、同じ文書は 1 つにまとめる。
function CitationChips({
  citations,
  nodeNames,
}: {
  citations: Citation[];
  nodeNames: Record<string, string>;
}) {
  const docs = React.useMemo(() => {
    const seen = new Set<string>();
    return citations.filter((c) =>
      // 閲覧できない出典（番号だけ）はファイルのチップにしない。Web の出典はツールの行が
      // 取得先のホストを既に出しているので重ねない。
      c.withheld || isWebCitation(c) || seen.has(c.node_id) ? false : (seen.add(c.node_id), true),
    );
  }, [citations]);
  if (docs.length === 0) return null;
  return (
    <span className="flex min-w-0 shrink items-center gap-1">
      <span className="shiki-fade-r flex min-w-0 items-center gap-1 overflow-hidden">
        {docs.slice(0, MAX_CITATION_CHIPS).map((c) => (
          <span
            key={c.node_id}
            className="max-w-[12rem] shrink-0 truncate rounded-full bg-muted px-1.5 py-[1px] text-[11px] text-foreground/65"
          >
            {citationChipLabel(c, nodeNames)}
          </span>
        ))}
      </span>
      {docs.length > MAX_CITATION_CHIPS ? (
        <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground/70">
          +{docs.length - MAX_CITATION_CHIPS}
        </span>
      ) : null}
    </span>
  );
}

export function ToolActivity({
  items,
  streaming = false,
  /// 計画のサブタスク（`plan` の doing）。あればフェーズ行に優先して出す。
  phaseOverride = null,
  /// 参照ドキュメント。running 時のフェーズ行に最大 MAX_CITATION_CHIPS 件のチップとして表示。
  citations = [],
  /// ステップの合間（次のイベント待ち）のフェーズ行の文言。待った時間に応じて親が進める。
  waitingLabel = null,
}: {
  items: ToolActivityItem[];
  streaming?: boolean;
  phaseOverride?: string | null;
  citations?: Citation[];
  waitingLabel?: string | null;
}) {
  // 開閉は**自動で変えない**。既定は直近 3 件の窓（実行中はロールし、終わっても残す）で、
  // ヘッダを押すと全件のタイムラインになる。実行状態から開閉を導くと、ツールを使った瞬間に
  // 開き、終わる（または本文が流れ始める）と閉じるので、ステップごとにパカパカする。
  const [manualOpen, setManualOpen] = React.useState<boolean | null>(null);

  // node_id しか持たないツール（office/document/slide/csv）と citation のファイル名を解決する。
  const nodeIds = React.useMemo(() => {
    const ids = items.map((it) => nodeIdOf(it)).filter((v): v is string => v !== null);
    for (const c of citations) if (!isWebCitation(c)) ids.push(c.node_id);
    return [...new Set(ids)];
  }, [items, citations]);
  const nodeNames = useNodeNames(nodeIds);
  const hasTools = items.length > 0;
  const toolsRunning = hasTools && items.some((it) => it.running);
  // ステップの合間（結果が返り、次の呼び出しを待つ間）も実行中として扱う。ツールの running
  // だけで導くと、ステップ境界のたびに見出しが ✓ と回転を行き来する。`streaming` は呼び出し側が
  // 「この操作のまとまりが回答の末尾にある（後に本文が来ていない）」ときだけ立てる。
  const between = streaming && hasTools && !toolsRunning;
  // 描画対象が無い間は計時しない（生成開始直後の空振りを避ける）。
  const running =
    streaming && (toolsRunning || between || (!hasTools && citations.length > 0));
  const elapsed = useElapsed(running);

  if (!hasTools && citations.length === 0) return null;

  // ツール呼び出しが無く citation だけ届いた経路（pre-filter 等）。チップだけ出す。
  // 進行表示は ChainOfThought が担うので、ここではステータス行を重ねない。
  if (!hasTools) {
    if (streaming) {
      return (
        <div
          className="mb-2.5 flex items-center py-1 pl-[1.375rem] text-[13px]"
          data-testid="tool-activity"
        >
          <CitationChips citations={citations} nodeNames={nodeNames} />
        </div>
      );
    }
    return (
      <div className="mb-2.5" data-testid="tool-activity">
        <div className="flex w-full items-center gap-2 rounded-lg py-1 text-[13px]">
          <Check className="size-3.5 shrink-0" style={{ color: seasonVar(3) }} aria-hidden />
          <span className="shrink-0 font-medium text-foreground/85">参照ドキュメント</span>
          <CitationChips citations={citations} nodeNames={nodeNames} />
        </div>
      </div>
    );
  }

  const open = manualOpen ?? false;
  const lastCategory = describeTool(items[items.length - 1]).category;
  const season = seasonVar(seasonIndexFor(lastCategory, running));
  const summary = summarizeTools(items);
  const runningItems = items.filter((it) => it.running);
  const failed = items.filter((it) => it.ok === false).length;
  const nameOf = (it: ToolActivityItem) => {
    const id = nodeIdOf(it);
    return id ? nodeNames[id] : null;
  };
  // 1 行に出す文言。実行中は「いま何をしているか」、合間は待ちの文言、終わったら要約。
  const current = runningItems[runningItems.length - 1];
  const only = items.length === 1 ? items[0] : null;
  const label = toolsRunning
    ? (phaseOverride ?? withTense(describeTool(current, nameOf(current)), true, false))
    : running
      ? (phaseOverride ?? waitingLabel ?? "次の手順を考えています")
      : only
        ? withTense(describeTool(only, nameOf(only)), false, only.ok === false)
        : `${items.length} 件の操作`;
  // 文言の送り方: 実行中のツールが変わるたびに 1 行ぶん送る（同じ文言のまま待つ間は動かさない）。
  const tickerKey = toolsRunning ? (phaseOverride ?? current.key) : label;

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
        ) : only && failed > 0 ? (
          <AlertTriangle className="size-3.5 shrink-0 text-destructive" aria-hidden />
        ) : (
          <Check className="size-3.5 shrink-0" style={{ color: season }} aria-hidden />
        )}
        <Ticker
          tickerKey={tickerKey}
          animate={streaming}
          className={cn(
            "font-medium",
            // 実行中は文字を光が舐める（止まって見える時間帯に「生きている」ことを伝える）。
            running ? "shiki-text-shimmer" : only && failed > 0 ? "text-destructive" : "text-foreground/85",
          )}
        >
          {label}
        </Ticker>
        {toolsRunning && runningItems.length > 1 ? (
          // 並行実行は 1 行に収まらない。黙って 1 件だけ出すと「1 件しか走っていない」と読まれる。
          <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
            ほか {runningItems.length - 1} 件
          </span>
        ) : null}
        {running ? (
          <span className="shrink-0 tabular-nums text-muted-foreground">
            ・ {formatElapsed(elapsed)}
          </span>
        ) : null}
        <CitationChips citations={citations} nodeNames={nodeNames} />
        {!running && !only && summary.length > 0 ? (
          // 種類別の内訳は、狭い幅では見出しより先に省略する。
          <span className="min-w-0 truncate text-[11px] tabular-nums text-muted-foreground">
            {summary.join(" ・ ")}
          </span>
        ) : running && items.length > 1 ? (
          <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
            {items.length} 件目
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

      {open ? <ExpandedTimeline items={items} nodeNames={nodeNames} /> : null}
    </div>
  );
}

/// 1 行の窓。`tickerKey` が変わると、前の文言が上へ送られながら消え、新しい文言が下から入る
/// （ベルトコンベアを 1 行に縮めたもの）。窓の高さは 1 行で固定なので、隣は 1px も動かない。
/// 生成中でないとき（履歴）は送らない。
function Ticker({
  tickerKey,
  animate,
  className,
  children,
}: {
  tickerKey: string;
  animate: boolean;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <span className="relative block h-5 min-w-0 flex-1 overflow-hidden" data-testid="tool-activity-ticker">
      <AnimatePresence initial={false}>
        <motion.span
          key={tickerKey}
          className="absolute inset-0 flex items-center"
          initial={animate ? { y: "100%", opacity: 0 } : false}
          animate={{ y: 0, opacity: 1 }}
          exit={animate ? { y: "-100%", opacity: 0 } : { opacity: 0 }}
          transition={{ duration: animate ? ROLL_SEC : 0, ease: EASE_STANDARD }}
        >
          <span className={cn("min-w-0 truncate", className)}>{children}</span>
        </motion.span>
      </AnimatePresence>
    </span>
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
