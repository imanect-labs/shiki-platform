"use client";

/// AI 活用分析（#751）の画面で共通の部品: 期間・部署の絞り込み、読み込み、数値の表記、
/// 横棒・ヒートマップのセル。
///
/// 横棒とヒートマップは、グラフの系列の 1 番目の色（`--chart-1`＝冬・雪空。チャットの本文の図と
/// 同じ・#783）の濃淡だけにする。用途や部署は名前で区別し（行ラベル）、色で区別しない。量はバーの
/// 長さ、割合はセルの濃さ。利用区分（ヘビー・レギュラー・ライト）だけは系列色の 1〜3 番目で塗り分け、
/// 凡例を添える。状態色（成功・警告）は使わない。

import * as React from "react";
import Link from "next/link";
import { AlertCircle, ArrowDownRight, ArrowRight, ArrowUpRight, Minus } from "lucide-react";

import {
  fetchInsightDepartments,
  type InsightDepartment,
  type InsightsQuery,
} from "@/lib/insights-api";
import { cn } from "@/lib/utils";
import { EmptyState } from "@/components/ui/empty-state";
import { SegmentedControl } from "@/components/ui/segmented-control";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { addDays, formatInt, todayJst } from "../format";

export type InsightsPeriod = "30d" | "90d" | "month" | "last-month";

const PERIODS: { value: InsightsPeriod; label: string }[] = [
  { value: "30d", label: "30日" },
  { value: "90d", label: "90日" },
  { value: "month", label: "今月" },
  { value: "last-month", label: "先月" },
];

const ALL = "__all__";
const STORAGE_KEY = "shiki.insights.filter";

/// 期間 → 日本時間の日付範囲（両端含む）。
export function insightsRange(period: InsightsPeriod, today = todayJst()): {
  from: string;
  to: string;
} {
  const monthStart = `${today.slice(0, 8)}01`;
  switch (period) {
    case "90d":
      return { from: addDays(today, -89), to: today };
    case "month":
      return { from: monthStart, to: today };
    case "last-month": {
      const lastEnd = addDays(monthStart, -1);
      return { from: `${lastEnd.slice(0, 8)}01`, to: lastEnd };
    }
    default:
      return { from: addDays(today, -29), to: today };
  }
}

type FilterState = { period: InsightsPeriod; dept: string | null };

function readFilter(): FilterState {
  try {
    const raw = sessionStorage.getItem(STORAGE_KEY);
    if (raw) {
      const v = JSON.parse(raw) as Partial<FilterState>;
      const period = PERIODS.some((p) => p.value === v.period) ? (v.period as InsightsPeriod) : "30d";
      return { period, dept: typeof v.dept === "string" ? v.dept : null };
    }
  } catch {
    // 保存できない環境（プライベートモード等）は既定値
  }
  return { period: "30d", dept: null };
}

/// 期間・部署の絞り込み。分析の画面どうしを移っても同じ絞り込みのまま（タブ内で保持）。
export function useInsightsFilter() {
  const [state, setState] = React.useState<FilterState>({ period: "30d", dept: null });
  const [ready, setReady] = React.useState(false);
  React.useEffect(() => {
    setState(readFilter());
    setReady(true);
  }, []);
  const update = React.useCallback((patch: Partial<FilterState>) => {
    setState((prev) => {
      const next = { ...prev, ...patch };
      try {
        sessionStorage.setItem(STORAGE_KEY, JSON.stringify(next));
      } catch {
        // 保存できなくても画面内では効く
      }
      return next;
    });
  }, []);
  const query = React.useMemo<InsightsQuery>(
    () => ({ ...insightsRange(state.period), dept: state.dept ?? undefined }),
    [state],
  );
  return { ...state, ready, query, update };
}

export type InsightsFilter = ReturnType<typeof useInsightsFilter>;

/// 絞り込みの部署の選択肢（5 人未満の部署は選べない）。
export function useDepartments(): InsightDepartment[] {
  const [depts, setDepts] = React.useState<InsightDepartment[]>([]);
  React.useEffect(() => {
    let active = true;
    fetchInsightDepartments()
      .then((d) => active && setDepts(d))
      .catch(() => {
        // 選択肢が出ないだけ（全体の集計は見られる）
      });
    return () => {
      active = false;
    };
  }, []);
  return depts;
}

/// 期間と部署の絞り込み（画面の上に 1 行・すべてのグラフに効く）。
export function InsightsToolbar({ filter }: { filter: InsightsFilter }) {
  const depts = useDepartments();
  // 保存されていた部署が無くなった・少人数になったら全体に戻す。
  const { dept, update } = filter;
  React.useEffect(() => {
    if (!dept || depts.length === 0) return;
    const hit = depts.find((d) => d.role_id === dept);
    if (!hit || hit.too_small) update({ dept: null });
  }, [depts, dept, update]);
  return (
    <div className="grid grid-cols-2 gap-2 md:flex md:flex-wrap md:items-center">
      <SegmentedControl
        aria-label="期間"
        size="sm"
        options={PERIODS}
        value={filter.period}
        onValueChange={(v) => filter.update({ period: v as InsightsPeriod })}
        className="col-span-2 max-md:w-full max-md:[&>*]:flex-1"
      />
      <Select
        value={filter.dept ?? ALL}
        onValueChange={(v) => filter.update({ dept: v === ALL ? null : v })}
      >
        <SelectTrigger className="col-span-2 h-8 w-full text-[13px] md:w-52" aria-label="部署で絞り込む">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ALL}>すべての部署</SelectItem>
          {depts.map((d) => (
            <SelectItem key={d.role_id} value={d.role_id} disabled={d.too_small}>
              {d.label}
              <span className="ml-1.5 text-xs text-muted-foreground">
                {d.too_small ? "5 人未満" : `${formatInt(d.members)} 人`}
              </span>
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <span className="col-span-2 text-xs tabular-nums text-muted-foreground md:ml-auto">
        {filter.query.from} 〜 {filter.query.to}
      </span>
    </div>
  );
}

/// 絞り込みが変わるたびに読み直す。読み直し中は前の表示を薄くして残す（レイアウトを動かさない）。
export function useInsightsData<T>(
  fetcher: (q: InsightsQuery) => Promise<T>,
  filter: InsightsFilter,
): { data: T | null; error: string | null; loading: boolean; reload: () => void } {
  const [data, setData] = React.useState<T | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [loading, setLoading] = React.useState(true);
  const [nonce, setNonce] = React.useState(0);
  const { ready, query } = filter;
  React.useEffect(() => {
    if (!ready) return;
    let active = true;
    setLoading(true);
    fetcher(query)
      .then((d) => {
        if (!active) return;
        setData(d);
        setError(null);
      })
      .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)))
      .finally(() => active && setLoading(false));
    return () => {
      active = false;
    };
  }, [fetcher, ready, query, nonce]);
  const reload = React.useCallback(() => setNonce((n) => n + 1), []);
  return { data, error, loading, reload };
}

/// 読み込み・エラー・本文の切り替え。
export function InsightsBody<T>({
  state,
  what,
  children,
}: {
  state: { data: T | null; error: string | null; loading: boolean };
  what: string;
  children: (data: T) => React.ReactNode;
}) {
  if (state.error && !state.data) {
    return <EmptyState icon={AlertCircle} title={`${what}を読み込めませんでした`} description={state.error} />;
  }
  if (!state.data) {
    return (
      <div className="flex flex-col gap-4" aria-busy="true">
        <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
          <Skeleton className="h-[104px]" />
          <Skeleton className="h-[104px]" />
          <Skeleton className="h-[104px]" />
          <Skeleton className="h-[104px]" />
        </div>
        <Skeleton className="h-64" />
      </div>
    );
  }
  return (
    <div
      className={cn("flex flex-col gap-4 transition-opacity", state.loading && "opacity-60")}
      aria-busy={state.loading}
    >
      {state.error ? (
        <p role="alert" className="text-[13px] text-destructive">
          {state.error}
        </p>
      ) : null}
      {children(state.data)}
    </div>
  );
}

/// 割合（0〜1）→ `42%`。`null` は「—」。
export function pct(v: number | null | undefined, digits = 0): string {
  if (v == null || !Number.isFinite(v)) return "—";
  return `${(v * 100).toFixed(digits)}%`;
}

/// 時間 → `12.5 時間`（10 時間以上は整数）。
export function hours(h: number): string {
  return `${h >= 10 ? Math.round(h).toLocaleString("ja-JP") : h.toFixed(1)} 時間`;
}

/// 円 → `¥12,300`。
export function yen(n: number | null | undefined): string {
  if (n == null) return "—";
  return `¥${Math.round(n).toLocaleString("ja-JP")}`;
}

/// 前期比（割合の差をポイントで）。色は付けず、矢印と言葉で示す。
export function DeltaPt({
  cur,
  prev,
  label = "前期",
}: {
  cur: number | null | undefined;
  prev: number | null | undefined;
  label?: string;
}) {
  if (cur == null || prev == null) return null;
  const d = Number(((cur - prev) * 100).toFixed(1));
  const Icon = d === 0 ? Minus : d > 0 ? ArrowUpRight : ArrowDownRight;
  return (
    <span className="inline-flex items-center gap-0.5 text-xs text-muted-foreground">
      <Icon className="size-3.5" aria-hidden />
      {label ? `${label}比 ` : ""}
      {d > 0 ? "+" : ""}
      {d.toFixed(1)}pt
    </span>
  );
}

/// 横棒の 1 行（ラベル・値・相対量）。棒の色は 1 色、長さだけが量を表す。
export function BarRow({
  label,
  value,
  ratio,
  sub,
  muted = false,
}: {
  label: React.ReactNode;
  value: React.ReactNode;
  /// 0〜1（棒の長さ）。`null` は棒を出さない（伏せた値）。
  ratio: number | null;
  sub?: React.ReactNode;
  muted?: boolean;
}) {
  return (
    <li>
      <div className="flex items-baseline justify-between gap-3 text-[13px]">
        <span className={cn("min-w-0 truncate", muted && "text-muted-foreground")}>{label}</span>
        <span className="shrink-0 tabular-nums">
          {value}
          {sub ? <span className="ml-1.5 text-xs text-muted-foreground">{sub}</span> : null}
        </span>
      </div>
      <div className="mt-1.5 h-1.5 rounded-full bg-muted">
        {ratio != null && ratio > 0 ? (
          <div
            className="h-full rounded-full bg-[var(--chart-1)]"
            style={{ width: `${Math.max(2, Math.min(1, ratio) * 100)}%` }}
          />
        ) : null}
      </div>
    </li>
  );
}

/// ヒートマップのセル（割合 0〜1 を `--chart-1` の濃さで）。`null` は伏せた値。
/// `scale` は濃さが最大になる値（行の内訳など値が小さく偏る表は最大値に合わせる）。
export function HeatCell({
  value,
  title,
  scale = 1,
}: {
  value: number | null | undefined;
  title?: string;
  scale?: number;
}) {
  if (value == null) {
    return (
      <td className="px-1 py-1">
        <div
          className="flex h-8 items-center justify-center rounded-[4px] text-xs text-muted-foreground/70"
          title={title ? `${title}: 5 人未満のため非表示` : "5 人未満のため非表示"}
        >
          —
        </div>
      </td>
    );
  }
  const v = Math.max(0, Math.min(1, value));
  // 6% 〜 --heat-max の濃さ。文字は本文の色のまま（上限はライト 6.9:1・ダーク 5.0:1 を保つ値・globals.css）。
  const t = Math.min(1, v / Math.max(scale, 0.0001)).toFixed(3);
  return (
    <td className="px-1 py-1">
      <div
        className="flex h-8 items-center justify-center rounded-[4px] text-xs tabular-nums text-foreground"
        style={{
          backgroundColor: `color-mix(in oklab, var(--chart-1) calc(6% + (var(--heat-max) - 6%) * ${t}), transparent)`,
        }}
        title={title ? `${title}: ${pct(v)}` : pct(v)}
      >
        {pct(v)}
      </div>
    </td>
  );
}

/// ヒートマップの濃さの凡例（`max` は最も濃い色の値）。
export function HeatLegend({ max = 1 }: { max?: number }) {
  return (
    <div className="flex items-center gap-2 text-xs tabular-nums text-muted-foreground" aria-hidden>
      0%
      <span
        className="h-2 w-24 rounded-full"
        style={{
          background:
            "linear-gradient(to right, color-mix(in oklab, var(--chart-1) 6%, transparent), color-mix(in oklab, var(--chart-1) var(--heat-max), transparent))",
        }}
      />
      {pct(max)}
    </div>
  );
}

/// パネル下の注記。
export function Note({ children }: { children: React.ReactNode }) {
  return <p className="px-4 pb-3.5 text-xs leading-relaxed text-muted-foreground">{children}</p>;
}

/// 他の画面への小さなリンク（パネル右上）。
export function MoreLink({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <Link
      href={href}
      className="inline-flex items-center gap-1 rounded-md text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      {children}
      <ArrowRight className="size-3.5" aria-hidden />
    </Link>
  );
}

/// 分析画面の説明文の共通の後半（集計のタイミング・秘匿）。
export const INSIGHTS_FOOTNOTE =
  "1 時間ごとに集計（日付は日本時間）。5 人未満の部署・区分は数値を伏せます。";
