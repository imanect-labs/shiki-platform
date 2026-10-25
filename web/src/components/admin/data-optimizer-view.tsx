"use client";

import * as React from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { AlertCircle, Database, ExternalLink, Gauge } from "lucide-react";

import {
  fetchOptimizerStatus,
  fetchTableOptimizer,
  updateTableOptimizer,
  type OptimizerIndexCandidate,
  type OptimizerStatus,
  type TableOptimizerReport,
  type UnitCosts,
} from "@/lib/admin-data-api";
import { getTable, listTables, type DataTable } from "@/lib/data-api";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/ui/empty-state";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/use-toast";
import { AdminPage, Panel, SettingRow, StatTile } from "./admin-page";

const COSTS: { key: keyof UnitCosts; label: string; hint: string }[] = [
  { key: "c_tuple", label: "索引の走査", hint: "1 エントリ" },
  { key: "c_probe", label: "点検索", hint: "1 回" },
  { key: "c_fetch", label: "本体の取得", hint: "1 行" },
  { key: "c_rtt", label: "DB 往復", hint: "1 回" },
  { key: "c_sort", label: "ソートの係数", hint: "n·log₂L あたり" },
];

const micro = (v: number) => `${v < 1 ? v.toFixed(3) : v < 10 ? v.toFixed(2) : v.toFixed(1)} µs`;
const corr = (v: number | null | undefined) => (v == null ? "—" : v.toFixed(3));

/// 環境の単価と起動時の較正の前後（テナントのデータを含まない）。
function OptimizerStatusPanel() {
  const [status, setStatus] = React.useState<OptimizerStatus | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  React.useEffect(() => {
    fetchOptimizerStatus()
      .then(setStatus)
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  }, []);

  const cal = status?.calibration ?? null;
  return (
    <Panel
      title="計画の単価"
      description="クエリの計画に使う、この環境の DB の処理単価。サーバの起動時に固定の検証表で測り直します（テナントのデータは使いません）。"
    >
      {error ? (
        <p className="flex items-center gap-2 px-4 py-4 text-[13px] text-destructive" role="alert">
          <AlertCircle className="size-4" aria-hidden />
          {error}
        </p>
      ) : !status ? (
        <div className="p-4" aria-busy="true">
          <Skeleton className="h-36" />
        </div>
      ) : (
        <div className="space-y-4 p-4">
          <div className="overflow-x-auto">
            <table className="w-full text-left text-[13px]" data-testid="optimizer-unit-costs">
              <thead className="text-xs text-muted-foreground">
                <tr className="border-b">
                  <th scope="col" className="py-2 pr-3 font-medium">操作</th>
                  {cal ? <th scope="col" className="py-2 pr-3 text-right font-medium">較正前</th> : null}
                  <th scope="col" className="py-2 text-right font-medium">{cal ? "いま（較正後）" : "いま"}</th>
                </tr>
              </thead>
              <tbody>
                {COSTS.map((c) => (
                  <tr key={c.key} className="border-b last:border-b-0">
                    <td className="py-2 pr-3">
                      {c.label}
                      <span className="block text-xs text-muted-foreground sm:ml-1.5 sm:inline">{c.hint}</span>
                    </td>
                    {cal ? (
                      <td className="py-2 pr-3 text-right tabular-nums text-muted-foreground">{micro(cal.before[c.key])}</td>
                    ) : null}
                    <td className="py-2 text-right font-medium tabular-nums">{micro(status.unit_costs[c.key])}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {cal ? (
            <div className="grid gap-3 sm:grid-cols-2">
              <StatTile
                label="予測と実測の相関（対数）"
                value={
                  <span>
                    <span className="text-muted-foreground">{corr(cal.correlation_before)}</span>
                    <span className="mx-1.5 text-base text-muted-foreground">→</span>
                    {corr(cal.correlation_after)}
                  </span>
                }
                sub="1 に近いほど、計画が速い・遅いを正しく見分けている"
              />
              <StatTile
                label="平均のずれ（対数誤差）"
                value={
                  <span>
                    <span className="text-muted-foreground">{cal.log_error_before.toFixed(2)}</span>
                    <span className="mx-1.5 text-base text-muted-foreground">→</span>
                    {cal.log_error_after.toFixed(2)}
                  </span>
                }
                sub="0 で一致・0.69 で平均 2 倍ずれ"
              />
              <p className="text-xs text-muted-foreground sm:col-span-2">
                {new Date(cal.calibrated_at).toLocaleString("ja-JP")} に較正（検証用の操作 {cal.validation_samples} 本 ×{" "}
                {cal.runs} 回）
              </p>
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">
              較正はまだ行われていません（初期値で計画しています）。サーバの再起動時に測り直します。
            </p>
          )}
        </div>
      )}
    </Panel>
  );
}

function candidateLabel(c: OptimizerIndexCandidate): string {
  return c.kind === "composite_index" ? "複合索引" : "索引の昇格";
}

/// テーブル別のクエリ最適化の報告（推定値・統計値は含まない運用の数値と索引の候補だけ）。
function TableReport({ tableId, table }: { tableId: string; table: DataTable | null }) {
  const [report, setReport] = React.useState<TableOptimizerReport | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [saving, setSaving] = React.useState(false);

  React.useEffect(() => {
    let active = true;
    setReport(null);
    setError(null);
    fetchTableOptimizer(tableId)
      .then((r) => active && setReport(r))
      .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      active = false;
    };
  }, [tableId]);

  const labelOf = (key: string) => table?.schema.fields.find((f) => f.key === key)?.display_name || key;

  const toggleFixed = async (v: boolean) => {
    if (!report) return;
    setSaving(true);
    try {
      setReport(await updateTableOptimizer(tableId, v));
      toast({ title: v ? "固定計画に切り替えました" : "自動の計画に戻しました" });
    } catch (e) {
      toast({ variant: "destructive", title: "切り替えられませんでした", description: e instanceof Error ? e.message : String(e) });
    } finally {
      setSaving(false);
    }
  };

  if (error) {
    return (
      <Panel>
        <EmptyState icon={AlertCircle} title="報告を読み込めませんでした" description={error} />
      </Panel>
    );
  }
  if (!report) {
    return (
      <div className="flex flex-col gap-4" aria-busy="true">
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {[0, 1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-24" />
          ))}
        </div>
        <Skeleton className="h-40" />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4" data-testid="optimizer-table-report">
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <StatTile label="読取" value={report.requests.toLocaleString("ja-JP")} sub={`うち重すぎて断った ${report.rejected_requests.toLocaleString("ja-JP")}`} />
        <StatTile label="走査効率 p95" value={report.requests ? `${report.scan_ratio_p95.toFixed(1)} 倍` : "—"} sub="読んだ索引 ÷ 返した行（1 に近いほど無駄が少ない）" />
        <StatTile label="計画の切替率" value={report.requests ? `${(report.switch_rate * 100).toFixed(1)}%` : "—"} sub="見込みが外れて途中で計画を変えた割合" />
        <StatTile
          label="所要時間の見込みと実測"
          value={corr(report.timing_correlation)}
          sub={report.timing_correlation == null ? `組が 3 つ未満（${report.timing_samples} 組）` : `相関・${report.timing_samples} 組`}
        />
      </div>
      <p className="-mt-1 text-xs text-muted-foreground">
        数値はこのサーバのプロセスが起動してから観測した分です（再起動で 0 に戻ります）。値や推定値は含みません。
      </p>

      <Panel title="計画">
        <SettingRow
          label="固定計画で読む"
          htmlFor="optimizer-fixed-plan"
          description="統計に依らない決まった計画で読みます。応答時間から見えない行の分布を推測されたくない、機微なテーブル向けです。読取は遅くなることがあります。変更は監査ログに残ります。"
        >
          <Switch
            id="optimizer-fixed-plan"
            checked={report.fixed_plan}
            disabled={saving}
            onCheckedChange={(v) => void toggleFixed(v)}
          />
        </SettingRow>
      </Panel>

      <Panel
        title="索引の候補"
        description="並べ替え・絞り込みで索引が無く遅かった列と、よく一緒に使われる並べ替えの組み合わせ。テーブルの管理者が「列の編集」で索引を有効にできます。"
      >
        {report.candidates.length === 0 ? (
          <p className="px-4 py-6 text-center text-[13px] text-muted-foreground">候補はありません</p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[28rem] text-left text-[13px]" data-testid="optimizer-candidates">
              <thead className="text-xs text-muted-foreground">
                <tr className="border-b">
                  <th scope="col" className="px-4 py-2 font-medium">種類</th>
                  <th scope="col" className="px-4 py-2 font-medium">列</th>
                  <th scope="col" className="px-4 py-2 text-right font-medium">回数</th>
                </tr>
              </thead>
              <tbody>
                {report.candidates.map((c, i) => (
                  <tr key={i} className="border-b last:border-b-0">
                    <td className="px-4 py-2">
                      <Badge variant={c.kind === "composite_index" ? "secondary" : "outline"}>{candidateLabel(c)}</Badge>
                    </td>
                    <td className="px-4 py-2">{c.fields.map(labelOf).join(" → ")}</td>
                    <td className="px-4 py-2 text-right tabular-nums">{c.occurrences.toLocaleString("ja-JP")}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Panel>
    </div>
  );
}

/// データ › クエリ最適化（環境の単価・テーブル別の報告・固定計画・索引の候補）。
export function DataOptimizerView() {
  const router = useRouter();
  const params = useSearchParams();
  const tableId = params.get("table");
  const [tables, setTables] = React.useState<DataTable[] | null>(null);
  const [current, setCurrent] = React.useState<DataTable | null>(null);

  React.useEffect(() => {
    listTables()
      .then(setTables)
      .catch(() => setTables([]));
  }, []);
  React.useEffect(() => {
    if (!tableId) return setCurrent(null);
    const hit = tables?.find((t) => t.id === tableId);
    if (hit) return setCurrent(hit);
    // 一覧に無い（管理者本人が見られないテーブル）ときも ID で報告は開ける。列名は参照名のまま出す。
    let active = true;
    getTable(tableId)
      .then((t) => active && setCurrent(t))
      .catch(() => active && setCurrent(null));
    return () => {
      active = false;
    };
  }, [tableId, tables]);

  const open = (id: string) => router.replace(`/admin/data/optimizer?table=${encodeURIComponent(id)}`);

  return (
    <AdminPage
      title="クエリ最適化"
      description="業務データのテーブルの読取がどう計画されているか。推定値や統計値、テーブルの値は表示しません。"
      wide
    >
      <OptimizerStatusPanel />
      <Panel
        title="テーブル別"
        actions={
          tableId ? (
            <Link
              href={`/data/${tableId}`}
              className="inline-flex items-center gap-1 rounded text-xs font-medium text-primary hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              テーブルを開く
              <ExternalLink className="size-3.5" aria-hidden />
            </Link>
          ) : null
        }
      >
        <div className="flex flex-wrap items-center gap-2 px-4 py-3">
          <Gauge className="size-4 text-muted-foreground" aria-hidden />
          {tables === null ? (
            <Skeleton className="h-9 w-72" />
          ) : tables.length === 0 ? (
            <p className="text-[13px] text-muted-foreground">見られるテーブルがありません</p>
          ) : (
            <Select value={tableId ?? undefined} onValueChange={open}>
              <SelectTrigger aria-label="テーブル" className={cn("h-9 w-full sm:w-80", !tableId && "text-muted-foreground")} data-testid="optimizer-table-select">
                <SelectValue placeholder="テーブルを選ぶ" />
              </SelectTrigger>
              <SelectContent>
                {tables.map((t) => (
                  <SelectItem key={t.id} value={t.id}>
                    {t.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </div>
      </Panel>
      {tableId ? (
        <TableReport tableId={tableId} table={current} />
      ) : (
        <Panel>
          <EmptyState icon={Database} title="テーブルを選んでください" description="読取の数・走査効率・索引の候補を表示します。" />
        </Panel>
      )}
    </AdminPage>
  );
}
