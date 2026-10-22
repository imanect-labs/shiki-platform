"use client";

import * as React from "react";
import Link from "next/link";
import { Info, Lock } from "lucide-react";

import { fetchUseCases, type InsightsUseCases } from "@/lib/insights-api";
import { AdminPage, Panel } from "../admin-page";
import { formatInt } from "../format";
import {
  BarRow,
  DeltaPt,
  HeatCell,
  HeatLegend,
  InsightsBody,
  InsightsToolbar,
  INSIGHTS_FOOTNOTE,
  Note,
  pct,
  useInsightsData,
  useInsightsFilter,
} from "./common";

function ClassifyNotice({ u }: { u: InsightsUseCases }) {
  if (!u.classify_enabled) {
    return (
      <div className="flex gap-3 rounded-lg border bg-card px-4 py-3.5 text-[13px] leading-relaxed">
        <Info className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
        <p>
          <span className="font-medium">用途の分類はオフです。</span>
          <span className="text-muted-foreground">
            {" "}
            オンにすると、会話の最初の発話を AI が読み、用途のラベル（文書作成・要約など）だけを保存します。
            使われた機能（文書検索・スキルなど）は分類がオフでも集計されます。
          </span>
          <Link
            href="/admin/ai/insights/settings"
            className="ml-1 font-medium underline underline-offset-4"
          >
            分析の設定
          </Link>
        </p>
      </div>
    );
  }
  const at = u.last_classified_at
    ? new Date(u.last_classified_at).toLocaleString("ja-JP", {
        month: "numeric",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
      })
    : null;
  return (
    <div className="flex gap-3 rounded-lg border bg-card px-4 py-3 text-xs leading-relaxed text-muted-foreground">
      <Lock className="mt-0.5 size-3.5 shrink-0" aria-hidden />
      <p>
        管理者は会話の本文を見られません。用途は AI が付けたラベルだけを保存しています
        {at ? `（最終分類 ${at}）` : "（まだ分類された会話はありません）"}。
      </p>
    </div>
  );
}

function Categories({ u }: { u: InsightsUseCases }) {
  const max = Math.max(0.0001, ...u.categories.map((c) => c.share));
  return (
    <Panel
      title="用途の内訳"
      description={`分類された会話 ${formatInt(u.total_threads)} 件`}
    >
      {u.categories.length === 0 ? (
        <p className="px-4 py-6 text-center text-xs text-muted-foreground">この期間に分類された会話はありません</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[560px] text-[13px]">
            <caption className="sr-only">用途ごとの会話の割合</caption>
            <thead className="whitespace-nowrap text-xs text-muted-foreground">
              <tr className="border-b">
                <th scope="col" className="px-4 py-2 text-left font-normal">用途</th>
                <th scope="col" className="w-[38%] px-2 py-2 text-left font-normal">割合</th>
                <th scope="col" className="px-2 py-2 text-right font-normal">会話</th>
                <th scope="col" className="px-2 py-2 text-right font-normal">利用者</th>
                <th scope="col" className="px-4 py-2 text-right font-normal">前期比</th>
              </tr>
            </thead>
            <tbody>
              {u.categories.map((c) => (
                <tr key={c.key} className="border-b last:border-b-0">
                  <th scope="row" className="px-4 py-2.5 text-left font-normal">{c.label}</th>
                  <td className="px-2 py-2.5">
                    <div className="flex items-center gap-2">
                      <div className="h-1.5 flex-1 rounded-full bg-muted">
                        <div
                          className="h-full rounded-full bg-[var(--chart-1)]"
                          style={{ width: `${Math.max(2, (c.share / max) * 100)}%` }}
                        />
                      </div>
                      <span className="w-10 text-right tabular-nums">{pct(c.share)}</span>
                    </div>
                  </td>
                  <td className="px-2 py-2.5 text-right tabular-nums">{formatInt(c.threads)}</td>
                  <td className="px-2 py-2.5 text-right tabular-nums text-muted-foreground">
                    {c.users == null ? "—" : formatInt(c.users)}
                  </td>
                  <td className="px-4 py-2.5 text-right">
                    {c.share_prev == null ? (
                      <span className="text-xs text-muted-foreground">—</span>
                    ) : (
                      <DeltaPt cur={c.share} prev={c.share_prev} label="" />
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Panel>
  );
}

function Matrix({ u }: { u: InsightsUseCases }) {
  if (u.matrix.length === 0) return null;
  // 行の内訳は値が小さく偏るので、表の最大値で一番濃くする（凡例も合わせる）。
  const max = Math.max(0.01, ...u.matrix.flatMap((r) => r.shares.map((v) => v ?? 0)));
  return (
    <Panel
      title="部署 × 用途"
      description="各部署の会話に占める割合（行で 100%）。「—」は 5 人未満のため伏せています。"
      actions={<HeatLegend max={max} />}
    >
      <div className="overflow-x-auto px-3 pb-2 pt-1">
        <table className="w-full min-w-[720px] table-fixed border-separate border-spacing-0 text-[13px]">
          <caption className="sr-only">部署ごとの用途の割合</caption>
          <thead>
            <tr className="whitespace-nowrap text-xs text-muted-foreground">
              <th scope="col" className="sticky left-0 z-[1] w-36 bg-card px-1 py-2 text-left font-normal">部署</th>
              {u.matrix_labels.map((l) => (
                <th key={l} scope="col" className="px-1 py-2 text-center font-normal">
                  <span className="line-clamp-2 whitespace-normal">{l}</span>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {u.matrix.map((r) => (
              <tr key={r.role_id}>
                <th
                  scope="row"
                  className="sticky left-0 z-[1] max-w-40 truncate bg-card px-1 py-1 text-left font-normal"
                  title={r.label}
                >
                  {r.label}
                </th>
                {r.shares.map((v, i) => (
                  <HeatCell
                    key={u.matrix_columns[i]}
                    value={v}
                    scale={max}
                    title={`${r.label}・${u.matrix_labels[i]}`}
                  />
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <Note>他の部署でよく使われているのに、ある部署では少ない用途は、研修テーマの候補になります。</Note>
    </Panel>
  );
}

function Features({ u }: { u: InsightsUseCases }) {
  const base = Math.max(1, u.active_users);
  return (
    <Panel title="使われている機能" description={`この期間に使った ${formatInt(u.active_users)} 人のうち、その機能を使った人。`}>
      <ul className="flex flex-col gap-3 px-4 py-3.5">
        {u.features.map((f) => (
          <BarRow
            key={f.key}
            label={f.label}
            value={`${formatInt(f.users)} 人`}
            sub={`${pct(f.users / base)}・${formatInt(f.count)} 回`}
            ratio={f.users / base}
          />
        ))}
      </ul>
    </Panel>
  );
}

function Tops({
  title,
  items,
  empty,
}: {
  title: string;
  items: InsightsUseCases["top_skills"];
  empty: string;
}) {
  const max = Math.max(1, ...items.map((i) => i.count));
  return (
    <Panel title={title}>
      {items.length === 0 ? (
        <p className="px-4 py-6 text-center text-xs text-muted-foreground">{empty}</p>
      ) : (
        <ul className="flex flex-col gap-3 px-4 py-3.5">
          {items.map((i) => (
            <BarRow
              key={i.id}
              label={i.name}
              value={`${formatInt(i.count)} 回`}
              sub={i.users == null ? "5 人未満" : `${formatInt(i.users)} 人`}
              ratio={i.count / max}
            />
          ))}
        </ul>
      )}
    </Panel>
  );
}

/// 活用分析 › 用途。
export function InsightsUseCasesView() {
  const filter = useInsightsFilter();
  const state = useInsightsData(fetchUseCases, filter);
  return (
    <AdminPage
      title="用途"
      description={`何に使われているか。会話ごとに AI が付けた用途のラベルと、使われた機能から集計します。${INSIGHTS_FOOTNOTE}`}
      wide
    >
      <InsightsToolbar filter={filter} />
      <InsightsBody state={state} what="用途">
        {(u) => (
          <>
            <ClassifyNotice u={u} />
            {u.classify_enabled ? (
              <>
                <Categories u={u} />
                <Matrix u={u} />
              </>
            ) : null}
            <Features u={u} />
            <div className="grid grid-cols-1 gap-3 lg:grid-cols-3">
              <Tops title="よく使われるスキル" items={u.top_skills} empty="この期間に使われたスキルはありません" />
              <Tops title="よく使われるミニアプリ" items={u.top_miniapps} empty="この期間に使われたミニアプリはありません" />
              <Tops title="よく動いたワークフロー" items={u.top_workflows} empty="この期間に動いたワークフローはありません" />
            </div>
          </>
        )}
      </InsightsBody>
    </AdminPage>
  );
}
