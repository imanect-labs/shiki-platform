"use client";

import * as React from "react";
import Link from "next/link";

import { fetchImpact, type InsightsImpact } from "@/lib/insights-api";
import { AdminPage, Panel, StatTile } from "../admin-page";
import { formatInt, formatUsd } from "../format";
import {
  BarRow,
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

const SETTINGS = "/admin/ai/insights/settings";

function Savings({ i }: { i: InsightsImpact }) {
  const rows = [...[...i.savings].sort((a, b) => b.hours - a.hours), i.workflow];
  return (
    <Panel
      title="削減時間の内訳"
      description="完了した会話 × 人がやると何分か（前提）。"
      actions={<MoreLink href={SETTINGS}>前提を編集</MoreLink>}
    >
      <div className="overflow-x-auto">
        <table className="w-full min-w-[520px] text-[13px]">
          <caption className="sr-only">用途ごとの推定削減時間</caption>
          <thead className="whitespace-nowrap text-xs text-muted-foreground">
            <tr className="border-b">
              <th scope="col" className="px-4 py-2 text-left font-normal">用途</th>
              <th scope="col" className="px-2 py-2 text-right font-normal">完了</th>
              <th scope="col" className="px-2 py-2 text-right font-normal">前提（分/件）</th>
              <th scope="col" className="px-4 py-2 text-right font-normal">推定削減</th>
            </tr>
          </thead>
          <tbody>
            {!i.classify_enabled ? (
              <tr className="border-b">
                <td colSpan={4} className="px-4 py-3 text-xs text-muted-foreground">
                  用途の分類がオフのため、会話の削減時間は出していません（
                  <Link href={SETTINGS} className="underline underline-offset-4">
                    分析の設定
                  </Link>
                  ）。
                </td>
              </tr>
            ) : null}
            {rows.map((r) => (
              <tr key={r.key} className="border-b last:border-b-0">
                <th scope="row" className="px-4 py-2.5 text-left font-normal">{r.label}</th>
                <td className="px-2 py-2.5 text-right tabular-nums">{formatInt(r.completed)}</td>
                <td className="px-2 py-2.5 text-right tabular-nums text-muted-foreground">{r.minutes}</td>
                <td className="px-4 py-2.5 text-right tabular-nums">{hours(r.hours)}</td>
              </tr>
            ))}
          </tbody>
          <tfoot>
            <tr className="border-t">
              <th scope="row" colSpan={3} className="px-4 py-2.5 text-left font-medium">
                合計
              </th>
              <td className="px-4 py-2.5 text-right font-medium tabular-nums">{hours(i.total_hours)}</td>
            </tr>
          </tfoot>
        </table>
      </div>
      <Note>
        「完了」は、その期間に正常に終わった回答があり 👎 が付いていない会話。ワークフローは成功した実行 ×
        手作業の分数（{i.workflow_minutes} 分）。どれも前提にもとづく推定値です。
      </Note>
    </Panel>
  );
}

function Satisfaction({ title, rows, empty }: { title: string; rows: InsightsImpact["by_model"]; empty: string }) {
  return (
    <Panel title={title} description="評価された回答のうち 👍 の割合。">
      {rows.length === 0 ? (
        <p className="px-4 py-6 text-center text-xs text-muted-foreground">{empty}</p>
      ) : (
        <ul className="flex flex-col gap-3 px-4 py-3.5">
          {rows.map((r) => (
            <BarRow
              key={r.key}
              label={r.label}
              value={pct(r.rate)}
              sub={`${formatInt(r.up + r.down)} 件`}
              ratio={r.rate ?? null}
            />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function Reasons({ i }: { i: InsightsImpact }) {
  const max = Math.max(1, ...i.reasons.map((r) => r.count));
  return (
    <Panel title="👎 の理由" description="回答の下の 👎 から選ばれた理由。">
      {i.reasons.length === 0 ? (
        <p className="px-4 py-6 text-center text-xs text-muted-foreground">この期間の 👎 はありません</p>
      ) : (
        <ul className="flex flex-col gap-3 px-4 py-3.5">
          {i.reasons.map((r) => (
            <BarRow key={r.key} label={r.label} value={`${formatInt(r.count)} 件`} ratio={r.count / max} />
          ))}
        </ul>
      )}
      <Note>「内容が不正確」「社内の情報が見つからない」が多い用途は、モデル・スキル・文書の見直し候補です。</Note>
    </Panel>
  );
}

/// 数値の一覧（成果物・品質）。
function Figures({ title, description, items }: {
  title: string;
  description?: string;
  items: { label: string; value: string; sub?: string }[];
}) {
  return (
    <Panel title={title} description={description}>
      <dl className="grid grid-cols-3 divide-x">
        {items.map((x) => (
          <div key={x.label} className="px-4 py-3.5">
            <dt className="text-xs text-muted-foreground">{x.label}</dt>
            <dd className="mt-1 text-xl font-semibold tracking-tight">{x.value}</dd>
            {x.sub ? <dd className="text-xs text-muted-foreground">{x.sub}</dd> : null}
          </div>
        ))}
      </dl>
    </Panel>
  );
}

function Body({ i }: { i: InsightsImpact }) {
  const cost = i.cost_jpy != null ? yen(i.cost_jpy) : formatUsd(i.cost_usd_micros);
  return (
    <>
      <section aria-label="指標" className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <StatTile
          label="推定削減時間"
          value={hours(i.total_hours)}
          sub={i.yen_saved != null ? `時給換算 ${yen(i.yen_saved)}` : "時給を設定すると金額でも出ます"}
        />
        <StatTile
          label="満足度"
          value={pct(i.satisfaction_overall)}
          sub={i.ratings > 0 ? `評価 ${formatInt(i.ratings)} 件` : "まだ評価がありません"}
        />
        <StatTile label="AI の利用料" value={cost} sub="利用量画面と同じ会計" />
        <StatTile
          label="費用対効果"
          value={i.return_ratio != null ? `約 ${i.return_ratio.toFixed(1)} 倍` : "—"}
          sub={
            i.return_ratio != null
              ? "削減時間の金額 ÷ 利用料"
              : "時給と円換算レートを設定すると出ます"
          }
        />
      </section>

      <Savings i={i} />

      <div className="grid grid-cols-1 gap-3 lg:grid-cols-3">
        {i.classify_enabled ? (
          <Satisfaction title="満足度（用途別）" rows={i.satisfaction} empty="この期間の評価はありません" />
        ) : null}
        <Satisfaction title="満足度（モデル別）" rows={i.by_model} empty="この期間の評価はありません" />
        <Reasons i={i} />
      </div>

      <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
        <Figures
          title="成果物"
          description="この期間に作られたもの。"
          items={[
            { label: "AI が作った文書", value: formatInt(i.outputs.documents) },
            { label: "スキル", value: formatInt(i.outputs.skills_created) },
            { label: "ワークフロー", value: formatInt(i.outputs.workflows_created) },
          ]}
        />
        <Figures
          title="品質の兆候"
          description={`回答 ${formatInt(i.quality.runs)} 件のうち。`}
          items={[
            { label: "失敗", value: pct(i.quality.failed_rate, 1) },
            { label: "途中で停止", value: pct(i.quality.cancelled_rate, 1) },
            { label: "👎", value: pct(i.quality.down_rate, 1), sub: "評価のうち" },
          ]}
        />
      </div>

      <p className="text-xs leading-relaxed text-muted-foreground">
        {i.hourly_wage_jpy != null
          ? `金額は時給 ${yen(i.hourly_wage_jpy)}（管理者の設定）で換算した参考値です。`
          : "時給は分析の設定で入れられます。"}
        削減時間は前提にもとづく推定値で、会計・請求の数字ではありません。
      </p>
    </>
  );
}

/// 活用分析 › 効果。
export function InsightsImpactView() {
  const filter = useInsightsFilter();
  const state = useInsightsData(fetchImpact, filter);
  return (
    <AdminPage
      title="効果"
      description={`何が良くなったか。削減時間は前提にもとづく推定値、満足度は評価された回答だけの割合です。${INSIGHTS_FOOTNOTE}`}
      wide
    >
      <InsightsToolbar filter={filter} />
      <InsightsBody state={state} what="効果">
        {(i) => <Body i={i} />}
      </InsightsBody>
    </AdminPage>
  );
}
