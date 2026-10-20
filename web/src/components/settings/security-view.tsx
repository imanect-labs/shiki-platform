"use client";

import * as React from "react";
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import {
  CheckCircle2,
  Fingerprint,
  KeyRound,
  Loader2,
  Plus,
  RotateCw,
  Smartphone,
  Trash2,
  TriangleAlert,
  XCircle,
} from "lucide-react";

import {
  fetchSecurity,
  startAccountAction,
  type AccountActionKind,
  type CredentialInfo,
  type SecurityOverview,
} from "@/lib/account-api";
import { formatDateTime } from "@/lib/format";
import { cn } from "@/lib/utils";
import { AdminPage, Panel, SettingRow } from "@/components/admin/admin-page";
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

/// Keycloak から戻ってきたときの結果表示（`?action=…&status=…`。値は BFF が閉じた語彙に丸めている）。
const ACTION_LABEL: Record<string, string> = {
  passkey: "パスキーの登録",
  otp: "認証アプリの設定",
  password: "パスワードの変更",
  delete_credential: "ログイン方法の削除",
};

function ResultNotice({ action, status }: { action: string; status: string }) {
  const label = ACTION_LABEL[action];
  if (!label) return null;
  const tone =
    status === "success"
      ? { icon: CheckCircle2, text: `${label}が完了しました。`, cls: "border-transparent bg-[oklch(0.95_0.04_150)] text-[oklch(0.35_0.09_150)] dark:bg-[oklch(0.28_0.05_150)] dark:text-[oklch(0.86_0.08_150)]" }
      : status === "cancelled"
        ? { icon: XCircle, text: `${label}をキャンセルしました。`, cls: "bg-muted/50 text-muted-foreground" }
        : { icon: TriangleAlert, text: `${label}に失敗しました。もう一度お試しください。`, cls: "border-transparent bg-destructive/10 text-destructive" };
  const Icon = tone.icon;
  return (
    <div
      role={status === "error" ? "alert" : "status"}
      className={cn("flex items-center gap-2.5 rounded-lg border px-3.5 py-2.5 text-[13px]", tone.cls)}
    >
      <Icon className="size-4 shrink-0" aria-hidden />
      {tone.text}
    </div>
  );
}

/// 確認ダイアログの対象（削除する資格情報）。
type PendingDelete = { kind: "passkey" | "otp"; credential: CredentialInfo };

function credentialName(kind: "passkey" | "otp", c: CredentialInfo): string {
  if (c.label) return c.label;
  return kind === "passkey" ? "名前のないパスキー" : "認証アプリ";
}

function CredentialList({
  kind,
  items,
  empty,
  onDelete,
}: {
  kind: "passkey" | "otp";
  items: CredentialInfo[];
  empty: string;
  onDelete: (c: CredentialInfo) => void;
}) {
  const Icon = kind === "passkey" ? Fingerprint : Smartphone;
  if (items.length === 0) {
    return <p className="px-4 py-5 text-center text-[13px] text-muted-foreground">{empty}</p>;
  }
  return (
    <ul className="divide-y">
      {items.map((c) => (
        <li key={c.id} className="flex items-center gap-3 px-4 py-3">
          <span
            aria-hidden
            className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-foreground/70"
          >
            <Icon className="size-[18px]" />
          </span>
          <div className="min-w-0 flex-1">
            <div className="truncate text-[13.5px] font-medium">{credentialName(kind, c)}</div>
            <div className="text-xs text-muted-foreground">
              {c.created_at ? `${formatDateTime(c.created_at)} に追加` : "追加日時不明"}
            </div>
          </div>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => onDelete(c)}
            aria-label={`${credentialName(kind, c)}を削除`}
            className="text-muted-foreground hover:text-destructive"
          >
            <Trash2 aria-hidden />
            <span className="max-sm:sr-only">削除</span>
          </Button>
        </li>
      ))}
    </ul>
  );
}

/// 設定 → ログイン方法（パスワード・パスキー・認証アプリ）。
///
/// 一覧は Keycloak Account API（BFF 経由）から読む。追加・変更・削除は Keycloak の画面で行い
/// （再認証と確認を Keycloak が挟む）、終わると `?action=…&status=…` 付きでここへ戻る。
export function SecurityView() {
  const router = useRouter();
  const pathname = usePathname();
  const searchParams = useSearchParams();
  const [data, setData] = React.useState<SecurityOverview | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [reloadKey, setReloadKey] = React.useState(0);
  const [pending, setPending] = React.useState<PendingDelete | null>(null);
  // Keycloak へ移動中（二度押し防止・ボタンにスピナー）。
  const [leaving, setLeaving] = React.useState<string | null>(null);
  const [notice, setNotice] = React.useState<{ action: string; status: string } | null>(null);

  // ブラウザの「戻る」（bfcache 復元）で戻ってきたら、移動中の表示を解く。
  React.useEffect(() => {
    const onShow = (e: PageTransitionEvent) => e.persisted && setLeaving(null);
    window.addEventListener("pageshow", onShow);
    return () => window.removeEventListener("pageshow", onShow);
  }, []);

  // 戻ってきた結果を一度だけ表示し、URL からは消す（再読み込みで出し直さない）。
  React.useEffect(() => {
    const action = searchParams.get("action");
    const status = searchParams.get("status");
    if (action && status) {
      setNotice({ action, status });
      router.replace(pathname, { scroll: false });
    }
  }, [searchParams, router, pathname]);

  React.useEffect(() => {
    let active = true;
    setError(null);
    fetchSecurity()
      .then((d) => active && setData(d))
      .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      active = false;
    };
  }, [reloadKey]);

  const go = (action: AccountActionKind, credentialId?: string) => {
    setLeaving(credentialId ?? action);
    startAccountAction(action, credentialId);
  };

  const twoFactorOn = (data?.otp.length ?? 0) > 0;

  return (
    <AdminPage
      title="ログイン方法"
      description="パスワード・パスキー・2 段階認証を管理します。変更は認証画面で本人確認してから反映されます。"
    >
      {notice ? <ResultNotice action={notice.action} status={notice.status} /> : null}

      {error ? (
        <div className="flex flex-col items-start gap-3 rounded-lg border px-4 py-4 text-[13px]">
          <p className="text-muted-foreground">ログイン方法を読み込めませんでした（{error}）</p>
          <Button variant="outline" size="sm" onClick={() => setReloadKey((k) => k + 1)}>
            <RotateCw aria-hidden />
            再読み込み
          </Button>
        </div>
      ) : !data ? (
        <div className="flex flex-col gap-4" aria-busy="true" aria-label="読み込み中">
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-40 w-full" />
          <Skeleton className="h-32 w-full" />
        </div>
      ) : (
        <>
          <Panel>
            <SettingRow
              label="パスワード"
              description={
                data.password.configured
                  ? data.password.updated_at
                    ? `最終更新 ${formatDateTime(data.password.updated_at)}`
                    : "設定済み"
                  : "パスワードは設定されていません。"
              }
            >
              <Button
                variant="outline"
                size="sm"
                onClick={() => go("password")}
                disabled={leaving !== null}
                className="md:ml-auto"
              >
                {leaving === "password" ? <Loader2 className="animate-spin" aria-hidden /> : <KeyRound aria-hidden />}
                {data.password.configured ? "パスワードを変更" : "パスワードを設定"}
              </Button>
            </SettingRow>
          </Panel>

          <Panel
            title="パスキー"
            description="指紋・顔認証・端末の画面ロックで、パスワードを入力せずにログインできます。"
            actions={
              data.passkey_available ? (
                <Button size="sm" onClick={() => go("passkey")} disabled={leaving !== null}>
                  {leaving === "passkey" ? <Loader2 className="animate-spin" aria-hidden /> : <Plus aria-hidden />}
                  パスキーを追加
                </Button>
              ) : null
            }
          >
            {data.passkey_available ? (
              <CredentialList
                kind="passkey"
                items={data.passkeys}
                empty="パスキーはまだ登録されていません。"
                onDelete={(c) => setPending({ kind: "passkey", credential: c })}
              />
            ) : (
              <p className="px-4 py-5 text-[13px] text-muted-foreground">
                この組織ではパスキーが有効になっていません。
              </p>
            )}
          </Panel>

          <Panel
            title={
              <span className="flex items-center gap-2">
                2 段階認証
                <Badge variant={twoFactorOn ? "success" : "muted"}>{twoFactorOn ? "有効" : "無効"}</Badge>
              </span>
            }
            description="パスワードでログインするとき、認証アプリの確認コードも求めます。"
            actions={
              data.otp_available ? (
                <Button
                  size="sm"
                  variant={twoFactorOn ? "outline" : "default"}
                  onClick={() => go("otp")}
                  disabled={leaving !== null}
                >
                  {leaving === "otp" ? <Loader2 className="animate-spin" aria-hidden /> : <Plus aria-hidden />}
                  {twoFactorOn ? "認証アプリを追加" : "認証アプリを設定"}
                </Button>
              ) : null
            }
          >
            {data.otp_available ? (
              <CredentialList
                kind="otp"
                items={data.otp}
                empty="認証アプリは設定されていません。Google Authenticator や Microsoft Authenticator などが使えます。"
                onDelete={(c) => setPending({ kind: "otp", credential: c })}
              />
            ) : (
              <p className="px-4 py-5 text-[13px] text-muted-foreground">
                この組織では認証アプリが有効になっていません。
              </p>
            )}
          </Panel>
        </>
      )}

      <Dialog open={pending !== null} onOpenChange={(open) => !open && setPending(null)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>
              {pending ? `${credentialName(pending.kind, pending.credential)}を削除しますか？` : ""}
            </DialogTitle>
            <DialogDescription>
              {pending?.kind === "passkey"
                ? "削除すると、このパスキーではログインできなくなります。"
                : "削除すると、この認証アプリの確認コードは使えなくなります。最後の 1 つを削除すると 2 段階認証は無効になります。"}
              認証画面で本人確認をしてから削除します。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setPending(null)}>
              キャンセル
            </Button>
            <Button
              variant="destructive"
              disabled={leaving !== null}
              onClick={() => pending && go("delete_credential", pending.credential.id)}
            >
              {leaving && leaving === pending?.credential.id ? (
                <Loader2 className="animate-spin" aria-hidden />
              ) : (
                <Trash2 aria-hidden />
              )}
              削除へ進む
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </AdminPage>
  );
}
