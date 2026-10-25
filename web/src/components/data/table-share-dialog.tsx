"use client";

/// テーブルの共有ダイアログ（#864）。開けるのはテーブルの owner だけ（サーバも owner 以外を 403 にする）。
/// UX と部品（SharePeoplePanel）はアーティファクト・ドライブの共有と同じで、対象が
/// `/data/tables/{id}/shares` である点だけが異なる。スマホでは Dialog が下からのシートになる。
///
/// 共有はテーブル単位の権限（閲覧 = viewer・編集 = editor）で、行の絞り込み・読めない列は開いた人の
/// 権限で効く（共有しても owner と同じものが見えるわけではない）ことを説明に出す。

import { CopyLinkButton } from "@/components/share/copy-link-button";
import { SHARE_DIALOG_CONTENT, ShareDialogFooter } from "@/components/share/share-dialog-layout";
import { SharePeoplePanel } from "@/components/share/share-people-panel";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { listTableShares, shareTable, unshareTable } from "@/lib/data-api";

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
      <DialogContent className={SHARE_DIALOG_CONTENT} data-testid="table-share-dialog">
        <DialogHeader className="mb-4">
          <DialogTitle className="leading-snug [overflow-wrap:anywhere]">「{name}」を共有</DialogTitle>
          <DialogDescription>
            同じ組織のメンバー・部署にテーブルの閲覧・編集を許可します。行の絞り込みと読めない列は、開いた人の権限で効きます。
          </DialogDescription>
        </DialogHeader>

        <SharePeoplePanel
          open={open}
          resourceId={tableId}
          load={() => listTableShares(tableId)}
          grant={(target, role) => shareTable(tableId, { target, role })}
          revoke={(target, role) => unshareTable(tableId, { target, role })}
        />

        {shareUrl ? (
          <ShareDialogFooter>
            <span className="min-w-0 text-xs text-muted-foreground">アクセス権のある人へリンクを渡す</span>
            <CopyLinkButton url={shareUrl} className="shrink-0" />
          </ShareDialogFooter>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
