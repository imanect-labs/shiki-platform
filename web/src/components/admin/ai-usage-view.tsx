"use client";

import * as React from "react";
import { AlertCircle, Download } from "lucide-react";

import {
  fetchAdminModels,
  fetchUsage,
  type AdminModel,
  type UsageResponse,
  type UsageRow,
} from "@/lib/admin-ai-api";
import { Button } from "@/components/ui/button";
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
import { AdminPage, Panel, StatTile } from "./admin-page";
import {
  addDays,
  formatCompact,
  formatInt,
  formatJpy,
  formatUsd,
  todayJst,
} from "./format";
import { metricWithData, UsageChart, type UsageMetric } from "./usage-chart";

type Period = "7d" | "month" | "last-month" | "90d";

const PERIODS = [
  { value: "7d", label: "7日" },
  { value: "month", label: "今月" },
  { value: "last-month", label: "先月" },
  { value: "90d", label: "90日" },
];

const SOURCES = [
  { value: "chat", label: "チャット" },
  { value: "agent", label: "エージェント" },
  { value: "workflow", label: "ワークフロー" },
  { value: "miniapp", label: "ミニアプリ" },
  { value: "other", label: "その他" },
];

const ALL = "__all__";

/// 期間 → 日本時間の日付範囲（両端含む）。
export function periodRange(period: Period, today = todayJst()): { from: string; to: string } {
  const monthStart = `${today.slice(0, 8)}01`;
  switch (period) {
    case "7d":
      return { from: addDays(today, -6), to: today };
    case "90d":
      return { from: addDays(today, -89), to: today };
    case "last-month": {
      const lastEnd = addDays(monthStart, -1);
      return { from: `${lastEnd.slice(0, 8)}01`, to: lastEnd };
    }
    default:
      return { from: monthStart, to: today };
  }
}

/// 内訳 1 つ（横棒で相対量を示す・値はコスト）。
function Breakdown({
  title,
  rows,
  jpyPerUsd,
}: {
  title: string;
  rows: UsageRow[];
  jpyPerUsd: number | null | undefined;
}) {
  const max = Math.max(1, ...rows.map((r) => r.cost_usd_micros || r.tokens));
  return (
    <Panel title={title}>
      {rows.length === 0 ? (
        <p className="px-4 py-6 text-center text-xs text-muted-foreground">この期間の利用はありません</p>
      ) : (
        <ul className="flex flex-col gap-3 px-4 py-3.5">
          {rows.map((r) => {
            const basis = r.cost_usd_micros || r.tokens;
            const jpy = formatJpy(r.cost_usd_micros, jpyPerUsd);
            return (
              <li key={r.key}>
                <div className="flex items-baseline justify-between gap-3 text-[13px]">
                  <span className="min-w-0 truncate" title={r.key}>
                    {r.label}
                  </span>
                  <span className="shrink-0 tabular-nums text-foreground/85">
                    {formatUsd(r.cost_usd_micros)}
                    <span className="ml-1.5 text-xs text-muted-foreground">
                      {jpy ?? `${formatCompact(r.tokens)} tok`}
                    </span>
                  </span>
                </div>
                <div className="mt-1.5 h-1.5 rounded-full bg-muted">
                  <div
                    className="h-full rounded-full bg-primary"
                    style={{ width: `${Math.max(2, (basis / max) * 100)}%` }}
                  />
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </Panel>
  );
}

/// 日別の CSV（Excel で開けるよう BOM 付き）。
function downloadCsv(data: UsageResponse) {
  const header = "日付,コスト(USD),トークン,呼び出し";
  const lines = data.daily.map(
    (d) => `${d.day},${(d.cost_usd_micros / 1_000_000).toFixed(6)},${d.tokens},${d.calls}`,
  );
  const blob = new Blob([`﻿${[header, ...lines].join("\n")}`], {
    type: "text/csv;charset=utf-8",
  });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `ai-usage_${data.from}_${data.to}.csv`;
  a.click();
  URL.revokeObjectURL(url);
}

/// AI の利用量ダッシュボード。数字は会計（llm_usage）そのもの。
export function AiUsageView() {
  const [period, setPeriod] = React.useState<Period>("month");
  const [model, setModel] = React.useState<string>(ALL);
  const [source, setSource] = React.useState<string>(ALL);
  const [metric, setMetric] = React.useState<UsageMetric>("cost");
  const metricChosen = React.useRef(false);
  const [data, setData] = React.useState<UsageResponse | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [loading, setLoading] = React.useState(true);
  const [models, setModels] = React.useState<AdminModel[]>([]);

  React.useEffect(() => {
    let active = true;
    fetchAdminModels()
      .then((r) => active && setModels(r.models))
      .catch(() => {
        // モデル名の選択肢が出ないだけ（集計は続ける）。
      });
    return () => {
      active = false;
    };
  }, []);

  React.useEffect(() => {
    let active = true;
    setLoading(true);
    const range = periodRange(period);
    fetchUsage({
      ...range,
      model: model === ALL ? undefined : model,
      source: source === ALL ? undefined : source,
    })
      .then((r) => {
        if (!active) return;
        setData(r);
        setError(null);
        // 初回だけ、値のある指標を選ぶ（単価未設定でコストが全部 0 のときはトークン）。
        if (!metricChosen.current) setMetric(metricWithData(r.daily));
      })
      .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)))
      .finally(() => active && setLoading(false));
    return () => {
      active = false;
    };
  }, [period, model, source]);

  const rate = data?.jpy_per_usd;
  const totals = data?.totals;

  return (
    <AdminPage
      title="利用量"
      description="AI の呼び出しとコスト。課金の会計と同じ数字です（日付は日本時間）。"
      wide
      actions={
        <Button
          variant="outline"
          size="sm"
          disabled={!data}
          onClick={() => data && downloadCsv(data)}
        >
          <Download aria-hidden />
          CSV
        </Button>
      }
    >
      <div className="flex flex-wrap items-center gap-2">
        <SegmentedControl
          aria-label="期間"
          size="sm"
          options={PERIODS}
          value={period}
          onValueChange={(v) => setPeriod(v as Period)}
        />
        <Select value={model} onValueChange={setModel}>
          <SelectTrigger className="h-8 w-44 text-[13px]" aria-label="モデルで絞り込む">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>すべてのモデル</SelectItem>
            {models.map((m) => (
              <SelectItem key={m.id} value={m.id}>
                {m.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={source} onValueChange={setSource}>
          <SelectTrigger className="h-8 w-40 text-[13px]" aria-label="利用元で絞り込む">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>すべての利用元</SelectItem>
            {SOURCES.map((s) => (
              <SelectItem key={s.value} value={s.value}>
                {s.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {data ? (
          <span className="ml-auto text-xs text-muted-foreground tabular-nums">
            {data.from} 〜 {data.to}
          </span>
        ) : null}
      </div>

      {error ? (
        <EmptyState icon={AlertCircle} title="利用量を読み込めませんでした" description={error} />
      ) : !data || !totals ? (
        <div className="flex flex-col gap-4" aria-busy="true">
          <div className="grid gap-3 sm:grid-cols-3">
            <Skeleton className="h-24" />
            <Skeleton className="h-24" />
            <Skeleton className="h-24" />
          </div>
          <Skeleton className="h-64" />
        </div>
      ) : (
        <div className={loading ? "flex flex-col gap-4 opacity-60 transition-opacity" : "flex flex-col gap-4"}>
          <section aria-label="指標" className="grid gap-3 sm:grid-cols-3">
            <StatTile
              label="コスト"
              value={formatUsd(totals.cost_usd_micros)}
              sub={formatJpy(totals.cost_usd_micros, rate) ?? "円換算は「モデル」で設定できます"}
            />
            <StatTile
              label="トークン"
              value={formatCompact(totals.prompt_tokens + totals.completion_tokens)}
              sub={`入力 ${formatCompact(totals.prompt_tokens)} ・ 出力 ${formatCompact(totals.completion_tokens)}`}
            />
            <StatTile
              label="呼び出し"
              value={formatInt(totals.calls)}
              sub={`利用者 ${formatInt(totals.users)} 人`}
            />
          </section>

          <Panel
            title="日別"
            actions={
              <SegmentedControl
                aria-label="グラフの指標"
                size="sm"
                options={[
                  { value: "cost", label: "コスト" },
                  { value: "tokens", label: "トークン" },
                  { value: "calls", label: "呼び出し" },
                ]}
                value={metric}
                onValueChange={(v) => {
                  metricChosen.current = true;
                  setMetric(v as UsageMetric);
                }}
              />
            }
          >
            <div className="px-3 pb-3 pt-4">
              <UsageChart
                days={data.daily}
                metric={metric}
                label={`${data.from} から ${data.to} までの日別の${
                  metric === "cost" ? "コスト" : metric === "tokens" ? "トークン数" : "呼び出し回数"
                }`}
              />
            </div>
          </Panel>

          <div className="grid gap-3 lg:grid-cols-3">
            <Breakdown title="モデル別" rows={data.by_model} jpyPerUsd={rate} />
            <Breakdown title="ユーザー別（上位 20）" rows={data.by_user} jpyPerUsd={rate} />
            <Breakdown title="利用元別" rows={data.by_source} jpyPerUsd={rate} />
          </div>

          <p className="text-xs leading-relaxed text-muted-foreground">
            {rate
              ? `円は 1 USD = ${rate} 円（管理者が設定したレート）で換算した参考額です。請求・会計は USD で行われます。`
              : "円での参考表示は「モデル」の設定で換算レートを入れると出ます。請求・会計は USD で行われます。"}
          </p>
        </div>
      )}
    </AdminPage>
  );
}
