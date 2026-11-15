"use client";
import * as React from "react";
import Link from "next/link";
import { Search } from "lucide-react";
import type { components } from "@/generated/api";
import { Button } from "@/components/ui/button";
import { IconInput } from "@/components/ui/icon-input";
import { AdminPage, Panel } from "../admin-page";
import { request, params } from "./api";
type MemberPage = components["schemas"]["DirectoryMemberPage"];
export function DirectoryMembersView() {
  const [query, setQuery] = React.useState("");
  const [cursor, setCursor] = React.useState<string | null>(null);
  const [page, setPage] = React.useState<MemberPage>();
  const [error, setError] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  React.useEffect(() => {
    const controller = new AbortController();
    setBusy(true);
    const timer = setTimeout(() => {
      request<MemberPage>(`/members${params({ q: query, cursor })}`, {
        signal: controller.signal,
      })
        .then((result) => {
          setPage(result);
          setError("");
        })
        .catch((e) => {
          if (!controller.signal.aborted) setError(e.message);
        })
        .finally(() => {
          if (!controller.signal.aborted) setBusy(false);
        });
    }, 200);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [query, cursor]);
  return (
    <AdminPage
      title="メンバー"
      description="組織で認証済みのメンバーと、最終認証同期時の所属ロールを確認します。"
      wide
      actions={
        <Button size="sm" variant="outline" asChild>
          <Link href="/admin/directory">ADとの対応付けを管理</Link>
        </Button>
      }
    >
      <IconInput
        icon={Search}
        aria-label="メンバーを検索"
        value={query}
        onValueChange={(value) => {
          setQuery(value);
          setCursor(null);
        }}
        placeholder="名前・メールで検索"
        containerClassName="max-w-sm"
      />
      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}
      <Panel>
        <div className="overflow-auto" aria-busy={busy}>
          <table className="w-full text-left text-[13px]">
            <thead className="border-b bg-muted/20 text-xs text-muted-foreground">
              <tr>
                <th className="px-4 py-3 font-medium">メンバー</th>
                <th className="px-4 py-3 font-medium">直接所属するロール</th>
              </tr>
            </thead>
            <tbody className="divide-y">
              {page?.items.map((member) => (
                <tr key={member.id}>
                  <td className="px-4 py-3">
                    <p className="font-medium">{member.display_name}</p>
                    <p className="mt-1 text-xs text-muted-foreground">
                      {member.email}
                    </p>
                  </td>
                  <td className="max-w-md px-4 py-3 text-xs leading-6">
                    {member.role_names.join("、") || "なし"}
                    {member.role_count > member.role_names.length && (
                      <span className="text-muted-foreground">
                        {" "}
                        ほか{member.role_count - member.role_names.length}件
                      </span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {!page?.items.length && (
            <p className="p-5 text-xs text-muted-foreground">
              {busy
                ? "メンバーを取得中…"
                : "該当するメンバーはいません。ADで作成されたユーザーは、初回ログイン後に表示されます。"}
            </p>
          )}
        </div>
        <div className="flex justify-end gap-2 border-t p-3">
          {cursor && (
            <Button size="sm" variant="ghost" onClick={() => setCursor(null)}>
              先頭に戻る
            </Button>
          )}
          {page?.next_cursor && (
            <Button
              size="sm"
              variant="outline"
              onClick={() => setCursor(page.next_cursor ?? null)}
            >
              次へ
            </Button>
          )}
        </div>
      </Panel>
      <p className="text-xs leading-6 text-muted-foreground">
        アカウントの作成・停止はADまたはKeycloakで管理します。ADの所属変更と対応付けは、次回ログイン・セッション更新時に同期されます。
      </p>
    </AdminPage>
  );
}
