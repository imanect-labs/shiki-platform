"use client";

import * as React from "react";
import { AlertCircle } from "lucide-react";

import {
  fetchDataSettings,
  updateDataSettings,
  type DataTenantSettings,
  type UpdateDataSettingsRequest,
} from "@/lib/admin-data-api";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/use-toast";
import { AdminPage, Panel, SettingRow } from "./admin-page";

/// 画面とサーバで同じ範囲（`crates/data` の `MAX_*`）。サーバが正で、ここは早めに気付かせるため。
const LIMITS = {
  max_indexed_fields: { min: 1, max: 1_000 },
  max_view_lookup_columns: { min: 1, max: 100 },
  count_cap: { min: 1, max: 10_000_000 },
} as const;

type Key = keyof typeof LIMITS;
/// 入力欄の文字列（空欄 = 既定）。
type Draft = Record<Key, string>;

function draftOf(s: DataTenantSettings): Draft {
  const str = (v: number | null | undefined) => (v != null ? String(v) : "");
  return {
    max_indexed_fields: str(s.max_indexed_fields),
    max_view_lookup_columns: str(s.max_view_lookup_columns),
    count_cap: str(s.count_cap),
  };
}

const num = (v: string): number | null => (v.trim() === "" ? null : Number(v.trim().replace(/,/g, "")));

/// 変更点だけの更新リクエスト。`count_cap` は省略すると既定へ戻る契約なので常に送る。
export function diffDataSettings(base: Draft, next: Draft): UpdateDataSettingsRequest | null {
  const changed = (Object.keys(LIMITS) as Key[]).filter((k) => next[k].trim() !== base[k].trim());
  if (changed.length === 0) return null;
  const req: UpdateDataSettingsRequest = { count_cap: num(next.count_cap) };
  if (changed.includes("max_indexed_fields")) req.max_indexed_fields = num(next.max_indexed_fields);
  if (changed.includes("max_view_lookup_columns")) req.max_view_lookup_columns = num(next.max_view_lookup_columns);
  return req;
}

export function validateDataSettings(d: Draft): string | null {
  const labels: Record<Key, string> = {
    max_indexed_fields: "索引列の上限",
    max_view_lookup_columns: "ルックアップ・ロールアップ列の上限",
    count_cap: "件数の上限",
  };
  for (const k of Object.keys(LIMITS) as Key[]) {
    const v = d[k].trim().replace(/,/g, "");
    if (v === "") continue;
    const n = /^\d+$/.test(v) ? Number(v) : NaN;
    const { min, max } = LIMITS[k];
    if (!(n >= min && n <= max))
      return `${labels[k]}は ${min.toLocaleString("ja-JP")}〜${max.toLocaleString("ja-JP")} の整数で入力してください（空欄で既定）`;
  }
  return null;
}

function seconds(ms: number): string {
  const s = ms / 1000;
  return Number.isInteger(s) ? `${s} 秒` : `${s.toFixed(1)} 秒`;
}

/// データ › 設定（テナントの構造化データの上限と、権限の反映の約束）。
export function DataSettingsView() {
  const [settings, setSettings] = React.useState<DataTenantSettings | null>(null);
  const [base, setBase] = React.useState<Draft | null>(null);
  const [draft, setDraft] = React.useState<Draft | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [saving, setSaving] = React.useState(false);

  const apply = (s: DataTenantSettings) => {
    const d = draftOf(s);
    setSettings(s);
    setBase(d);
    setDraft(d);
  };

  React.useEffect(() => {
    fetchDataSettings()
      .then(apply)
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  }, []);

  if (error) {
    return (
      <AdminPage title="データの設定">
        <EmptyState icon={AlertCircle} title="設定を読み込めませんでした" description={error} />
      </AdminPage>
    );
  }
  if (!settings || !base || !draft) {
    return (
      <AdminPage title="データの設定">
        <div className="flex flex-col gap-4" aria-busy="true">
          <Skeleton className="h-64" />
          <Skeleton className="h-28" />
        </div>
      </AdminPage>
    );
  }

  const set = (patch: Partial<Draft>) => setDraft({ ...draft, ...patch });
  const req = diffDataSettings(base, draft);
  const problem = validateDataSettings(draft);

  const save = async () => {
    if (problem || !req) return;
    setSaving(true);
    try {
      apply(await updateDataSettings(req));
      toast({ title: "データの設定を保存しました" });
    } catch (e) {
      toast({
        title: "保存できませんでした",
        description: e instanceof Error ? e.message : String(e),
        variant: "destructive",
      });
    } finally {
      setSaving(false);
    }
  };

  const field = (k: Key, id: string, effective: number | undefined, unit: string) => (
    <div className="flex items-center gap-2">
      <Input
        id={id}
        inputMode="numeric"
        placeholder={effective != null ? `既定（${effective.toLocaleString("ja-JP")}）` : "既定"}
        value={draft[k]}
        onChange={(e) => set({ [k]: e.target.value } as Partial<Draft>)}
        className={cn("h-9 w-40 text-right tabular-nums", draft[k] !== base[k] && "font-medium")}
      />
      <span className="text-xs text-muted-foreground">{unit}</span>
    </div>
  );

  return (
    <AdminPage
      title="データの設定"
      description="業務データのテーブル（/data）の上限。テナント全体に効き、変更は監査ログに残ります。空欄は既定値です。"
    >
      <Panel title="上限" description="大きくすると重いクエリや書込の増幅を許すことになります。必要な分だけ上げてください。">
        <SettingRow
          label="索引列の上限（テーブルあたり）"
          htmlFor="data-max-indexed"
          description={`並べ替え・絞り込みに使える列（索引）の数。超える列で並べ替えようとすると、テーブルの管理者の承認が要ります。いまは ${settings.effective_max_indexed_fields ?? "既定の"} 列。`}
        >
          {field("max_indexed_fields", "data-max-indexed", settings.effective_max_indexed_fields, "列")}
        </SettingRow>
        <SettingRow
          label="ルックアップ・ロールアップ列の上限（ビューあたり）"
          htmlFor="data-max-lookup"
          description={`1 つのビューで表示できる、他のテーブルを参照する列の数。超えるビューは保存できません。いまは ${settings.effective_max_view_lookup_columns ?? "既定の"} 列。`}
        >
          {field("max_view_lookup_columns", "data-max-lookup", settings.effective_max_view_lookup_columns, "列")}
        </SettingRow>
        <SettingRow
          label="件数の上限"
          htmlFor="data-count-cap"
          description={`表の「◯件」を正確に数える上限。超えると「${settings.effective_count_cap.toLocaleString("ja-JP")} 件以上」と出します。大きくすると件数の取得が遅くなります。`}
        >
          {field("count_cap", "data-count-cap", settings.effective_count_cap, "件")}
        </SettingRow>
      </Panel>

      <Panel title="権限の変更の反映" description="サーバの設定で決まる値です（この画面では変えられません）。">
        <SettingRow
          label="変更の自動反映への権限の反映"
          description="表を開いている人の画面へ他の人の変更を流すとき、共有を外した・行の権限を変えたことが反映されるまでの最大の遅れ。表を開き直す・読み直すときは遅れません。"
        >
          <p className="text-sm">
            最大{" "}
            <span className="font-semibold tabular-nums" data-testid="data-revocation-delay">
              {settings.stream_revocation_delay_ms != null ? seconds(settings.stream_revocation_delay_ms) : "—"}
            </span>
          </p>
        </SettingRow>
      </Panel>

      {req ? (
        <div className="sticky bottom-4 z-10 flex flex-wrap items-center justify-between gap-3 rounded-lg border bg-card px-4 py-3 shadow-md max-md:bottom-[max(0.75rem,env(safe-area-inset-bottom))]">
          <p className={cn("text-[13px]", problem ? "text-destructive" : "text-muted-foreground")} role={problem ? "alert" : undefined}>
            {problem ?? "保存していない変更があります"}
          </p>
          <div className="flex gap-2">
            <Button variant="outline" size="sm" onClick={() => setDraft(base)} disabled={saving}>
              取り消す
            </Button>
            <Button size="sm" onClick={() => void save()} loading={saving} disabled={!!problem} data-testid="data-settings-save">
              変更を保存
            </Button>
          </div>
        </div>
      ) : null}
    </AdminPage>
  );
}
