"use client";

/// メッセージ › 設定（組織の管理者・Task 14.15・design §4.14）。
///
/// 公開チャンネルを作れる人・発言の保存期間・DM と非公開チャンネルの閲覧を承認する人。保存は `rev` を
/// そのまま返す楽観ロックで、他の人が先に変えていたら（409）読み直して知らせる。

import * as React from "react";
import { AlertCircle } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/use-toast";
import {
  getMessagingSettings,
  MessagingError,
  putMessagingSettings,
  type MessagingSettings,
} from "@/lib/messaging-api";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";
import { UserMultiPicker, type PickedUser } from "@/components/messages/user-picker";
import { AdminPage, Panel, SettingRow } from "./admin-page";

type Draft = { creators: "everyone" | "admins"; retention: string; approvers: PickedUser[] };

function draftOf(s: MessagingSettings): Draft {
  return {
    creators: s.public_channel_creators === "admins" ? "admins" : "everyone",
    retention: s.retention_days != null ? String(s.retention_days) : "",
    approvers: s.access_approvers.map((id) => ({ id, name: cachedUserName(id) ?? id })),
  };
}

function same(a: Draft, b: Draft): boolean {
  return (
    a.creators === b.creators &&
    a.retention.trim() === b.retention.trim() &&
    a.approvers.map((x) => x.id).join(",") === b.approvers.map((x) => x.id).join(",")
  );
}

/// 入力の検査（サーバが正。ここは早めに気付かせるため）。
export function validateMessagingDraft(d: Draft, auditDays: number): string | null {
  const v = d.retention.trim();
  if (v === "") return null;
  if (!/^\d+$/.test(v) || Number(v) < 1) return "保存期間は 1 以上の整数（日）で入力してください（空欄は無期限）";
  if (auditDays > 0 && Number(v) < auditDays)
    return `保存期間は監査ログの保存期間（${auditDays} 日）より短くできません`;
  return null;
}

export function MessagingSettingsView() {
  const [settings, setSettings] = React.useState<MessagingSettings | null>(null);
  const [base, setBase] = React.useState<Draft | null>(null);
  const [draft, setDraft] = React.useState<Draft | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [saving, setSaving] = React.useState(false);

  const apply = React.useCallback((s: MessagingSettings) => {
    setSettings(s);
    const d = draftOf(s);
    setBase(d);
    setDraft(d);
    // 承認者の表示名を引いてから作り直す（id のまま見せる時間を短くする）。
    void resolveUserNames(s.access_approvers).then((learned) => {
      if (!learned) return;
      const named = draftOf(s);
      setBase(named);
      setDraft((cur) => (cur && same(cur, d) ? named : cur));
    });
  }, []);

  const load = React.useCallback(() => {
    getMessagingSettings()
      .then(apply)
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  }, [apply]);
  React.useEffect(load, [load]);

  if (error) {
    return (
      <AdminPage title="メッセージの設定">
        <EmptyState icon={AlertCircle} title="設定を読み込めませんでした" description={error} />
      </AdminPage>
    );
  }
  if (!settings || !base || !draft) {
    return (
      <AdminPage title="メッセージの設定">
        <div className="flex flex-col gap-4" aria-busy="true">
          <Skeleton className="h-64" />
        </div>
      </AdminPage>
    );
  }

  const audit = settings.audit_retention_days ?? 0;
  const problem = validateMessagingDraft(draft, audit);
  const dirty = !same(base, draft);

  const save = async () => {
    if (problem || !dirty) return;
    setSaving(true);
    try {
      const v = draft.retention.trim();
      apply(
        await putMessagingSettings({
          ...settings,
          public_channel_creators: draft.creators,
          retention_days: v === "" ? null : Number(v),
          access_approvers: draft.approvers.map((u) => u.id),
          rev: settings.rev,
        }),
      );
      toast({ title: "メッセージの設定を保存しました" });
    } catch (e) {
      if (e instanceof MessagingError && e.status === 409) {
        toast({
          title: "他の人が先に設定を変更しました",
          description: "最新の設定を読み込み直しました。もう一度変更してください。",
          variant: "destructive",
        });
        load();
      } else {
        toast({ title: "保存できませんでした", description: e instanceof Error ? e.message : String(e), variant: "destructive" });
      }
    } finally {
      setSaving(false);
    }
  };

  return (
    <AdminPage
      title="メッセージの設定"
      description="職員間メッセージ（チャンネル・DM）の組織全体の設定です。変更は監査ログに残ります。"
    >
      <Panel title="チャンネル">
        <SettingRow
          label="公開チャンネルを作れる人"
          description="非公開チャンネルと DM は、組織のメンバーなら誰でも作れます。"
        >
          <div className="flex flex-col gap-1.5" role="radiogroup" aria-label="公開チャンネルを作れる人">
            {(
              [
                { v: "everyone", label: "組織のメンバー全員" },
                { v: "admins", label: "組織の管理者だけ" },
              ] as const
            ).map((o) => (
              <label key={o.v} className="flex items-center gap-2 text-[13px]">
                <input
                  type="radio"
                  name="creators"
                  value={o.v}
                  checked={draft.creators === o.v}
                  onChange={() => setDraft({ ...draft, creators: o.v })}
                  className="size-4 accent-[var(--primary)]"
                />
                {o.label}
              </label>
            ))}
          </div>
        </SettingRow>
      </Panel>

      <Panel title="保存期間">
        <SettingRow
          label="発言の保存期間"
          htmlFor="msg-retention"
          description={
            <>
              期限を過ぎた発言は検索から消え、「この発言は保存期間を過ぎました」と表示されます。
              監査ログの保存期間（{audit > 0 ? `${audit} 日` : "未設定"}）より短くできません・空欄は無期限です。
            </>
          }
        >
          <div className="flex items-center gap-2">
            <Input
              id="msg-retention"
              inputMode="numeric"
              placeholder="無期限"
              value={draft.retention}
              onChange={(e) => setDraft({ ...draft, retention: e.target.value })}
              aria-invalid={problem ? true : undefined}
              className={cn("h-9 w-32 text-right tabular-nums", draft.retention !== base.retention && "font-medium")}
            />
            <span className="text-xs text-muted-foreground">日</span>
          </div>
        </SettingRow>
      </Panel>

      <Panel
        title="閲覧の承認者"
        description="監査担当者が DM・非公開チャンネルを読むには、ここで指名した人の承認が要ります。未指名なら誰も読めません。申請者本人は承認できません。"
      >
        <div className="px-4 py-4">
          <UserMultiPicker
            value={draft.approvers}
            onChange={(approvers) => setDraft({ ...draft, approvers })}
            label="承認者を探す"
          />
          {draft.approvers.length === 0 ? (
            <p className="mt-2 text-[12px] text-muted-foreground">承認者がいないため、DM・非公開チャンネルは閲覧できません。</p>
          ) : null}
        </div>
      </Panel>

      {dirty ? (
        <div className="sticky bottom-4 z-10 flex flex-wrap items-center justify-between gap-3 rounded-lg border bg-card px-4 py-3 shadow-md max-md:bottom-[max(0.75rem,env(safe-area-inset-bottom))]">
          <p className={cn("text-[13px]", problem ? "text-destructive" : "text-muted-foreground")} role={problem ? "alert" : undefined}>
            {problem ?? "保存していない変更があります"}
          </p>
          <div className="flex gap-2">
            <Button variant="outline" size="sm" onClick={() => setDraft(base)} disabled={saving}>
              取り消す
            </Button>
            <Button size="sm" onClick={() => void save()} loading={saving} disabled={!!problem} data-testid="messaging-settings-save">
              変更を保存
            </Button>
          </div>
        </div>
      ) : null}
    </AdminPage>
  );
}
