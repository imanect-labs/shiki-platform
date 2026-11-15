"use client";

/// DM・非公開チャンネルの閲覧の申請（承認レコード）の一覧と、承認・却下・取り消し（design §4.14 eDiscovery）。
///
/// 一覧は「承認者は全件・監査担当者は自分の申請」をサーバが返し分ける。申請者本人は承認できない
/// （サーバが 403 を返す）ので、自分の申請には承認の操作を出さない。

import * as React from "react";
import { Loader2, ShieldCheck } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { decideAccess, listAccessRequests, revokeAccess, type AccessGrant } from "@/lib/messaging-api";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";
import { formatDateTime } from "@/lib/format";

const STATUS: Record<string, { label: string; tone: string }> = {
  pending: { label: "承認待ち", tone: "bg-[color-mix(in_oklab,var(--season-autumn)_28%,transparent)] text-foreground" },
  approved: { label: "承認済み", tone: "bg-[color-mix(in_oklab,var(--season-spring)_30%,transparent)] text-foreground" },
  rejected: { label: "却下", tone: "bg-muted text-muted-foreground" },
  revoked: { label: "取り消し済み", tone: "bg-muted text-muted-foreground" },
};

function day(iso: string): string {
  return new Date(iso).toLocaleDateString("ja-JP");
}

export function AccessRequestList({
  me,
  channelNames,
  refreshKey = 0,
}: {
  me: string;
  /// チャンネル id → 名前（監査担当者の画面では分かる・承認者の一覧 API は id しか返さない）。
  channelNames?: ReadonlyMap<string, string>;
  refreshKey?: number;
}) {
  const [items, setItems] = React.useState<AccessGrant[] | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState<string | null>(null);
  const [, setNamesTick] = React.useState(0);

  const load = React.useCallback(() => {
    listAccessRequests()
      .then((r) => {
        setItems(r);
        setError(null);
        const ids = r.flatMap((g) => [g.requester, g.approver ?? ""]).filter(Boolean);
        void resolveUserNames(ids).then((learned) => learned && setNamesTick((n) => n + 1));
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  }, []);
  React.useEffect(load, [load, refreshKey]);

  const act = async (key: string, fn: () => Promise<AccessGrant>, done: string) => {
    setBusy(key);
    try {
      const g = await fn();
      setItems((prev) => prev?.map((x) => (x.id === g.id ? g : x)) ?? prev);
      toast({ title: done });
    } catch (e) {
      toast({ title: "操作できませんでした", description: e instanceof Error ? e.message : String(e), variant: "destructive" });
    } finally {
      setBusy(null);
    }
  };

  const name = (id: string | null | undefined) => (id ? (id === me ? "自分" : (cachedUserName(id) ?? id)) : "—");

  if (error) {
    return (
      <p className="rounded-xl border border-dashed border-border px-4 py-8 text-center text-[12.5px] text-muted-foreground" role="alert">
        {error}
      </p>
    );
  }
  if (items === null) {
    return (
      <div className="flex items-center justify-center gap-2 py-10 text-[12.5px] text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        読み込んでいます
      </div>
    );
  }
  if (items.length === 0) {
    return <EmptyState icon={ShieldCheck} title="閲覧の申請はありません" description="DM・非公開チャンネルの閲覧が申請されると、ここに並びます。" />;
  }

  return (
    <ul className="overflow-hidden rounded-xl border border-border/60 bg-card/40" aria-label="閲覧の申請">
      {items.map((g, i) => {
        const st = STATUS[g.status] ?? { label: g.status, tone: "bg-muted text-muted-foreground" };
        const mine = g.requester === me;
        return (
          <li key={g.id} className={cn("flex flex-col gap-2 px-4 py-3 sm:flex-row sm:items-start", i > 0 && "shiki-dash-top")} data-testid="access-request">
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-1.5">
                <span className={cn("rounded-full px-2 py-px text-[11px] font-medium", st.tone)}>{st.label}</span>
                <span className="truncate text-[13px] font-medium text-foreground">
                  {g.channel_name || channelNames?.get(g.channel_id) || `チャンネル ${g.channel_id.slice(0, 8)}`}
                </span>
              </div>
              <p className="mt-1 text-[12.5px] leading-relaxed text-foreground/90">{g.reason}</p>
              <dl className="mt-1 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[11.5px] text-muted-foreground">
                <dt>申請者</dt>
                <dd>{name(g.requester)}</dd>
                <dt>対象の期間</dt>
                <dd>
                  {day(g.messages_from)} 〜 {day(new Date(new Date(g.messages_until).getTime() - 1).toISOString())}
                </dd>
                <dt>閲覧できる日数</dt>
                <dd>
                  {g.valid_days} 日{g.valid_until ? `（${formatDateTime(g.valid_until)} まで）` : ""}
                </dd>
                {g.approver ? (
                  <>
                    <dt>判断した人</dt>
                    <dd>
                      {name(g.approver)}
                      {g.decided_at ? `（${formatDateTime(g.decided_at)}）` : ""}
                    </dd>
                  </>
                ) : null}
                <dt>申請日時</dt>
                <dd>{formatDateTime(g.created_at)}</dd>
              </dl>
            </div>
            <div className="flex shrink-0 flex-wrap gap-1.5">
              {g.status === "pending" && !mine ? (
                <>
                  <Button size="sm" loading={busy === `a:${g.id}`} onClick={() => void act(`a:${g.id}`, () => decideAccess(g.id, true), "承認しました")}>
                    承認
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    loading={busy === `r:${g.id}`}
                    onClick={() => void act(`r:${g.id}`, () => decideAccess(g.id, false), "却下しました")}
                  >
                    却下
                  </Button>
                </>
              ) : null}
              {g.status === "pending" && mine ? (
                <span className="text-[11.5px] text-muted-foreground">承認者の判断を待っています（自分では承認できません）</span>
              ) : null}
              {g.status === "approved" ? (
                <Button
                  size="sm"
                  variant="outline"
                  className="text-destructive hover:text-destructive"
                  loading={busy === `v:${g.id}`}
                  onClick={() => void act(`v:${g.id}`, () => revokeAccess(g.id), "承認を取り消しました")}
                >
                  取り消す
                </Button>
              ) : null}
            </div>
          </li>
        );
      })}
    </ul>
  );
}
