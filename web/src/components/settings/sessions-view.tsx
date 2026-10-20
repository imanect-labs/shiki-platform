"use client";

import * as React from "react";
import { Laptop, Loader2, LogOut, RotateCw, Smartphone } from "lucide-react";

import {
  fetchSessions,
  signOutOtherSessions,
  signOutSession,
  type DeviceInfo,
  type SessionInfo,
} from "@/lib/account-api";
import { formatDateTime } from "@/lib/format";
import { AdminPage, Panel } from "@/components/admin/admin-page";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/use-toast";

/// 「3 分前」「2 日前」のような相対時刻（1 週間以上前は日時）。
function relativeTime(iso: string | null | undefined, now: number): string {
  if (!iso) return "—";
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "—";
  const sec = Math.max(0, Math.round((now - t) / 1000));
  if (sec < 60) return "たった今";
  if (sec < 3600) return `${Math.floor(sec / 60)} 分前`;
  if (sec < 86400) return `${Math.floor(sec / 3600)} 時間前`;
  if (sec < 7 * 86400) return `${Math.floor(sec / 86400)} 日前`;
  return formatDateTime(iso);
}

/// 端末の見出し（「Chrome · macOS 14」）。Keycloak が判別できない値は落としてある。
function deviceTitle(device: DeviceInfo, session: SessionInfo): string {
  const browser = session.browser?.split("/")[0];
  const os = [device.os, device.os_version].filter(Boolean).join(" ");
  const parts = [browser, os || device.device].filter(Boolean);
  return parts.length > 0 ? parts.join(" · ") : "不明な端末";
}

type Row = { device: DeviceInfo; session: SessionInfo };

/// 端末ごとのセッションを 1 行 1 セッションに平らにする（このブラウザを先頭・あとは新しい順）。
function flatten(devices: DeviceInfo[]): Row[] {
  const rows = devices.flatMap((device) => device.sessions.map((session) => ({ device, session })));
  return rows.sort((a, b) => {
    if (a.session.current !== b.session.current) return a.session.current ? -1 : 1;
    return (b.session.last_access_at ?? "").localeCompare(a.session.last_access_at ?? "");
  });
}

/// 設定 → 端末とセッション。Keycloak の SSO セッションを一覧し、他の端末からログアウトさせる。
/// ログアウトさせると Keycloak が該当端末の shiki セッションへ backchannel logout を送る。
export function SessionsView() {
  const [rows, setRows] = React.useState<Row[] | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [reloadKey, setReloadKey] = React.useState(0);
  const [busy, setBusy] = React.useState<string | null>(null);
  const [confirmAll, setConfirmAll] = React.useState(false);
  // 相対時刻の基準（読み込んだ時刻）。
  const [now, setNow] = React.useState(0);

  React.useEffect(() => {
    let active = true;
    setError(null);
    fetchSessions()
      .then((d) => {
        if (!active) return;
        setNow(Date.now());
        setRows(flatten(d.devices));
      })
      .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      active = false;
    };
  }, [reloadKey]);

  const others = rows?.filter((r) => !r.session.current) ?? [];

  const revoke = async (row: Row) => {
    setBusy(row.session.id);
    try {
      await signOutSession(row.session.id);
      setRows((prev) => prev?.filter((r) => r.session.id !== row.session.id) ?? null);
      toast({ title: "ログアウトさせました", description: deviceTitle(row.device, row.session) });
    } catch (e) {
      toast({
        title: "ログアウトできませんでした",
        description: e instanceof Error ? e.message : String(e),
        variant: "destructive",
      });
    } finally {
      setBusy(null);
    }
  };

  const revokeAll = async () => {
    setBusy("all");
    try {
      await signOutOtherSessions();
      setRows((prev) => prev?.filter((r) => r.session.current) ?? null);
      setConfirmAll(false);
      toast({ title: "他の端末からすべてログアウトさせました" });
    } catch (e) {
      toast({
        title: "ログアウトできませんでした",
        description: e instanceof Error ? e.message : String(e),
        variant: "destructive",
      });
    } finally {
      setBusy(null);
    }
  };

  return (
    <AdminPage
      title="端末とセッション"
      description="あなたのアカウントでログインしている端末です。心当たりのない端末はログアウトさせてください。"
      actions={
        <Button
          variant="outline"
          size="sm"
          onClick={() => setConfirmAll(true)}
          disabled={!rows || others.length === 0 || busy !== null}
        >
          <LogOut aria-hidden />
          他の端末からすべてログアウト
        </Button>
      }
    >
      {error ? (
        <div className="flex flex-col items-start gap-3 rounded-lg border px-4 py-4 text-[13px]">
          <p className="text-muted-foreground">セッションを読み込めませんでした（{error}）</p>
          <Button variant="outline" size="sm" onClick={() => setReloadKey((k) => k + 1)}>
            <RotateCw aria-hidden />
            再読み込み
          </Button>
        </div>
      ) : !rows ? (
        <div className="flex flex-col gap-2" aria-busy="true" aria-label="読み込み中">
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      ) : (
        <Panel
          title="ログイン中の端末"
          description={`${rows.length} 件のセッション`}
          actions={
            <Button variant="ghost" size="sm" onClick={() => setReloadKey((k) => k + 1)}>
              <RotateCw aria-hidden />
              更新
            </Button>
          }
        >
          {rows.length === 0 ? (
            <p className="px-4 py-6 text-center text-[13px] text-muted-foreground">
              ログイン中のセッションはありません。
            </p>
          ) : (
            <ul className="divide-y">
              {rows.map((row) => {
                const Icon = row.device.mobile ? Smartphone : Laptop;
                const { session } = row;
                return (
                  <li key={session.id} className="flex items-center gap-3 px-4 py-3.5">
                    <span
                      aria-hidden
                      className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-muted text-foreground/70"
                    >
                      <Icon className="size-5" />
                    </span>
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="truncate text-[13.5px] font-medium">
                          {deviceTitle(row.device, session)}
                        </span>
                        {session.current ? <Badge variant="success">このブラウザ</Badge> : null}
                      </div>
                      <div className="mt-0.5 flex flex-wrap gap-x-3 gap-y-0.5 text-xs text-muted-foreground">
                        {session.ip_address ? <span>IP {session.ip_address}</span> : null}
                        <span>最終アクセス {relativeTime(session.last_access_at, now)}</span>
                        <span className="max-sm:hidden">
                          ログイン {session.started_at ? formatDateTime(session.started_at) : "—"}
                        </span>
                      </div>
                    </div>
                    {session.current ? null : (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => void revoke(row)}
                        disabled={busy !== null}
                        aria-label={`${deviceTitle(row.device, session)}からログアウト`}
                      >
                        {busy === session.id ? (
                          <Loader2 className="animate-spin" aria-hidden />
                        ) : (
                          <LogOut aria-hidden />
                        )}
                        <span className="max-sm:sr-only">ログアウト</span>
                      </Button>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </Panel>
      )}

      <Dialog open={confirmAll} onOpenChange={(open) => busy === null && setConfirmAll(open)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>他の端末からすべてログアウトしますか？</DialogTitle>
            <DialogDescription>
              このブラウザ以外の {others.length} 件のセッションが終了します。それらの端末では再度ログインが必要になります。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmAll(false)} disabled={busy !== null}>
              キャンセル
            </Button>
            <Button variant="destructive" onClick={() => void revokeAll()} disabled={busy !== null}>
              {busy === "all" ? <Loader2 className="animate-spin" aria-hidden /> : <LogOut aria-hidden />}
              すべてログアウト
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </AdminPage>
  );
}
