"use client";

import * as React from "react";

import { useMe } from "@/hooks/use-me";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { SettingsPage, SettingsRow, SettingsSection, SoonBadge } from "./settings-ui";

/// 読み取り専用の値（IdP が正本の項目）。
function ReadOnly({ children }: { children: React.ReactNode }) {
  return <div className="min-w-0 break-all text-[13px] text-foreground/85 sm:text-right">{children}</div>;
}

/// 設定 → プロフィール。アカウント情報は /me（IdP 由来）を表示し、表示名・写真など
/// shiki 側で持つ予定の項目はモック（保存されない）。
export function ProfileView() {
  const { data, loading } = useMe();
  const [name, setName] = React.useState("");
  const initial = (data?.email ?? data?.id ?? "?").slice(0, 1).toUpperCase();

  return (
    <SettingsPage title="プロフィール" description="アカウントの情報と、ほかのメンバーへの見え方を設定します。">
      <SettingsSection
        title={
          <>
            公開プロフィール
            <SoonBadge />
          </>
        }
        description="共有ダイアログやメンションで、ほかのメンバーに表示されます。"
      >
        <SettingsRow label="写真" description="正方形の画像を推奨します。">
          <div className="flex items-center gap-3">
            <span
              aria-hidden
              className="flex size-12 items-center justify-center rounded-full border bg-background text-lg font-semibold text-foreground/80"
            >
              {initial}
            </span>
            <Button variant="outline" size="sm" disabled>
              画像を変更
            </Button>
          </div>
        </SettingsRow>
        <SettingsRow label="表示名" htmlFor="profile-name" description="空欄ならメールアドレスを表示します。">
          <Input
            id="profile-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={data?.email?.split("@")[0] ?? "表示名"}
            className="w-full sm:w-72"
          />
        </SettingsRow>
        <SettingsRow label="役職・自己紹介" htmlFor="profile-title">
          <Input id="profile-title" placeholder="例: 営業 1 課・提案書の作成を担当" className="w-full sm:w-72" />
        </SettingsRow>
      </SettingsSection>

      <SettingsSection title="アカウント" description="組織のディレクトリ（ID 基盤）が管理する項目です。変更は管理者に依頼してください。">
        {loading || !data ? (
          <div className="flex flex-col gap-3 px-5 py-5" aria-busy="true" aria-label="読み込み中">
            <Skeleton className="h-5 w-64" />
            <Skeleton className="h-5 w-48" />
            <Skeleton className="h-5 w-56" />
          </div>
        ) : (
          <>
            <SettingsRow label="メールアドレス">
              <ReadOnly>{data.email ?? "—"}</ReadOnly>
            </SettingsRow>
            <SettingsRow label="組織">
              <ReadOnly>
                {data.org}
                {data.is_admin ? <span className="ml-2 text-xs text-muted-foreground">管理者</span> : null}
              </ReadOnly>
            </SettingsRow>
            <SettingsRow label="所属ロール" description="共有やアクセス権の判定に使われます。">
              {data.roles.length > 0 ? (
                <div className="flex flex-wrap gap-1.5 sm:justify-end">
                  {data.roles.map((r) => (
                    <Badge key={r} variant="outline">
                      {r}
                    </Badge>
                  ))}
                </div>
              ) : (
                <ReadOnly>—</ReadOnly>
              )}
            </SettingsRow>
            <SettingsRow label="ユーザー ID">
              <ReadOnly>
                <code className="font-mono text-xs text-muted-foreground">{data.id}</code>
              </ReadOnly>
            </SettingsRow>
          </>
        )}
      </SettingsSection>
    </SettingsPage>
  );
}
