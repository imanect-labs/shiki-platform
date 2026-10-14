"use client";

import * as React from "react";
import { AlertCircle, Globe2, Sparkles } from "lucide-react";

import {
  fetchAdminModels,
  updateAiSettings,
  type AdminModel,
  type AiSettings,
  type EffortLevel,
  type UpdateAiSettingsRequest,
} from "@/lib/admin-ai-api";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/use-toast";
import { AdminPage, Panel, SettingRow } from "./admin-page";
import { EFFORT_LABEL, formatPrice } from "./format";

const AUTO = "auto";
const INHERIT = "__inherit__";

const EFFORT_OPTIONS = [
  { value: "low", label: EFFORT_LABEL.low },
  { value: "medium", label: EFFORT_LABEL.medium },
  { value: "high", label: EFFORT_LABEL.high },
];

/// 画面上で編集中の設定（保存前）。
type Draft = {
  enabled: Record<string, boolean>;
  default_model: string;
  allow_user_choice: boolean;
  default_effort: EffortLevel;
  auto_model: string | null;
  auto_effort: EffortLevel | null;
  /// 入力欄の文字列（空は「円を出さない」）。
  jpy: string;
};

function draftOf(models: AdminModel[], s: AiSettings): Draft {
  return {
    enabled: Object.fromEntries(models.map((m) => [m.id, m.enabled])),
    default_model: s.default_model,
    allow_user_choice: s.allow_user_choice,
    default_effort: s.default_effort,
    auto_model: s.auto_model ?? null,
    auto_effort: s.auto_effort ?? null,
    jpy: s.jpy_per_usd != null ? String(s.jpy_per_usd) : "",
  };
}

/// 変更点だけの更新リクエスト（変えていない項目は送らない）。
export function diffRequest(base: Draft, next: Draft): UpdateAiSettingsRequest {
  const req: UpdateAiSettingsRequest = {};
  const models = Object.keys(next.enabled)
    .filter((id) => next.enabled[id] !== base.enabled[id])
    .map((id) => ({ id, enabled: next.enabled[id] }));
  if (models.length) req.models = models;
  if (next.default_model !== base.default_model) req.default_model = next.default_model;
  if (next.allow_user_choice !== base.allow_user_choice)
    req.allow_user_choice = next.allow_user_choice;
  if (next.default_effort !== base.default_effort) req.default_effort = next.default_effort;
  if (next.auto_model !== base.auto_model) req.auto_model = next.auto_model;
  if (next.auto_effort !== base.auto_effort) req.auto_effort = next.auto_effort;
  if (next.jpy.trim() !== base.jpy.trim())
    req.jpy_per_usd = next.jpy.trim() === "" ? null : Number(next.jpy);
  return req;
}

function isEmpty(req: UpdateAiSettingsRequest): boolean {
  return Object.keys(req).length === 0;
}

/// 画面上の検証（サーバも同じ規則で検証する。ここは早めに気付かせるため）。
function validate(models: AdminModel[], d: Draft): string | null {
  if (!models.some((m) => d.enabled[m.id])) return "少なくとも 1 つのモデルを公開してください";
  if (d.default_model !== AUTO && !d.enabled[d.default_model])
    return "既定のモデルが非公開になっています";
  if (d.jpy.trim() !== "") {
    const n = Number(d.jpy);
    if (!Number.isFinite(n) || n < 1 || n > 1000) return "円換算レートは 1〜1000 円で入力してください";
  }
  return null;
}

/// モデルカタログ（公開・既定・Auto）とユーザーの選択肢・円換算レート。
export function AiModelsView() {
  const [models, setModels] = React.useState<AdminModel[] | null>(null);
  const [settings, setSettings] = React.useState<AiSettings | null>(null);
  const [base, setBase] = React.useState<Draft | null>(null);
  const [draft, setDraft] = React.useState<Draft | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [saving, setSaving] = React.useState(false);

  const load = React.useCallback(async () => {
    try {
      const r = await fetchAdminModels();
      const d = draftOf(r.models, r.settings);
      setModels(r.models);
      setSettings(r.settings);
      setBase(d);
      setDraft(d);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  React.useEffect(() => {
    void load();
  }, [load]);

  if (error) {
    return (
      <AdminPage title="モデル">
        <EmptyState icon={AlertCircle} title="モデルを読み込めませんでした" description={error} />
      </AdminPage>
    );
  }
  if (!models || !settings || !base || !draft) {
    return (
      <AdminPage title="モデル" wide>
        <div className="flex flex-col gap-4" aria-busy="true">
          <Skeleton className="h-72" />
          <Skeleton className="h-40" />
        </div>
      </AdminPage>
    );
  }

  const set = (patch: Partial<Draft>) => setDraft({ ...draft, ...patch });
  const req = diffRequest(base, draft);
  const dirty = !isEmpty(req);
  const problem = validate(models, draft);
  const label = (id: string | null | undefined) =>
    models.find((m) => m.id === id)?.label ?? id ?? "—";
  const enabledModels = models.filter((m) => draft.enabled[m.id]);

  const save = async () => {
    if (problem) return;
    setSaving(true);
    try {
      await updateAiSettings(req);
      toast({ title: "AI の設定を保存しました" });
      await load();
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
      title="モデル"
      description="組織で使える AI モデルと、チャットでの選び方を決めます。接続先と単価はデプロイ設定で管理されます。"
      wide
    >
      <Panel
        title="モデルカタログ"
        description="公開したモデルだけが使えます。保存後にデプロイ設定へ追加されたモデルは、ここで公開するまで使えません。"
      >
        <div className="overflow-x-auto">
          <table className="w-full min-w-[820px] border-collapse text-[13px]">
            <thead>
              <tr className="bg-muted/40 text-left text-xs text-muted-foreground">
                <th scope="col" className="w-16 px-4 py-2 font-medium">公開</th>
                <th scope="col" className="px-2 py-2 font-medium">モデル</th>
                <th scope="col" className="px-2 py-2 font-medium">モデル ID</th>
                <th scope="col" className="px-2 py-2 font-medium">プロバイダ</th>
                <th scope="col" className="px-2 py-2 font-medium">思考の深さ</th>
                <th scope="col" className="px-2 py-2 text-right font-medium">
                  単価（入力 / 出力・100 万トークン）
                </th>
                <th scope="col" className="w-16 px-4 py-2 text-center font-medium">既定</th>
              </tr>
            </thead>
            <tbody>
              <tr className="border-t">
                <td className="px-4 py-2.5 text-muted-foreground">—</td>
                <td className="px-2 py-2.5" colSpan={5}>
                  <div className="flex items-center gap-1.5 font-medium">
                    <Sparkles className="size-3.5 text-primary" aria-hidden />
                    Auto
                  </div>
                  <div className="mt-0.5 text-xs text-muted-foreground">
                    ユーザーがモデルを選ばないときに使います。いまの振り先:{" "}
                    {label(settings.auto_target_model)}
                    {settings.auto_target_effort
                      ? `（${EFFORT_LABEL[settings.auto_target_effort]}）`
                      : ""}
                  </div>
                </td>
                <td className="px-4 py-2.5 text-center">
                  <input
                    type="radio"
                    name="default-model"
                    aria-label="Auto を既定にする"
                    checked={draft.default_model === AUTO}
                    onChange={() => set({ default_model: AUTO })}
                    className="size-4 accent-[var(--primary)]"
                  />
                </td>
              </tr>
              {models.map((m) => {
                const on = draft.enabled[m.id];
                return (
                  <tr key={m.id} className={cn("border-t", !on && "text-muted-foreground")}>
                    <td className="px-4 py-2.5">
                      <Switch
                        checked={on}
                        onCheckedChange={(v) => set({ enabled: { ...draft.enabled, [m.id]: v } })}
                        aria-label={`${m.label} を公開`}
                      />
                    </td>
                    <td className="px-2 py-2.5">
                      <div className="flex flex-wrap items-center gap-1.5 font-medium">
                        {m.label}
                        {m.processed_abroad ? (
                          <Badge variant="warning" className="gap-1 px-1.5 py-0 text-[10.5px]">
                            <Globe2 className="size-3" aria-hidden />
                            国外処理
                          </Badge>
                        ) : null}
                      </div>
                      {m.description ? (
                        <div className="mt-0.5 text-xs text-muted-foreground">{m.description}</div>
                      ) : null}
                    </td>
                    <td className="px-2 py-2.5 font-mono text-xs text-muted-foreground">{m.id}</td>
                    <td className="px-2 py-2.5">{m.provider_label}</td>
                    <td className="px-2 py-2.5">{m.supports_effort ? "3 段階" : "—"}</td>
                    <td className="px-2 py-2.5 text-right font-mono text-xs text-muted-foreground">
                      {formatPrice(m.prompt_price_micros_per_mtok)} /{" "}
                      {formatPrice(m.completion_price_micros_per_mtok)}
                    </td>
                    <td className="px-4 py-2.5 text-center">
                      <input
                        type="radio"
                        name="default-model"
                        aria-label={`${m.label} を既定にする`}
                        checked={draft.default_model === m.id}
                        disabled={!on}
                        onChange={() => set({ default_model: m.id })}
                        className="size-4 accent-[var(--primary)] disabled:opacity-40"
                      />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </Panel>

      <Panel title="ユーザーの選択肢">
        <SettingRow
          label="チャットでのモデル選択"
          description="オフにすると、全員が既定（Auto など）で使います。"
          htmlFor="allow-user-choice"
        >
          <Switch
            id="allow-user-choice"
            checked={draft.allow_user_choice}
            onCheckedChange={(v) => set({ allow_user_choice: v })}
          />
        </SettingRow>
        <SettingRow
          label="思考の深さの初期値"
          description="ユーザーがモデルを選んだときの初期値です。チャットごとに変えられます。"
        >
          <SegmentedControl
            aria-label="思考の深さの初期値"
            size="sm"
            options={EFFORT_OPTIONS}
            value={draft.default_effort}
            onValueChange={(v) => set({ default_effort: v as EffortLevel })}
          />
        </SettingRow>
      </Panel>

      <Panel
        title="Auto の振り先"
        description="今後は問い合わせ内容でモデルを選ぶ予定です。いまは固定の 1 モデルを使います。"
      >
        <SettingRow
          label="モデル"
          description="公開していないモデルを指定した場合は、既定のモデルに切り替わります。"
        >
          <Select
            value={draft.auto_model ?? INHERIT}
            onValueChange={(v) => set({ auto_model: v === INHERIT ? null : v })}
          >
            <SelectTrigger className="h-9 w-full max-w-xs" aria-label="Auto のモデル">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={INHERIT}>デプロイ設定に従う</SelectItem>
              {enabledModels.map((m) => (
                <SelectItem key={m.id} value={m.id}>
                  {m.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingRow>
        <SettingRow label="思考の深さ">
          <Select
            value={draft.auto_effort ?? INHERIT}
            onValueChange={(v) => set({ auto_effort: v === INHERIT ? null : (v as EffortLevel) })}
          >
            <SelectTrigger className="h-9 w-full max-w-xs" aria-label="Auto の思考の深さ">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={INHERIT}>デプロイ設定に従う</SelectItem>
              {EFFORT_OPTIONS.map((o) => (
                <SelectItem key={o.value} value={o.value}>
                  {o.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingRow>
      </Panel>

      <Panel title="円での表示">
        <SettingRow
          label="円換算レート"
          description="利用量に円の参考額を並べて出します。請求・会計は USD のままです。空欄なら円を出しません。"
          htmlFor="jpy-rate"
        >
          <div className="flex items-center gap-2">
            <span className="text-[13px] text-muted-foreground">1 USD =</span>
            <Input
              id="jpy-rate"
              inputMode="decimal"
              value={draft.jpy}
              onChange={(e) => set({ jpy: e.target.value })}
              placeholder="例: 150"
              className="h-9 w-28 tabular-nums"
            />
            <span className="text-[13px] text-muted-foreground">円</span>
          </div>
        </SettingRow>
      </Panel>

      {dirty ? (
        <div className="sticky bottom-4 z-10 flex flex-wrap items-center justify-between gap-3 rounded-lg border bg-card px-4 py-3 shadow-md">
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
