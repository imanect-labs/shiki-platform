"use client";

/// アプリのテーブルを使える部署・グループ（GAP-68）。インストールの後から、アプリのすべてのテーブルに、部署・グループの
/// 閲覧・編集・追加だけを足す・外す。意味はインストールの要求（viewer_roles・editor_roles・submitter_roles）と同じ。
/// 変えられるのはインストールできる人（アプリの持ち主）だけで、サーバーもほかの人を 403 にする。変えた内容は監査ログに残る。
///
/// 部品はテーブルの共有と同じ SharePeoplePanel（呼び名もテーブルの共有と同じ・相手は部署・グループだけ）。足す・外すは
/// その場でサーバーへ送る（`PATCH /apps/installations/{app_id}/roles`）。

import * as React from "react";

import { TABLE_ROLES } from "@/components/data/table-share-dialog";
import { SHARE_DIALOG_CONTENT } from "@/components/share/share-dialog-layout";
import { SharePeoplePanel, type PeopleEntry, type PeopleTarget } from "@/components/share/share-people-panel";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { TableShareRole } from "@/lib/data-api";
import {
  getAppTableRoles,
  updateAppTableRoles,
  type AppTableAccess,
  type AppTableRolesChange,
} from "@/lib/miniapp-b1-api";
import { cachedRoleName, resolveRoleNames } from "@/lib/role-names";

/// 役割 → 要求の欄（インストールの要求と同じ名前）。
const FIELD: Record<TableShareRole, keyof AppTableAccess["roles"]> = {
  viewer: "viewer_roles",
  editor: "editor_roles",
  submitter: "submitter_roles",
};

/// 変えられない人に出す文（サーバーが 403 を返す）。
const OWNER_ONLY = {
  title: "変えられるのは、このアプリの持ち主だけです",
  body: "使える部署・グループを変えたいときは、持ち主に頼んでください。",
};

function entriesOf(access: AppTableAccess): PeopleEntry<TableShareRole>[] {
  return (Object.keys(FIELD) as TableShareRole[]).flatMap((role) =>
    (access.roles[FIELD[role]] ?? []).map((id) => ({
      target: { type: "role" as const, id },
      role,
      display_name: cachedRoleName(id) ?? null,
    })),
  );
}

/// サーバーの断り（本文に理由が無い）を、画面の言葉に直す。
function explain(e: unknown): Error {
  const status = (e as { status?: number } | null)?.status;
  if (status === 400) return new Error("部署・グループが見つからないか、正しくありません。");
  if (status === 403) return new Error(`${OWNER_ONLY.title}。`);
  if (status === 404) return new Error("このアプリはアンインストールされています。");
  return e instanceof Error ? e : new Error(String(e));
}

export function AppTableRolesDialog({
  open,
  onOpenChange,
  appId,
  appName,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  appId: string;
  appName: string;
}) {
  const [tables, setTables] = React.useState<string[] | null>(null);
  React.useEffect(() => {
    if (open) setTables(null);
  }, [open, appId]);

  const load = React.useCallback(async () => {
    const access = await getAppTableRoles(appId);
    setTables(access.tables.map((t) => t.name));
    const entries = entriesOf(access);
    // 一覧には表示名で出す（ディレクトリから引く。引けなければ id のまま）。
    if (await resolveRoleNames(entries.map((e) => e.target.id))) return entriesOf(access);
    return entries;
  }, [appId]);

  const change = React.useCallback(
    async (target: PeopleTarget, role: TableShareRole, op: keyof AppTableRolesChange) => {
      try {
        await updateAppTableRoles(appId, { [op]: { [FIELD[role]]: [target.id] } });
      } catch (e) {
        throw explain(e);
      }
    },
    [appId],
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className={SHARE_DIALOG_CONTENT} data-testid="app-table-roles-dialog">
        <DialogHeader className="mb-4">
          <DialogTitle className="leading-snug [overflow-wrap:anywhere]">
            「{appName}」のテーブルを使える部署・グループ
          </DialogTitle>
          <DialogDescription data-testid="app-table-roles-note">
            {tables && tables.length > 0
              ? `選んだ部署・グループの人が、このアプリのテーブル（${tables.map((n) => `「${n}」`).join("、")}）を使えるようになります。`
              : "選んだ部署・グループの人が、このアプリのテーブルを使えるようになります。"}
            外した部署・グループの人は、すぐに使えなくなります。見られるレコードと列は、開いた人の権限で決まります。
          </DialogDescription>
        </DialogHeader>
        <SharePeoplePanel<TableShareRole>
          open={open}
          resourceId={appId}
          load={load}
          grant={(target, role) => change(target, role, "add")}
          revoke={(target, role) => change(target, role, "remove")}
          roles={TABLE_ROLES}
          kinds={["role"]}
          ownerOnly={OWNER_ONLY}
        />
      </DialogContent>
    </Dialog>
  );
}
