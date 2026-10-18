"use client";

/// ワークフローエディタのページクローム（実行・保存・⋯）と、スマホ本文先頭の見出し。
///
/// - 右側: 主操作「▶ 実行」→ ⋯（スマホ: ブロックを追加・元に戻す・やり直す・実行履歴・版・
///   共有・自動実行・保存）。戻るは一覧へ。
/// - PC のバーには従来どおり、ブロック一覧・元に戻す/やり直す・状態バッジ・実行履歴・版・共有・
///   自動実行・保存を並べる（幅に余裕があるため ⋯ に畳まない）。
/// - スマホの本文先頭: 「✓ v1・保存済み」と名前、状態バッジ、「フロー｜実行履歴」タブ。
/// - 未保存のまま「実行」を押すと、保存してから実行ダイアログを開く（実行は保存済みの版で動く）。

import * as React from "react";
import { useRouter } from "next/navigation";
import {
  AlertCircle,
  AlertTriangle,
  Blocks,
  CheckCircle2,
  CloudUpload,
  History,
  Loader2,
  PencilLine,
  Play,
  Plus,
  Power,
  Redo2,
  RotateCcw,
  Share2,
  Undo2,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { ArtifactShareDialog } from "@/components/artifacts/share-dialog";
import { VersionsDialog } from "@/components/artifacts/versions-dialog";
import {
  ChromeIconButton,
  ChromePrimaryButton,
  usePageChrome,
  type ChromeMenuItem,
} from "@/components/shell/page-header-context";
import { cn } from "@/lib/utils";
import { getRegistration, type Registration } from "@/lib/workflow-api";
import type { EditorChromeContext } from "@/components/workflow/editor/workflow-editor";
import { AddNodeMenu } from "@/components/workflow/editor/add-node-menu";
import { EnableDialog } from "./enable-dialog";
import { RunNowDialog } from "./run-now-dialog";
import { WorkflowTabs } from "./workflow-tabs";

/// 保存状態（v1・保存済み など）。ドットではなくアイコン＋文字で示す。
function SaveState({
  version,
  dirty,
  saving,
  errorCount,
}: {
  version: number;
  dirty: boolean;
  saving: boolean;
  errorCount: number;
}) {
  return (
    <span className="inline-flex min-w-0 items-center gap-2">
      {saving ? (
        <span className="inline-flex items-center gap-1">
          <Loader2 className="size-3.5 animate-spin" aria-hidden />v{version}・保存中…
        </span>
      ) : dirty ? (
        <span className="inline-flex items-center gap-1 text-[color:var(--season-autumn)]">
          <PencilLine className="size-3.5" aria-hidden />v{version}・未保存
        </span>
      ) : (
        <span className="inline-flex items-center gap-1">
          <CheckCircle2 className="size-3.5" aria-hidden />v{version}・保存済み
        </span>
      )}
      {errorCount > 0 ? (
        <span className="inline-flex items-center gap-1 text-destructive">
          <AlertCircle className="size-3.5" aria-hidden />
          エラー {errorCount}
        </span>
      ) : null}
    </span>
  );
}

/// 自動実行の状態バッジ（有効・再同意が必要）。再同意はタップで設定を開く。
function RegistrationBadge({
  registration,
  onOpen,
  desktop,
}: {
  registration: Registration | null;
  onOpen: () => void;
  desktop: boolean;
}) {
  const status = registration?.status ?? "none";
  if (status === "suspended_reconsent") {
    const badge = (
      <button
        type="button"
        onClick={onOpen}
        className="rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <Badge variant="warning">
          <AlertTriangle className="size-3" aria-hidden />
          再同意が必要
        </Badge>
      </button>
    );
    // タッチでは Tooltip が読めないので、スマホは説明を文字で添える。
    if (!desktop) {
      return (
        <span className="inline-flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
          {badge}
          権限の変更で自動実行が停止しています
        </span>
      );
    }
    return (
      <Tooltip>
        <TooltipTrigger asChild>{badge}</TooltipTrigger>
        <TooltipContent>権限の変更で自動実行が停止しています。クリックして再同意</TooltipContent>
      </Tooltip>
    );
  }
  if (status === "enabled") {
    return <Badge variant="success">自動実行 有効（v{registration?.enabledVersion}）</Badge>;
  }
  return null;
}

export function WorkflowChrome({ ctx }: { ctx: EditorChromeContext }) {
  const router = useRouter();
  const [registration, setRegistration] = React.useState<Registration | null>(null);
  const [enableOpen, setEnableOpen] = React.useState(false);
  const [runOpen, setRunOpen] = React.useState(false);
  const [versionsOpen, setVersionsOpen] = React.useState(false);
  const [shareOpen, setShareOpen] = React.useState(false);
  const [firstOpen, setFirstOpen] = React.useState(false);

  const reload = React.useCallback(() => {
    getRegistration(ctx.workflowId)
      .then(setRegistration)
      .catch(() => setRegistration(null));
  }, [ctx.workflowId]);
  React.useEffect(reload, [reload]);

  const { state, dispatch, save, saving, desktop, workflowId } = ctx;
  const name = state.ir.display_name || state.ir.name;
  const errorCount = state.serverErrors.length;
  const status = registration?.status ?? "none";
  const hasAutomaticTrigger = state.ir.triggers.some(
    (t) => t.kind === "schedule" || t.kind === "event",
  );
  // 有効化済み/再同意待ちの間はトリガを消した編集中でも設定（停止導線）を出し続ける。
  const showEnable =
    hasAutomaticTrigger || status === "enabled" || status === "suspended_reconsent";
  const enableLabel = status === "enabled" ? "自動実行の設定" : "自動実行を有効化";
  const runsHref = `/workflows/${workflowId}/runs`;

  const run = React.useCallback(async () => {
    // 実行は保存済みの版で動く。未保存なら先に保存し、保存できたときだけ開く。
    if (state.dirty && !(await save())) return;
    setRunOpen(true);
  }, [state.dirty, save]);

  usePageChrome(
    () => {
      const menu: ChromeMenuItem[] = desktop
        ? []
        : [
            { key: "add", label: "ブロックを追加", icon: Plus, onSelect: ctx.requestAddAtEnd },
            {
              key: "undo",
              label: "元に戻す",
              icon: Undo2,
              disabled: state.undoStack.length === 0,
              onSelect: () => dispatch({ type: "undo" }),
            },
            {
              key: "redo",
              label: "やり直す",
              icon: Redo2,
              disabled: state.redoStack.length === 0,
              onSelect: () => dispatch({ type: "redo" }),
            },
            { key: "runs", label: "実行履歴", icon: History, href: runsHref, separatorBefore: true },
            {
              key: "versions",
              label: "バージョン履歴",
              icon: RotateCcw,
              onSelect: () => setVersionsOpen(true),
            },
            { key: "share", label: "共有", icon: Share2, onSelect: () => setShareOpen(true) },
            ...(showEnable
              ? [
                  {
                    key: "enable",
                    label: state.dirty ? `${enableLabel}（保存後に設定）` : enableLabel,
                    icon: Power,
                    disabled: state.dirty,
                    onSelect: () => setEnableOpen(true),
                  },
                ]
              : []),
            {
              key: "save",
              label: saving ? "保存中…" : state.dirty ? "保存" : "保存済み",
              icon: CloudUpload,
              disabled: saving || !state.dirty,
              onSelect: () => void save(),
              separatorBefore: true,
            },
          ];
      return {
        title: name,
        subtitle: (
          <SaveState
            version={state.savedVersion}
            dirty={state.dirty}
            saving={saving}
            errorCount={errorCount}
          />
        ),
        back: { href: "/workflows", label: "ワークフロー一覧へ" },
        menuTitle: name,
        primary: (
          <ChromePrimaryButton onClick={() => void run()} disabled={saving}>
            <Play aria-hidden />
            実行
          </ChromePrimaryButton>
        ),
        desktopExtra: (
          <div className="mr-1 flex items-center gap-1.5">
            <Button
              variant={ctx.paletteOpen ? "secondary" : "ghost"}
              size="sm"
              aria-pressed={ctx.paletteOpen}
              onClick={ctx.togglePalette}
              title="ブロック一覧の表示切替"
            >
              <Blocks className="size-4" aria-hidden />
              ブロック
            </Button>
            <Button
              variant="ghost"
              size="icon"
              aria-label="元に戻す"
              disabled={state.undoStack.length === 0}
              onClick={() => dispatch({ type: "undo" })}
            >
              <Undo2 className="size-4" aria-hidden />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              aria-label="やり直す"
              disabled={state.redoStack.length === 0}
              onClick={() => dispatch({ type: "redo" })}
            >
              <Redo2 className="size-4" aria-hidden />
            </Button>
            {state.ir.nodes.length === 0 ? (
              <Popover open={firstOpen} onOpenChange={setFirstOpen}>
                <PopoverTrigger asChild>
                  <Button variant="outline" size="sm">
                    <Plus className="size-4" aria-hidden />
                    最初のブロック
                  </Button>
                </PopoverTrigger>
                <PopoverContent side="bottom" align="end" className="w-auto p-3">
                  <AddNodeMenu
                    contextLabel="最初のブロックを追加"
                    onPick={(nodeType) => {
                      setFirstOpen(false);
                      dispatch({ type: "add_node", nodeType, position: { x: 0, y: 0 } });
                    }}
                  />
                </PopoverContent>
              </Popover>
            ) : null}
            {desktop ? (
              <RegistrationBadge
                registration={registration}
                onOpen={() => setEnableOpen(true)}
                desktop
              />
            ) : null}
            <ChromeIconButton label="実行履歴" onClick={() => router.push(runsHref)}>
              <History aria-hidden />
            </ChromeIconButton>
            <ChromeIconButton label="バージョン履歴" onClick={() => setVersionsOpen(true)}>
              <RotateCcw aria-hidden />
            </ChromeIconButton>
            {/* 共有: ワークフローは artifact（kind=workflow）なので artifact ReBAC（viewer/editor）に
                そのまま載る（新規 relation 不要・#334）。skills/apps と同じ ArtifactShareDialog を再利用。 */}
            <ChromeIconButton
              label="共有"
              onClick={() => setShareOpen(true)}
              data-testid="workflow-share"
            >
              <Share2 aria-hidden />
            </ChromeIconButton>
            {showEnable ? (
              <Button
                variant="outline"
                size="sm"
                onClick={() => setEnableOpen(true)}
                disabled={state.dirty}
                title={
                  state.dirty
                    ? "保存してから設定できます（同意は保存済みバージョンに適用されます）"
                    : undefined
                }
              >
                <Power className="size-4" aria-hidden />
                {enableLabel}
              </Button>
            ) : null}
            <Button
              size="sm"
              variant={state.dirty ? "default" : "outline"}
              onClick={() => void save()}
              disabled={saving || !state.dirty}
            >
              {saving ? (
                <Loader2 className="size-4 animate-spin" aria-hidden />
              ) : (
                <CloudUpload className="size-4" aria-hidden />
              )}
              保存
            </Button>
          </div>
        ),
        menu,
      };
    },
    [
      // save は state.ir を deps に持つため、IR が変わるたび identity が変わる。これを
      // クローム deps に入れることで、ノード params 等の編集でも保存/実行アクションが最新の
      // IR を掴む（stale なクロームで旧 IR を保存/実行する事故を防ぐ）。
      save,
      run,
      name,
      state.savedVersion,
      state.dirty,
      errorCount,
      state.undoStack.length,
      state.redoStack.length,
      state.ir.nodes.length,
      ctx.paletteOpen,
      ctx.togglePalette,
      ctx.requestAddAtEnd,
      saving,
      desktop,
      registration,
      showEnable,
      enableLabel,
      firstOpen,
      runsHref,
      router,
      dispatch,
    ],
  );

  return (
    <>
      {/* スマホ: タイトルバーが無いので、本文の先頭に保存状態・名前・切替タブを置く。 */}
      {!desktop ? (
        <div className="flex shrink-0 flex-col gap-3 px-4 pb-3 pt-1">
          <div className="flex min-w-0 flex-col gap-1">
            <div className="text-[12.5px] text-muted-foreground">
              <SaveState
                version={state.savedVersion}
                dirty={state.dirty}
                saving={saving}
                errorCount={errorCount}
              />
            </div>
            <h1 className="line-clamp-2 text-[26px] font-bold leading-tight tracking-tight">
              {name}
            </h1>
            <div className={cn(status === "none" && "hidden")}>
              <RegistrationBadge
                registration={registration}
                onOpen={() => setEnableOpen(true)}
                desktop={false}
              />
            </div>
          </div>
          <WorkflowTabs workflowId={workflowId} current="flow" />
        </div>
      ) : null}

      <EnableDialog
        open={enableOpen}
        onOpenChange={setEnableOpen}
        workflowId={workflowId}
        version={state.savedVersion}
        registration={registration}
        onChanged={reload}
      />
      <RunNowDialog
        open={runOpen}
        onOpenChange={setRunOpen}
        workflowId={workflowId}
        ir={state.ir}
        version={state.savedVersion}
      />
      <VersionsDialog
        open={versionsOpen}
        onOpenChange={setVersionsOpen}
        artifactId={workflowId}
        name={name}
        currentVersion={state.savedVersion}
      />
      <ArtifactShareDialog
        open={shareOpen}
        onOpenChange={setShareOpen}
        artifactId={workflowId}
        name={name}
        shareUrl={
          typeof window !== "undefined"
            ? `${window.location.origin}/workflows/${workflowId}`
            : undefined
        }
      />
    </>
  );
}
