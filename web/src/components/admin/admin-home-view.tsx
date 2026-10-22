"use client";

import * as React from "react";
import Link from "next/link";
import { ArrowRight, BarChart3, ScrollText, Sparkles, Users } from "lucide-react";

import { fetchAdminModels, fetchUsage, type AdminModelsResponse, type UsageResponse } from "@/lib/admin-ai-api";
import { Skeleton } from "@/components/ui/skeleton";
import { AdminPage, Panel, StatTile } from "./admin-page";
import { periodRange } from "./ai-usage-view";
import { EFFORT_LABEL, formatCompact, formatInt, formatJpy, formatUsd } from "./format";
import { MockBadge } from "./mock-banner";
import { metricWithData, UsageChart } from "./usage-chart";

const SHORTCUTS = [
  { href: "/admin/ai/insights", icon: BarChart3, title: "活用分析", body: "浸透・用途・効果（部署別）" },
  { href: "/admin/ai/models", icon: Sparkles, title: "モデル", body: "公開するモデル・既定・Auto・円換算" },
  { href: "/admin/members", icon: Users, title: "メンバー", body: "招待・無効化・アクセスの理由", mock: true },
  { href: "/admin/audit", icon: ScrollText, title: "監査ログ", body: "操作・権限判定・引用の記録" },
];

const METRIC_TITLE = { cost: "日別コスト", tokens: "日別トークン", calls: "日別の呼び出し" } as const;

/// 管理コンソールのホーム（今月の AI 利用量とモデル設定の要約）。
export function AdminHomeView() {
  const [usage, setUsage] = React.useState<UsageResponse | null>(null);
  const [models, setModels] = React.useState<AdminModelsResponse | null>(null);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    let active = true;
    Promise.all([fetchUsage(periodRange("month")), fetchAdminModels()])
      .then(([u, m]) => {
        if (!active) return;
        setUsage(u);
        setModels(m);
      })
      .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      active = false;
    };
  }, []);

  const enabled = models?.models.filter((m) => m.enabled) ?? [];
  const autoLabel =
    models?.models.find((m) => m.id === models.settings.auto_target_model)?.label ??
    models?.settings.auto_target_model ??
    "—";

  return (
    <AdminPage title="ホーム" description="今月の AI の利用状況と、組織の設定の要約です。">
      {error ? (
        <p className="rounded-lg border px-4 py-3 text-[13px] text-muted-foreground">
          AI の利用量を表示できません（{error}）
        </p>
      ) : !usage || !models ? (
        <div className="flex flex-col gap-3" aria-busy="true">
          <div className="grid gap-3 sm:grid-cols-3">
            <Skeleton className="h-24" />
            <Skeleton className="h-24" />
            <Skeleton className="h-24" />
          </div>
          <Skeleton className="h-56" />
        </div>
      ) : (
        <>
          <section aria-label="今月の指標" className="grid gap-3 sm:grid-cols-3">
            <StatTile
              label="今月のコスト"
              value={formatUsd(usage.totals.cost_usd_micros)}
              sub={formatJpy(usage.totals.cost_usd_micros, usage.jpy_per_usd) ?? `${usage.from} 〜`}
            />
            <StatTile
              label="呼び出し"
              value={formatInt(usage.totals.calls)}
              sub={`${formatCompact(usage.totals.prompt_tokens + usage.totals.completion_tokens)} トークン ・ 利用者 ${formatInt(usage.totals.users)} 人`}
            />
            <StatTile
              label="公開中のモデル"
              value={`${enabled.length} / ${models.models.length}`}
              sub={`Auto → ${autoLabel}${
                models.settings.auto_target_effort
                  ? `（${EFFORT_LABEL[models.settings.auto_target_effort]}）`
                  : ""
              }`}
            />
          </section>
          <Panel
            title={METRIC_TITLE[metricWithData(usage.daily)]}
            actions={
              <Link
                href="/admin/ai/usage"
                className="inline-flex items-center gap-1 text-xs text-primary hover:underline"
              >
                利用量を開く
                <ArrowRight className="size-3.5" aria-hidden />
              </Link>
            }
          >
            <div className="px-1 pb-1 pt-2">
              <UsageChart days={usage.daily} metric={metricWithData(usage.daily)} />
            </div>
          </Panel>
        </>
      )}
      <section aria-label="ショートカット" className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        {SHORTCUTS.map((s) => {
          const Icon = s.icon;
          return (
            <Link
              key={s.href}
              href={s.href}
              className="group rounded-lg border bg-card px-4 py-3.5 outline-none transition-colors hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-ring"
            >
              <div className="flex items-center gap-2 text-[13px] font-medium">
                <Icon className="size-4 text-muted-foreground" aria-hidden />
                {s.title}
                {s.mock ? <MockBadge /> : null}
                <ArrowRight
                  className="ml-auto size-3.5 text-muted-foreground transition-transform group-hover:translate-x-0.5"
                  aria-hidden
                />
              </div>
              <p className="mt-1 text-xs text-muted-foreground">{s.body}</p>
            </Link>
          );
        })}
      </section>
    </AdminPage>
  );
}
