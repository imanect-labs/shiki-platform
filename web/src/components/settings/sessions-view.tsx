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
import { formatDateTime, relativeTime } from "@/lib/format";
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
import { IconTile, SettingsEmpty, SettingsItem, SettingsPage, SettingsSection, StatusDot } from "./settings-ui";

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
    <SettingsPage
      title="端末とセッション"
      description="あなたのアカウントでログインしている端末です。心当たりのない端末はログアウトさせてください。"
    >
      {error ? (
        <div className="flex flex-col items-start gap-3 rounded-2xl border border-dashed px-5 py-5 text-[13px]">
          <p className="text-muted-foreground">セッションを読み込めませんでした（{error}）</p>
          <Button variant="outline" size="sm" onClick={() => setReloadKey((k) => k + 1)}>
            <RotateCw aria-hidden />
            再読み込み
          </Button>
        </div>
      ) : !rows ? (
        <div className="flex flex-col gap-3" aria-busy="true" aria-label="読み込み中">
          <Skeleton className="ml-1 h-5 w-40" />
          <Skeleton className="h-36 w-full rounded-2xl" />
        </div>
      ) : (
        <>
          <SettingsSection
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
              <SettingsEmpty>ログイン中のセッションはありません。</SettingsEmpty>
            ) : (
              <ul>
                {rows.map((row) => {
                  const { session } = row;
                  return (
                    <SettingsItem
                      key={session.id}
                      icon={<IconTile icon={row.device.mobile ? Smartphone : Laptop} />}
                      title={
                        <>
                          <span className="truncate">{deviceTitle(row.device, session)}</span>
                          {session.current ? <StatusDot>このブラウザ</StatusDot> : null}
                        </>
                      }
                      meta={
                        <>
                          {session.ip_address ? <span>IP {session.ip_address}</span> : null}
                          <span>最終アクセス {relativeTime(session.last_access_at, now)}</span>
                          <span className="max-sm:hidden">
                            ログイン {session.started_at ? formatDateTime(session.started_at) : "—"}
                          </span>
                        </>
                      }
                      action={
                        session.current ? null : (
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
                        )
                      }
                    />
                  );
                })}
              </ul>
            )}
          </SettingsSection>

          {others.length > 0 ? (
            <SettingsSection>
              <div className="flex flex-col gap-3 px-5 py-4 sm:flex-row sm:items-center sm:justify-between sm:gap-8">
                <div className="min-w-0">
                  <div className="text-[13.5px] font-medium">他の端末からすべてログアウト</div>
                  <p className="mt-0.5 text-[12.5px] leading-relaxed text-muted-foreground">
                    このブラウザ以外の {others.length} 件のセッションを終了します。
                  </p>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setConfirmAll(true)}
                  disabled={busy !== null}
                  className="text-destructive hover:text-destructive sm:shrink-0"
                >
                  <LogOut aria-hidden />
                  すべてログアウト
                </Button>
              </div>
            </SettingsSection>
          ) : null}
        </>
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
    </SettingsPage>
  );
}
