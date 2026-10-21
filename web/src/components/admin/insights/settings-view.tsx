"use client";

import * as React from "react";
import { AlertCircle } from "lucide-react";

import {
  fetchInsightSettings,
  updateInsightSettings,
  type InsightSettings,
  type UpdateInsightSettings,
} from "@/lib/insights-api";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/use-toast";
import { AdminPage, Panel, SettingRow } from "../admin-page";

/// 編集中の設定（数値は入力欄の文字列のまま持つ）。
type Draft = {
  classify_enabled: boolean;
  individual_view: boolean;
  wage: string;
  minutes: Record<string, string>;
  workflow: string;
};

function draftOf(s: InsightSettings): Draft {
  return {
    classify_enabled: s.classify_enabled,
    individual_view: s.individual_view,
    wage: s.hourly_wage_jpy != null ? String(s.hourly_wage_jpy) : "",
    minutes: Object.fromEntries(s.minutes.map((m) => [m.key, String(m.minutes)])),
    workflow: String(s.workflow_minutes),
  };
}

/// 変更点だけの更新リクエスト。
export function diffInsightSettings(base: Draft, next: Draft): UpdateInsightSettings {
  const req: UpdateInsightSettings = {};
  if (next.classify_enabled !== base.classify_enabled) req.classify_enabled = next.classify_enabled;
  if (next.individual_view !== base.individual_view) req.individual_view = next.individual_view;
  if (next.wage.trim() !== base.wage.trim())
    req.hourly_wage_jpy = next.wage.trim() === "" ? null : Number(next.wage);
  const minutes = Object.keys(next.minutes)
    .filter((k) => next.minutes[k].trim() !== base.minutes[k]?.trim())
    .map((k) => ({ key: k, minutes: Number(next.minutes[k]) }));
  if (minutes.length) req.minutes = minutes;
  if (next.workflow.trim() !== base.workflow.trim()) req.workflow_minutes = Number(next.workflow);
  return req;
}

/// 画面上の検証（サーバも同じ範囲で検証する）。
export function validateInsightSettings(d: Draft): string | null {
  const int = (v: string) => /^\d+$/.test(v.trim()) ? Number(v) : NaN;
  if (d.wage.trim() !== "") {
    const n = int(d.wage);
    if (!(n >= 1 && n <= 100_000)) return "時給は 1〜100,000 円の整数で入力してください";
  }
  for (const v of Object.values(d.minutes)) {
    const n = int(v);
    if (!(n >= 0 && n <= 600)) return "用途ごとの時間は 0〜600 分の整数で入力してください";
  }
  const w = int(d.workflow);
  if (!(w >= 0 && w <= 600)) return "ワークフローの時間は 0〜600 分の整数で入力してください";
  return null;
}

/// 活用分析 › 設定（プライバシーと効果の前提）。
export function InsightsSettingsView() {
  const [settings, setSettings] = React.useState<InsightSettings | null>(null);
  const [base, setBase] = React.useState<Draft | null>(null);
  const [draft, setDraft] = React.useState<Draft | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [saving, setSaving] = React.useState(false);

  const apply = (s: InsightSettings) => {
    const d = draftOf(s);
    setSettings(s);
    setBase(d);
    setDraft(d);
  };

  React.useEffect(() => {
    fetchInsightSettings()
      .then(apply)
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  }, []);

  if (error) {
    return (
      <AdminPage title="分析の設定">
        <EmptyState icon={AlertCircle} title="設定を読み込めませんでした" description={error} />
      </AdminPage>
    );
  }
  if (!settings || !base || !draft) {
    return (
      <AdminPage title="分析の設定">
        <div className="flex flex-col gap-4" aria-busy="true">
          <Skeleton className="h-44" />
          <Skeleton className="h-72" />
        </div>
      </AdminPage>
    );
  }

  const set = (patch: Partial<Draft>) => setDraft({ ...draft, ...patch });
  const req = diffInsightSettings(base, draft);
  const dirty = Object.keys(req).length > 0;
  const problem = validateInsightSettings(draft);

  const save = async () => {
    if (problem) return;
    setSaving(true);
    try {
      apply(await updateInsightSettings(req));
      toast({ title: "分析の設定を保存しました" });
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

  return (
    <AdminPage
      title="分析の設定"
      description="活用分析で集める情報と、効果を見積もるときの前提。変更は監査ログに残ります。"
    >
      <Panel title="プライバシー" description="どちらも既定はオフです。オンにする前に従業員へ周知してください。">
        <SettingRow
          label="会話の用途を分類する"
          htmlFor="insights-classify"
          description="会話の最初の発話（最大 3 件・2,000 文字）を AI が読み、用途のラベルだけを保存します。本文は保存せず、管理者も読めません。分類に使った AI の利用料は利用量に計上されます。"
        >
          <Switch
            id="insights-classify"
            checked={draft.classify_enabled}
            onCheckedChange={(v) => set({ classify_enabled: v })}
          />
        </SettingRow>
        <SettingRow
          label="個人単位の表示"
          htmlFor="insights-individual"
          description="人ごとの利用日数・会話数・使った機能を見られるようにします（本文は見られません）。一覧を開くたびに、誰がいつ見たかが監査ログに残ります。"
        >
          <Switch
            id="insights-individual"
            checked={draft.individual_view}
            onCheckedChange={(v) => set({ individual_view: v })}
          />
        </SettingRow>
      </Panel>

      <Panel
        title="効果の前提"
        description="推定削減時間 = 完了した会話 × 人がやると何分か。組織の実態に合わせて調整してください。"
      >
        <SettingRow
          label="時給（円）"
          htmlFor="insights-wage"
          description="削減時間を金額に換算するときの時給。空欄なら金額は出しません。"
        >
          <Input
            id="insights-wage"
            inputMode="numeric"
            placeholder="例: 3000"
            value={draft.wage}
            onChange={(e) => set({ wage: e.target.value })}
            className="h-9 w-40"
          />
        </SettingRow>
        <SettingRow
          label="人がやると何分か（用途ごと・1 件あたり）"
          description="分類がオンのときに使います。初期値は一般的な目安です。0 にするとその用途は数えません。"
        >
          <div className="grid w-full grid-cols-1 gap-x-4 gap-y-2 sm:grid-cols-2">
            {settings.minutes.map((m) => {
              const changed = draft.minutes[m.key] !== String(m.default_minutes);
              return (
                <label key={m.key} className="flex items-center justify-between gap-2 text-[13px]">
                  <span className="min-w-0 truncate">{m.label}</span>
                  <span className="flex shrink-0 items-center gap-1.5">
                    <Input
                      inputMode="numeric"
                      aria-label={`${m.label}（分）`}
                      value={draft.minutes[m.key] ?? ""}
                      onChange={(e) =>
                        set({ minutes: { ...draft.minutes, [m.key]: e.target.value } })
                      }
                      className={cn("h-8 w-16 text-right tabular-nums", changed && "font-medium")}
                    />
                    <span className="w-4 text-xs text-muted-foreground">分</span>
                  </span>
                </label>
              );
            })}
          </div>
        </SettingRow>
        <SettingRow
          label="ワークフロー 1 回で浮く時間（分）"
          htmlFor="insights-workflow"
          description="成功したワークフローの実行 1 回あたり。0 にすると数えません。"
        >
          <Input
            id="insights-workflow"
            inputMode="numeric"
            value={draft.workflow}
            onChange={(e) => set({ workflow: e.target.value })}
            className="h-9 w-24 text-right tabular-nums"
          />
        </SettingRow>
      </Panel>

      {dirty ? (
        <div className="sticky bottom-4 z-10 flex flex-wrap items-center justify-between gap-3 rounded-lg border bg-card px-4 py-3 shadow-md max-md:bottom-[max(0.75rem,env(safe-area-inset-bottom))]">
          <p className={cn("text-[13px]", problem ? "text-destructive" : "text-muted-foreground")}>
            {problem ?? "保存していない変更があります"}
          </p>
          <div className="flex gap-2">
            <Button variant="outline" size="sm" onClick={() => setDraft(base)} disabled={saving}>
              取り消す
            </Button>
            <Button size="sm" onClick={() => void save()} loading={saving} disabled={!!problem}>
              変更を保存
            </Button>
          </div>
        </div>
      ) : null}
    </AdminPage>
  );
}
