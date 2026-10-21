"use client";

import * as React from "react";
import Link from "next/link";
import { Lightbulb } from "lucide-react";

import { fetchOverview, type InsightsOverview } from "@/lib/insights-api";
import { AdminPage, Panel, StatTile } from "../admin-page";
import { formatInt } from "../format";
import {
  BarRow,
  DeltaPt,
  hours,
  InsightsBody,
  InsightsToolbar,
  INSIGHTS_FOOTNOTE,
  MoreLink,
  Note,
  pct,
  useInsightsData,
  useInsightsFilter,
  yen,
} from "./common";
import { WeeklyChart } from "./weekly-chart";

function Funnel({ f }: { f: InsightsOverview["funnel"] }) {
  const steps = [
    { label: "利用対象者", n: f.population },
    { label: "一度でも使った", n: f.ever },
    { label: "直近 30 日に使った", n: f.last_30_days },
    { label: "毎週使う（4 週中 3 週以上）", n: f.weekly },
    { label: "ほぼ毎日（28 日中 16 日以上）", n: f.heavy },
  ];
  const base = Math.max(1, f.population);
  return (
    <ul className="flex flex-col gap-3 px-4 py-3.5">
      {steps.map((s, i) => (
        <BarRow
          key={s.label}
          label={s.label}
          value={`${formatInt(s.n)} 人`}
          sub={i === 0 ? undefined : pct(s.n / base)}
          ratio={s.n / base}
        />
      ))}
    </ul>
  );
}

function Body({ o, from, to }: { o: InsightsOverview; from: string; to: string }) {
  const depts = [...o.departments].sort((a, b) => (b.rate ?? -1) - (a.rate ?? -1));
  return (
    <>
      <section aria-label="指標" className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <StatTile
          label="利用率"
          value={pct(o.adoption_rate)}
          sub={`${formatInt(o.active_users)} / ${formatInt(o.population)} 人が使った`}
        >
          <div className="mt-1">
            <DeltaPt cur={o.adoption_rate} prev={o.adoption_rate_prev} />
          </div>
        </StatTile>
        <StatTile
          label="毎週使う人"
          value={`${formatInt(o.wau)} 人`}
          sub={`定着率（週 ÷ 月）${pct(o.stickiness)}`}
        />
        <StatTile
          label="満足度"
          value={pct(o.satisfaction)}
          sub={o.ratings > 0 ? `評価 ${formatInt(o.ratings)} 件のうち 👍` : "まだ評価がありません"}
        >
          <div className="mt-1">
            <DeltaPt cur={o.satisfaction} prev={o.satisfaction_prev} />
          </div>
        </StatTile>
        <StatTile
          label="推定削減時間"
          value={hours(o.hours_saved)}
          sub={o.yen_saved != null ? `時給換算 ${yen(o.yen_saved)}` : "前提にもとづく推定値"}
        />
      </section>

      <Panel
        title="週ごとのアクティブ利用者"
        description="その週に AI を使った人を、使った日数で分けています。"
      >
        <div className="px-4 pb-3 pt-3.5">
          <WeeklyChart weeks={o.weekly} from={from} to={to} />
        </div>
      </Panel>

      <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
        <Panel title="浸透のファネル" description="分母はディレクトリの在籍者（利用対象者）。">
          <Funnel f={o.funnel} />
          <Note>「一度使って離れた人」が多いときは、研修や使い方の周知が効きやすいサインです。</Note>
        </Panel>
        <Panel
          title="部署別の利用率"
          actions={<MoreLink href="/admin/ai/insights/adoption">利用者と部署</MoreLink>}
        >
          {depts.length === 0 ? (
            <p className="px-4 py-6 text-center text-xs text-muted-foreground">
              部署の情報がありません（ログイン時の所属グループから作られます）
            </p>
          ) : (
            <ul className="flex max-h-[296px] flex-col gap-3 overflow-y-auto px-4 py-3.5">
              {depts.map((d) => (
                <BarRow
                  key={d.role_id}
                  label={d.label}
                  value={d.rate == null ? "—" : pct(d.rate)}
                  sub={d.rate == null ? "5 人未満" : `${formatInt(d.members)} 人`}
                  ratio={d.rate ?? null}
                  muted={d.rate == null}
                />
              ))}
            </ul>
          )}
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
        <Panel
          title="よく使われる用途"
          actions={
            o.classify_enabled ? <MoreLink href="/admin/ai/insights/use-cases">用途</MoreLink> : null
          }
        >
          {!o.classify_enabled ? (
            <div className="px-4 py-5 text-[13px] text-muted-foreground">
              用途の分類はオフです。
              <Link
                href="/admin/ai/insights/settings"
                className="ml-1 font-medium text-foreground underline underline-offset-4"
              >
                分析の設定
              </Link>
              でオンにすると、会話ごとに AI が用途のラベルを付けます。
            </div>
          ) : o.top_uses.length === 0 ? (
            <p className="px-4 py-6 text-center text-xs text-muted-foreground">
              まだ分類された会話がありません
            </p>
          ) : (
            <ul className="flex flex-col gap-3 px-4 py-3.5">
              {o.top_uses.map((u) => (
                <BarRow key={u.key} label={u.label} value={pct(u.share)} ratio={u.share} />
              ))}
            </ul>
          )}
        </Panel>
        <Panel title="気づき" description="集計値から決まった規則で出しています。">
          {o.highlights.length === 0 ? (
            <p className="px-4 py-6 text-center text-xs text-muted-foreground">
              目立った変化はありません
            </p>
          ) : (
            <ul className="flex flex-col divide-y">
              {o.highlights.map((h) => (
                <li key={h} className="flex gap-2.5 px-4 py-3 text-[13px] leading-relaxed">
                  <Lightbulb className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
                  {h}
                </li>
              ))}
            </ul>
          )}
        </Panel>
      </div>
    </>
  );
}

/// 活用分析 › 概要。
export function InsightsOverviewView() {
  const filter = useInsightsFilter();
  const state = useInsightsData(fetchOverview, filter);
  return (
    <AdminPage
      title="活用分析"
      description={`従業員がどれだけ AI を使い、何が良くなったか。${INSIGHTS_FOOTNOTE}`}
      wide
    >
      <InsightsToolbar filter={filter} />
      <InsightsBody state={state} what="活用分析">
        {(o) => <Body o={o} from={filter.query.from} to={filter.query.to} />}
      </InsightsBody>
    </AdminPage>
  );
}
