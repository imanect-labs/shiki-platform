"use client";

/// エディタヘッダの操作（実行・自動実行の有効化・実行履歴・バージョン）。

import * as React from "react";
import { useRouter } from "next/navigation";
import { AlertTriangle, History, Play, Power, RotateCcw, Share2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { ArtifactShareDialog } from "@/components/artifacts/share-dialog";
import { VersionsDialog } from "@/components/artifacts/versions-dialog";
import { getRegistration, type Registration } from "@/lib/workflow-api";
import type { EditorContext } from "@/components/workflow/editor/workflow-editor";
import { EnableDialog } from "./enable-dialog";
import { RunNowDialog } from "./run-now-dialog";

export function WorkflowHeaderActions({ ctx }: { ctx: EditorContext }) {
  const router = useRouter();
  const [registration, setRegistration] = React.useState<Registration | null>(null);
  const [enableOpen, setEnableOpen] = React.useState(false);
  const [runOpen, setRunOpen] = React.useState(false);
  const [versionsOpen, setVersionsOpen] = React.useState(false);
  const [shareOpen, setShareOpen] = React.useState(false);

  const reload = React.useCallback(() => {
    getRegistration(ctx.workflowId)
      .then(setRegistration)
      .catch(() => setRegistration(null));
  }, [ctx.workflowId]);
  React.useEffect(reload, [reload]);

  const hasAutomaticTrigger = ctx.state.ir.triggers.some(
    (t) => t.kind === "schedule" || t.kind === "event",
  );
  const status = registration?.status ?? "none";

  return (
    <>
      {status === "suspended_reconsent" ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              onClick={() => setEnableOpen(true)}
              className="focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-full"
            >
              <Badge variant="warning">
                <AlertTriangle className="size-3" aria-hidden />
                再同意が必要
              </Badge>
            </button>
          </TooltipTrigger>
          <TooltipContent>権限の変更で自動実行が停止しています。クリックして再同意</TooltipContent>
        </Tooltip>
      ) : status === "enabled" ? (
        <Badge variant="success">自動実行 有効（v{registration?.enabledVersion}）</Badge>
      ) : null}

      <Button
        variant="ghost"
        size="icon"
        aria-label="実行履歴"
        onClick={() => router.push(`/workflows/${ctx.workflowId}/runs`)}
      >
        <History className="size-4" aria-hidden />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        aria-label="バージョン履歴"
        onClick={() => setVersionsOpen(true)}
      >
        <RotateCcw className="size-4" aria-hidden />
      </Button>
      {/* 共有: ワークフローは artifact（kind=workflow）なので artifact ReBAC（viewer/editor）に
          そのまま載る（新規 relation 不要・#334）。skills/apps と同じ ArtifactShareDialog を再利用。 */}
      <Button
        variant="ghost"
        size="icon"
        aria-label="共有"
        onClick={() => setShareOpen(true)}
        data-testid="workflow-share"
      >
        <Share2 className="size-4" aria-hidden />
      </Button>
      {/* 有効化済み/再同意待ちの間はトリガを消した編集中でも設定（停止導線）を出し続ける */}
      {hasAutomaticTrigger || status === "enabled" || status === "suspended_reconsent" ? (
        <Button
          variant="outline"
          size="sm"
          onClick={() => setEnableOpen(true)}
          disabled={ctx.state.dirty}
          title={ctx.state.dirty ? "保存してから設定できます（同意は保存済みバージョンに適用されます）" : undefined}
        >
          <Power className="size-4" aria-hidden />
          {status === "enabled" ? "自動実行の設定" : "自動実行を有効化"}
        </Button>
      ) : null}
      <Button
        variant="outline"
        size="sm"
        onClick={() => setRunOpen(true)}
        disabled={ctx.state.dirty}
        title={ctx.state.dirty ? "保存してから実行できます" : undefined}
      >
        <Play className="size-4" aria-hidden />
        実行
      </Button>

      <EnableDialog
        open={enableOpen}
        onOpenChange={setEnableOpen}
        workflowId={ctx.workflowId}
        version={ctx.state.savedVersion}
        registration={registration}
        onChanged={reload}
      />
      <RunNowDialog
        open={runOpen}
        onOpenChange={setRunOpen}
        workflowId={ctx.workflowId}
        ir={ctx.state.ir}
        version={ctx.state.savedVersion}
      />
      <VersionsDialog
        open={versionsOpen}
        onOpenChange={setVersionsOpen}
        artifactId={ctx.workflowId}
        name={ctx.state.ir.display_name || ctx.state.ir.name}
        currentVersion={ctx.state.savedVersion}
      />
      <ArtifactShareDialog
        open={shareOpen}
        onOpenChange={setShareOpen}
        artifactId={ctx.workflowId}
        name={ctx.state.ir.display_name || ctx.state.ir.name}
      />
    </>
  );
}
