"use client";

/// アーティファクトのバージョン履歴ダイアログ（Task 6.11・不変版のメタ一覧）。

import * as React from "react";
import { Loader2 } from "lucide-react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { ListLoadFailed } from "@/components/artifacts/shared-more";
import { listArtifactVersions, type VersionMeta } from "@/lib/artifact-api";

export function VersionsDialog({
  open,
  onOpenChange,
  artifactId,
  name,
  onSelectVersion,
  currentVersion,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  artifactId: string | null;
  name: string;
  /// 指定時は「この版を表示」ボタンを出す（ミニアプリの版切替）。
  onSelectVersion?: (version: number) => void;
  currentVersion?: number;
}) {
  const [versions, setVersions] = React.useState<VersionMeta[] | null>(null);
  // 読み込みの失敗を「履歴がありません」に見せない（付録 C の P5・BUG-189）。
  const [failed, setFailed] = React.useState(false);
  const [attempt, setAttempt] = React.useState(0);

  React.useEffect(() => {
    if (!open || !artifactId) return;
    let active = true;
    setVersions(null);
    setFailed(false);
    listArtifactVersions(artifactId)
      .then((v) => active && setVersions(v))
      .catch(() => {
        if (!active) return;
        setVersions([]);
        setFailed(true);
      });
    return () => {
      active = false;
    };
  }, [open, artifactId, attempt]);

  if (!artifactId) return null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>「{name}」のバージョン履歴</DialogTitle>
          <DialogDescription>すべての版は不変で保存されています。</DialogDescription>
        </DialogHeader>
        {versions === null ? (
          <div className="flex items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            読み込み中…
          </div>
        ) : failed ? (
          <ListLoadFailed what="バージョン履歴" className="py-8" onRetry={() => setAttempt((n) => n + 1)} />
        ) : versions.length === 0 ? (
          <p className="py-6 text-center text-sm text-muted-foreground">履歴がありません。</p>
        ) : (
          <ul className="max-h-72 divide-y divide-border overflow-y-auto rounded-lg border border-border">
            {versions.map((v) => (
              <li
                key={v.version}
                className="flex items-center gap-3 px-3 py-2.5"
                data-testid="version-row"
              >
                <span
                  className="rounded bg-secondary px-2 py-0.5 text-xs font-semibold text-secondary-foreground"
                  data-testid="version-label"
                >
                  v{v.version}
                </span>
                <div className="min-w-0 flex-1 text-xs text-muted-foreground">
                  {new Date(v.createdAt).toLocaleString("ja-JP")}・{v.createdBy}
                </div>
                {currentVersion === v.version ? (
                  <span className="text-xs font-medium text-primary">表示中</span>
                ) : onSelectVersion ? (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => {
                      onSelectVersion(v.version);
                      onOpenChange(false);
                    }}
                  >
                    この版を表示
                  </Button>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </DialogContent>
    </Dialog>
  );
}
