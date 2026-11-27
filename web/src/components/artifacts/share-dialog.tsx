"use client";

/// アーティファクト（skill / ミニアプリ）の共有ダイアログ（Task 6.11）。
/// UX は Drive の ShareDialog の「メンバー・部署」と同一（検索 → 相手ごとに閲覧/編集を選んで付与・
/// 解除・変更）で、部品（SharePeoplePanel）も共有する。対象が artifact（/artifacts/{id}/shares）で
/// ある点だけが異なる。スマホでは Dialog が下からのシートになる。

import * as React from "react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  listArtifactShares,
  shareArtifact,
  unshareArtifact,
} from "@/lib/artifact-api";
import type { AccessReport } from "@/lib/artifact-api";
import { AccessReportView } from "@/components/share/access-report";
import { CopyLinkButton } from "@/components/share/copy-link-button";
import { SharePeoplePanel } from "@/components/share/share-people-panel";
import { SHARE_DIALOG_CONTENT, ShareDialogFooter } from "@/components/share/share-dialog-layout";

export function ArtifactShareDialog({
  open,
  onOpenChange,
  artifactId,
  name,
  shareUrl,
  note,
  access,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  artifactId: string | null;
  name: string;
  /// コピーするリンク（そのアーティファクトのページ URL）。省略時はリンクコピーを出さない
  /// （一覧ページから開いた場合に誤ったリンクを渡さないため）。
  shareUrl?: string;
  /// 共有の前に知らせること（まだ公開していないアプリなど）。
  note?: string;
  /// 使う人の権限の確かめ（アプリ・ワークフロー・GAP-37）。共有している相手ごとに、足りない権限と直し方を出す。
  access?: { load: () => Promise<AccessReport>; subject: "app" | "workflow" };
}) {
  // 相手を足した・外したら、使う人の権限を確かめ直す。
  const [changes, setChanges] = React.useState(0);
  if (!artifactId) return null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className={SHARE_DIALOG_CONTENT}>
        <DialogHeader className="mb-4">
          <DialogTitle className="leading-snug [overflow-wrap:anywhere]">「{name}」を共有</DialogTitle>
          <DialogDescription>
            同じ組織のメンバー・部署に権限を付与します。部署に共有するとそのメンバー全員に反映されます。
          </DialogDescription>
        </DialogHeader>

        {note ? (
          <p
            role="note"
            data-testid="share-dialog-note"
            className="mb-3 rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-700 dark:text-amber-400"
          >
            {note}
          </p>
        ) : null}

        <SharePeoplePanel
          open={open}
          resourceId={artifactId}
          load={() => listArtifactShares(artifactId)}
          grant={(target, role) => shareArtifact(artifactId, target, role)}
          revoke={(target, role) => unshareArtifact(artifactId, target, role)}
          onChanged={() => setChanges((n) => n + 1)}
        />

        {access && open ? (
          <AccessReportView load={access.load} reloadKey={changes} mode="share" subject={access.subject} />
        ) : null}

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
