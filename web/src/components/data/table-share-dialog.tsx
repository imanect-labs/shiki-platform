"use client";

/// テーブルの共有ダイアログ（#864）。開けるのはテーブルの owner だけ（サーバも owner 以外を 403 にする）。
/// UX と部品（SharePeoplePanel）はアーティファクト・ドライブの共有と同じで、対象が
/// `/data/tables/{id}/shares` である点だけが異なる。スマホでは Dialog が下からのシートになる。
///
/// 共有はテーブル単位の権限（閲覧 = viewer・編集 = editor・追加だけ = submitter・D-113）で、行の絞り込み・
/// 読めない列は開いた人の権限で効く（共有しても owner と同じものが見えるわけではない）ことを説明に出す。
/// 追加だけの人は、レコードを追加でき、自分が追加したレコードだけ見られる（申請を社員に出させる形）。

import { CopyLinkButton } from "@/components/share/copy-link-button";
import { SHARE_DIALOG_CONTENT, ShareDialogFooter } from "@/components/share/share-dialog-layout";
import { SharePeoplePanel, type PeopleRoleOption } from "@/components/share/share-people-panel";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { listTableShares, shareTable, unshareTable, type TableShareRole } from "@/lib/data-api";

/// テーブルの共有で選べる権限（先頭が既定）。
const TABLE_ROLES: PeopleRoleOption<TableShareRole>[] = [
  { value: "viewer", label: "閲覧", hint: "レコードを見られます。追加や編集はできません。" },
  { value: "editor", label: "編集", hint: "レコードを追加・編集・削除できます。" },
  {
    value: "submitter",
    label: "追加だけ",
    hint: "レコードを追加できます。見られるのは、自分が追加したレコードと、個別に共有されたレコードです。",
  },
];

export function TableShareDialog({
  open,
  onOpenChange,
  tableId,
  name,
  shareUrl,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  tableId: string;
  name: string;
  /// コピーするリンク（テーブルの作業画面の URL）。
  shareUrl?: string;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className={SHARE_DIALOG_CONTENT}
        data-testid="table-share-dialog"
        // 開いたボタンはシェルのクローム（PC のバー・スマホの右上）に描かれ、開閉の間に描き直されるので、
        // Radix の既定（開く前の要素へ戻す）では body に落ちる。見えている「共有」ボタンへ戻す。
        onCloseAutoFocus={(e) => {
          const trigger = Array.from(
            document.querySelectorAll<HTMLElement>('[data-testid="table-share-open"]'),
          ).find((el) => el.offsetParent !== null);
          if (trigger) {
            e.preventDefault();
            trigger.focus();
          }
        }}
      >
        <DialogHeader className="mb-4">
          <DialogTitle className="leading-snug [overflow-wrap:anywhere]">「{name}」を共有</DialogTitle>
          <DialogDescription>
            同じ組織のメンバーや部署と、このテーブルを共有します。見られるレコードと列は、開いた人の権限で決まります。
          </DialogDescription>
        </DialogHeader>

        <SharePeoplePanel
          open={open}
          resourceId={tableId}
          load={() => listTableShares(tableId)}
          grant={(target, role) => shareTable(tableId, { target, role })}
          revoke={(target, role) => unshareTable(tableId, { target, role })}
          roles={TABLE_ROLES}
        />

        {shareUrl ? (
          <ShareDialogFooter>
            <span className="min-w-0 text-xs text-muted-foreground">すでに開ける人にリンクを渡す</span>
            <CopyLinkButton url={shareUrl} className="shrink-0" />
          </ShareDialogFooter>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
