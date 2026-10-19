"use client";

import * as React from "react";
import Link from "next/link";
import { EyeOff, ShieldCheck } from "lucide-react";

import {
  fetchAdoption,
  fetchPeople,
  type InsightsAdoption,
  type InsightsPerson,
  type InsightsQuery,
} from "@/lib/insights-api";
import { Button } from "@/components/ui/button";
import { AdminPage, Panel } from "../admin-page";
import { addDays, formatInt } from "../format";
import {
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

const md = (iso: string) => {
  const [, m, d] = iso.split("-").map(Number);
  return `${m}/${d}`;
};

/// 部署 × 週の利用率。
function DeptHeatmap({ a, to }: { a: InsightsAdoption; to: string }) {
  const unfinished = (w: string) => addDays(w, 6) > to;
  if (a.departments.length === 0) {
    return (
      <p className="px-4 py-6 text-center text-xs text-muted-foreground">
        部署の情報がありません（ログイン時の所属グループから作られます）
      </p>
    );
  }
  return (
    <div className="overflow-x-auto px-3 pb-2 pt-1">
      <table className="w-full min-w-[640px] border-separate border-spacing-0 text-[13px]">
        <caption className="sr-only">部署ごとの週別の利用率</caption>
        <thead>
          <tr className="whitespace-nowrap text-xs text-muted-foreground">
            <th scope="col" className="sticky left-0 z-[1] bg-card px-1 py-2 text-left font-normal">
              部署
            </th>
            <th scope="col" className="px-1 py-2 text-right font-normal">
              人数
            </th>
            {a.weeks.map((w) => (
              <th key={w} scope="col" className="px-1 py-2 text-center font-normal tabular-nums">
                {md(w)}
                {unfinished(w) ? "*" : ""}
              </th>
            ))}
            <th scope="col" className="px-2 py-2 text-right font-normal">
              定着率
            </th>
            <th scope="col" className="px-2 py-2 text-right font-normal">
              会話/人・週
            </th>
            <th scope="col" className="px-2 py-2 text-left font-normal">
              主な用途
            </th>
          </tr>
        </thead>
        <tbody>
          {a.departments.map((d) => (
            <tr key={d.role_id}>
              <th
                scope="row"
                className="sticky left-0 z-[1] max-w-40 truncate bg-card px-1 py-1 text-left font-normal"
                title={d.label}
              >
                {d.label}
              </th>
              <td className="px-1 py-1 text-right tabular-nums text-muted-foreground">
                {formatInt(d.members)}
              </td>
              {d.hidden
                ? a.weeks.map((w) => <HeatCell key={w} value={null} title={`${d.label} ${md(w)}`} />)
                : d.weekly_rates.map((r, i) => (
                    <HeatCell key={a.weeks[i]} value={r} title={`${d.label} ${md(a.weeks[i])} の週`} />
                  ))}
              <td className="px-2 py-1 text-right tabular-nums">{pct(d.stickiness)}</td>
              <td className="px-2 py-1 text-right tabular-nums">
                {d.runs_per_user_week == null ? "—" : d.runs_per_user_week.toFixed(1)}
              </td>
              <td className="max-w-36 truncate px-2 py-1 text-muted-foreground" title={d.top_use ?? ""}>
                {d.top_use ?? "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {a.weeks.some(unfinished) ? (
        <p className="px-1 pt-2 text-xs text-muted-foreground">* 期間の終わりで途中の週（まだ日が残っているため低めに出ます）</p>
      ) : null}
    </div>
  );
}

/// 利用区分（期間の終わり基準・直近 28 日）。
function SegmentsPanel({ s }: { s: InsightsAdoption["segments"] }) {
  const rows = [
    { key: "heavy", label: "ヘビー", def: "28 日中 16 日以上", n: s.heavy, tone: "var(--primary)" },
    { key: "regular", label: "レギュラー", def: "4〜15 日", n: s.regular, tone: "var(--ordinal-2)" },
    { key: "light", label: "ライト", def: "1〜3 日", n: s.light, tone: "var(--ordinal-3)" },
    { key: "dormant", label: "休眠", def: "以前は使っていた", n: s.dormant, tone: "var(--muted)" },
    { key: "never", label: "未利用", def: "一度も使っていない", n: s.never, tone: "var(--muted)" },
  ];
  const total = Math.max(1, rows.reduce((n, r) => n + r.n, 0));
  return (
    <Panel title="利用区分" description="直近 28 日に使った日数で分けています（期間の終わり基準）。">
      <div className="px-4 pt-4">
        <div className="flex h-3 w-full gap-[2px] overflow-hidden rounded-full" aria-hidden>
          {rows
            .filter((r) => r.n > 0)
            .map((r) => (
              <div
                key={r.key}
                style={{ width: `${(r.n / total) * 100}%`, backgroundColor: r.tone }}
              />
            ))}
        </div>
      </div>
      <table className="mt-2 w-full text-[13px]">
        <caption className="sr-only">利用区分ごとの人数</caption>
        <tbody>
          {rows.map((r) => (
            <tr key={r.key} className="border-t first:border-t-0">
              <th scope="row" className="py-2.5 pl-4 text-left font-normal">
                <span className="inline-flex items-center gap-2">
                  <span
                    className="size-2.5 rounded-[3px]"
                    style={{ backgroundColor: r.tone }}
                    aria-hidden
                  />
                  {r.label}
                </span>
              </th>
              <td className="py-2.5 text-xs text-muted-foreground">{r.def}</td>
              <td className="py-2.5 text-right tabular-nums">{formatInt(r.n)} 人</td>
              <td className="w-16 py-2.5 pr-4 text-right text-xs tabular-nums text-muted-foreground">
                {pct(r.n / total)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </Panel>
  );
}

/// 初めて使った月ごとの継続利用。
function Cohorts({ cohorts, to }: { cohorts: InsightsAdoption["cohorts"]; to: string }) {
  const width = Math.max(0, ...cohorts.map((c) => c.retention.length));
  // 期間の終わりより先の月（まだ来ていない）は空欄にする（伏せた値の「—」と区別する）。
  const monthIndex = (ym: string) => Number(ym.slice(0, 4)) * 12 + Number(ym.slice(5, 7));
  const future = (month: string, k: number) => monthIndex(month) + k > monthIndex(to);
  return (
    <Panel
      title="継続利用"
      description="初めて使った月ごとに、翌月以降も使い続けている人の割合。"
    >
      {cohorts.length === 0 ? (
        <p className="px-4 py-6 text-center text-xs text-muted-foreground">まだデータがありません</p>
      ) : (
        <div className="overflow-x-auto px-3 pb-1 pt-1">
          <table className="w-full min-w-[420px] border-separate border-spacing-0 text-[13px]">
            <caption className="sr-only">初回利用月ごとの継続率</caption>
            <thead>
              <tr className="whitespace-nowrap text-xs text-muted-foreground">
                <th scope="col" className="px-1 py-2 text-left font-normal">
                  初回利用
                </th>
                <th scope="col" className="px-1 py-2 text-right font-normal">
                  人数
                </th>
                {Array.from({ length: width }, (_, i) => (
                  <th key={i} scope="col" className="px-1 py-2 text-center font-normal">
                    {i + 1} か月後
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {cohorts.map((c) => (
                <tr key={c.month}>
                  <th scope="row" className="whitespace-nowrap px-1 py-1 text-left font-normal tabular-nums">
                    {c.month.slice(0, 7)}
                  </th>
                  <td className="px-1 py-1 text-right tabular-nums text-muted-foreground">
                    {formatInt(c.users)}
                  </td>
                  {Array.from({ length: width }, (_, i) =>
                    i < c.retention.length && !future(c.month, i + 1) ? (
                      <HeatCell key={i} value={c.retention[i]} title={`${c.month.slice(0, 7)} の ${i + 1} か月後`} />
                    ) : (
                      <td key={i} />
                    ),
                  )}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <Note>研修やスキル配布の前後で、使い続ける人の割合がどう変わったかを見るのに使えます。</Note>
    </Panel>
  );
}

/// 個人別（組織の設定でオンのときだけ・開くと監査ログに残る）。
function People({ enabled, query }: { enabled: boolean; query: InsightsQuery }) {
  const [rows, setRows] = React.useState<InsightsPerson[] | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  // 絞り込みが変わったら閉じる（開き直し＝もう一度記録される）。
  React.useEffect(() => setRows(null), [query]);

  if (!enabled) {
    return (
      <Panel title="個人別">
        <div className="flex gap-3 px-4 py-4">
          <EyeOff className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
          <div className="text-[13px] leading-relaxed">
            <p className="font-medium">個人単位の表示はオフです</p>
            <p className="mt-0.5 text-muted-foreground">
              オンにすると、人ごとの利用日数・会話数・使った機能が見られます（会話の本文は見られません）。開いた記録は監査ログに残ります。従業員への周知を済ませてから
              <Link
                href="/admin/ai/insights/settings"
                className="mx-0.5 font-medium text-foreground underline underline-offset-4"
              >
                分析の設定
              </Link>
              で有効にしてください。
            </p>
          </div>
        </div>
      </Panel>
    );
  }

  const open = async () => {
    setLoading(true);
    try {
      setRows(await fetchPeople(query));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <Panel
      title="個人別"
      description="会話の本文は含みません。表示すると、誰がいつ見たかが監査ログに残ります。"
      actions={
        rows ? null : (
          <Button size="sm" variant="outline" onClick={() => void open()} loading={loading}>
            <ShieldCheck aria-hidden />
            表示する
          </Button>
        )
      }
    >
      {error ? (
        <p role="alert" className="px-4 py-3 text-[13px] text-destructive">
          {error}
        </p>
      ) : null}
      {rows == null ? (
        <p className="px-4 py-5 text-xs text-muted-foreground">
          「表示する」を押すと、この期間に使った人の一覧を出します。
        </p>
      ) : rows.length === 0 ? (
        <p className="px-4 py-6 text-center text-xs text-muted-foreground">この期間に使った人はいません</p>
      ) : (
        <div className="max-h-[480px] overflow-auto">
          <table className="w-full min-w-[720px] text-[13px]">
            <caption className="sr-only">個人別の利用</caption>
            <thead className="sticky top-0 whitespace-nowrap bg-card text-xs text-muted-foreground">
              <tr className="border-b">
                <th scope="col" className="px-4 py-2 text-left font-normal">名前</th>
                <th scope="col" className="px-2 py-2 text-right font-normal">利用日数</th>
                <th scope="col" className="px-2 py-2 text-right font-normal">発話</th>
                <th scope="col" className="px-2 py-2 text-right font-normal">会話</th>
                <th scope="col" className="px-2 py-2 text-right font-normal">エージェント</th>
                <th scope="col" className="px-2 py-2 text-right font-normal">検索</th>
                <th scope="col" className="px-2 py-2 text-right font-normal">スキル</th>
                <th scope="col" className="px-2 py-2 text-left font-normal">主な用途</th>
                <th scope="col" className="px-4 py-2 text-right font-normal">最終利用</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((p) => (
                <tr key={p.user_id} className="border-b last:border-b-0">
                  <th scope="row" className="max-w-56 px-4 py-2 text-left font-normal">
                    <div className="truncate">{p.name}</div>
                    <div className="truncate text-xs text-muted-foreground">{p.email}</div>
                  </th>
                  <td className="px-2 py-2 text-right tabular-nums">{formatInt(p.active_days)}</td>
                  <td className="px-2 py-2 text-right tabular-nums">{formatInt(p.runs)}</td>
                  <td className="px-2 py-2 text-right tabular-nums">{formatInt(p.threads)}</td>
                  <td className="px-2 py-2 text-right tabular-nums">{formatInt(p.agent_runs)}</td>
                  <td className="px-2 py-2 text-right tabular-nums">{formatInt(p.searches)}</td>
                  <td className="px-2 py-2 text-right tabular-nums">{formatInt(p.skill_uses)}</td>
                  <td className="max-w-36 truncate px-2 py-2 text-muted-foreground">{p.top_use ?? "—"}</td>
                  <td className="px-4 py-2 text-right tabular-nums text-muted-foreground">
                    {md(p.last_active)}
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

/// 活用分析 › 利用者と部署。
export function InsightsAdoptionView() {
  const filter = useInsightsFilter();
  const state = useInsightsData(fetchAdoption, filter);
  return (
    <AdminPage
      title="利用者と部署"
      description={`誰にどれだけ広がっているか。部署はディレクトリのロール（ログイン時の所属グループ）。${INSIGHTS_FOOTNOTE}`}
      wide
    >
      <InsightsToolbar filter={filter} />
      <InsightsBody state={state} what="利用者と部署">
        {(a) => (
          <>
            <Panel
              title="部署 × 週の利用率"
              description="その週に一度でも使った人の割合。「—」は 5 人未満のため伏せています。"
              actions={<HeatLegend />}
            >
              <DeptHeatmap a={a} to={filter.query.to} />
            </Panel>
            <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
              <SegmentsPanel s={a.segments} />
              <Cohorts cohorts={a.cohorts} to={filter.query.to} />
            </div>
            <People enabled={a.individual_view} query={filter.query} />
          </>
        )}
      </InsightsBody>
    </AdminPage>
  );
}
