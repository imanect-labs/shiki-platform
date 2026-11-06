"use client";

/// ワークフローエディタのページクローム（実行・保存・⋯）と、スマホ本文先頭の見出し。
///
/// - 右側: 主操作「▶ 実行」→ ⋯（スマホ: ブロックを追加・入力項目・元に戻す・やり直す・実行履歴・版・
///   共有・自動実行・保存）。戻るは一覧へ。
/// - PC のバーには、ブロック一覧・入力項目・元に戻す/やり直す・状態バッジ・実行履歴・版・共有・自動実行・保存を並べる。
///   バーの幅（サイドバーを除いた幅）が足りなければ畳み、タイトルと保存の状態をボタンの下に隠さない（BUG-231）:
///   64rem 未満はブロックと入力項目を名前なしのアイコンに、56rem 未満は実行履歴・版・共有・自動実行を ⋯ に（状態バッジは
///   見送りと再同意だけを印で残す）、36rem 未満は元に戻す・やり直すも ⋯ に畳む。
/// - 「入力項目」はきっかけと別に入力項目の設定を開く（「手動で実行」のきっかけが無いワークフローでも開ける・GAP-35）。
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
  TextCursorInput,
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
  ChromeMenu,
  ChromePrimaryButton,
  usePageChrome,
  type ChromeMenuItem,
} from "@/components/shell/page-header-context";
import { useMe } from "@/hooks/use-me";
import { checkWorkflowAccess, getArtifact } from "@/lib/artifact-api";
import { cn } from "@/lib/utils";
import { getRegistration, type Registration } from "@/lib/workflow-api";
import type { EditorChromeContext } from "@/components/workflow/editor/workflow-editor";
import { AddNodeMenu } from "@/components/workflow/editor/add-node-menu";
import { mapErrors } from "@/components/workflow/editor/validation";
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

/// 自動実行の状態バッジ（有効・見送り中・再同意が必要）。見送り中と再同意はタップで設定を開く。
///
/// PC のバーが狭いときに畳むクラス（BUG-231）: `narrowClassName` は有効なだけのバッジに付け（⋯ の「自動実行の設定」で
/// 開けるので出さない）、`narrowTextClassName` は見送りと再同意のバッジの文に付ける（印だけを残し、文はツールチップと
/// 読み上げで言う）。`shortTextClassName` は有効なバッジの「自動実行」に付ける（隣の「自動実行の設定」と重なる語を
/// 省き、「有効（v1）」とだけ見せる。読み上げでは省かない）。
function RegistrationBadge({
  registration,
  onOpen,
  desktop,
  narrowClassName,
  narrowTextClassName,
  shortTextClassName,
}: {
  registration: Registration | null;
  onOpen: () => void;
  desktop: boolean;
  narrowClassName?: string;
  narrowTextClassName?: string;
  shortTextClassName?: string;
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
          <span className={narrowTextClassName}>再同意が必要</span>
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
  // オンのまま、スケジュールや変更での起動を見送り続けている（GAP-67）。タップで自動実行の設定を開き、スマホは理由と
  // 直し方を文字で添える（ツールチップはタッチで読めない）。
  const skip = registration?.launchSkip ?? null;
  if (status === "enabled" && skip) {
    const badge = (
      <button
        type="button"
        onClick={onOpen}
        data-testid="workflow-launch-skip"
        className="rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <Badge variant="warning">
          <AlertTriangle className="size-3" aria-hidden />
          <span className={narrowTextClassName}>自動実行を見送っています</span>
        </Badge>
      </button>
    );
    // 理由と直し方は、それぞれ折り返さない塊にして文の切れ目で折る（「自動実行の設定」の途中で割らない）。
    const reason = (
      <>
        <span className="inline-block">{skip.message}</span>
        <span className="inline-block">{skip.fix}</span>
      </>
    );
    if (!desktop) {
      return (
        <span className="flex flex-col items-start gap-1 text-xs leading-relaxed text-muted-foreground">
          {badge}
          <span data-testid="workflow-launch-skip-reason">{reason}</span>
        </span>
      );
    }
    return (
      <Tooltip>
        <TooltipTrigger asChild>{badge}</TooltipTrigger>
        <TooltipContent className="max-w-sm">{reason}</TooltipContent>
      </Tooltip>
    );
  }
  if (status === "enabled") {
    return (
      <Badge variant="success" className={narrowClassName}>
        <span className={shortTextClassName}>自動実行 </span>有効（v{registration?.enabledVersion}）
      </Badge>
    );
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

  // 共有（相手の一覧と付与・解除）は持ち主だけができるので、持ち主にだけ出す（編集の共有を受けた人には、相手の
  // 一覧が空に見え、付与が断られる・BUG-120）。持ち主が分かるまでは出さない。
  const me = useMe();
  const [owner, setOwner] = React.useState<string | null>(null);
  React.useEffect(() => {
    let active = true;
    setOwner(null);
    getArtifact(ctx.workflowId)
      .then((m) => active && setOwner(m.owner))
      .catch(() => active && setOwner(null));
    return () => {
      active = false;
    };
  }, [ctx.workflowId]);
  const canShare = owner !== null && owner === me.data?.id;

  const { state, dispatch, save, saving, desktop, workflowId } = ctx;
  const name = state.ir.display_name || state.ir.name;
  const errorCount = state.serverErrors.length;
  // 入力項目の設定を開いているか・入力項目に直すところがあるか（ボタンに印を付ける）。
  const inputsOpen = state.selection.kind === "inputs";
  const inputErrorCount = React.useMemo(
    () => mapErrors(state.serverErrors).inputSchema.length,
    [state.serverErrors],
  );
  const toggleInputs = React.useCallback(
    () => dispatch({ type: "select", selection: inputsOpen ? { kind: "none" } : { kind: "inputs" } }),
    [dispatch, inputsOpen],
  );
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
      const undoItem: ChromeMenuItem = {
        key: "undo",
        label: "元に戻す",
        icon: Undo2,
        disabled: state.undoStack.length === 0,
        onSelect: () => dispatch({ type: "undo" }),
      };
      const redoItem: ChromeMenuItem = {
        key: "redo",
        label: "やり直す",
        icon: Redo2,
        disabled: state.redoStack.length === 0,
        onSelect: () => dispatch({ type: "redo" }),
      };
      // 実行履歴・版・共有・自動実行（スマホの ⋯ と、幅の狭い PC のバーの ⋯ に同じものを出す）。
      const otherItems: ChromeMenuItem[] = [
        { key: "runs", label: "実行履歴", icon: History, href: runsHref, separatorBefore: true },
        {
          key: "versions",
          label: "バージョン履歴",
          icon: RotateCcw,
          onSelect: () => setVersionsOpen(true),
        },
        ...(canShare
          ? [{ key: "share", label: "共有", icon: Share2, onSelect: () => setShareOpen(true) }]
          : []),
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
      ];
      const menu: ChromeMenuItem[] = desktop
        ? []
        : [
            { key: "add", label: "ブロックを追加", icon: Plus, onSelect: ctx.requestAddAtEnd },
            {
              key: "inputs",
              label: inputErrorCount > 0 ? `入力項目（直すところ ${inputErrorCount} 件）` : "入力項目",
              icon: TextCursorInput,
              onSelect: () => dispatch({ type: "select", selection: { kind: "inputs" } }),
              testId: "workflow-inputs-menu",
            },
            undoItem,
            redoItem,
            ...otherItems,
            {
              key: "save",
              label: saving ? "保存中…" : state.dirty ? "保存" : "保存済み",
              icon: CloudUpload,
              disabled: saving || !state.dirty,
              onSelect: () => void save(),
              separatorBefore: true,
            },
          ];
      // 幅の狭い PC のバーで畳んだ操作（BUG-231）。先頭の区切りは、元に戻す・やり直すを畳むときだけ要る。
      const [firstOther, ...restOther] = otherItems;
      const barMore: ChromeMenuItem[] = firstOther ? [{ ...firstOther, separatorBefore: false }, ...restOther] : [];
      const barMoreNarrow: ChromeMenuItem[] = [undoItem, redoItem, ...otherItems];

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
            {/* バーの幅で畳む（ビューポートの幅でなく、サイドバーを除いた幅・BUG-231）。64rem 未満は、ブロックと入力項目の
                名前を隠してアイコンだけにする。 */}
            <Button
              variant={ctx.paletteOpen ? "secondary" : "ghost"}
              size="sm"
              aria-pressed={ctx.paletteOpen}
              aria-label="ブロック"
              onClick={ctx.togglePalette}
              title="ブロック一覧の表示切替"
              className="@max-[64rem]/chrome:px-2"
            >
              <Blocks className="size-4" aria-hidden />
              <span className="@max-[64rem]/chrome:hidden">ブロック</span>
            </Button>
            <Button
              variant={inputsOpen ? "secondary" : "ghost"}
              size="sm"
              aria-pressed={inputsOpen}
              onClick={toggleInputs}
              title="実行するときに受け取る値を決めます"
              data-testid="workflow-inputs"
              className={cn(
                "@max-[64rem]/chrome:px-2",
                inputErrorCount > 0 && "text-destructive hover:text-destructive",
              )}
            >
              {inputErrorCount > 0 ? (
                <AlertCircle className="size-4" aria-hidden />
              ) : (
                <TextCursorInput className="size-4" aria-hidden />
              )}
              <span className="@max-[64rem]/chrome:sr-only">入力項目</span>
              {inputErrorCount > 0 ? <span className="sr-only">（直すところ {inputErrorCount} 件）</span> : null}
            </Button>
            {/* 36rem 未満は、元に戻す・やり直すも ⋯ に畳む（キーボードの Ctrl+Z でも戻せる）。 */}
            <Button
              variant="ghost"
              size="icon"
              aria-label="元に戻す"
              disabled={state.undoStack.length === 0}
              onClick={() => dispatch({ type: "undo" })}
              className="@max-[36rem]/chrome:hidden"
            >
              <Undo2 className="size-4" aria-hidden />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              aria-label="やり直す"
              disabled={state.redoStack.length === 0}
              onClick={() => dispatch({ type: "redo" })}
              className="@max-[36rem]/chrome:hidden"
            >
              <Redo2 className="size-4" aria-hidden />
            </Button>
            {state.ir.nodes.length === 0 ? (
              <Popover open={firstOpen} onOpenChange={setFirstOpen}>
                <PopoverTrigger asChild>
                  <Button variant="outline" size="sm" className="@max-[36rem]/chrome:px-2">
                    <Plus className="size-4" aria-hidden />
                    <span className="@max-[36rem]/chrome:sr-only">最初のブロック</span>
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
              // 64rem 未満は有効なバッジを「有効（v1）」と短くし、56rem 未満は見送りと再同意のバッジだけを印で残し（文は
              // ツールチップと読み上げ）、有効なだけのバッジは出さない。
              <RegistrationBadge
                registration={registration}
                onOpen={() => setEnableOpen(true)}
                desktop
                narrowClassName="@max-[56rem]/chrome:hidden"
                narrowTextClassName="@max-[56rem]/chrome:sr-only"
                shortTextClassName="@max-[64rem]/chrome:sr-only"
              />
            ) : null}
            {/* 56rem 未満は、実行履歴・バージョン履歴・共有・自動実行を ⋯ に畳む。 */}
            <ChromeIconButton
              label="実行履歴"
              onClick={() => router.push(runsHref)}
              className="@max-[56rem]/chrome:hidden"
            >
              <History aria-hidden />
            </ChromeIconButton>
            <ChromeIconButton
              label="バージョン履歴"
              onClick={() => setVersionsOpen(true)}
              className="@max-[56rem]/chrome:hidden"
            >
              <RotateCcw aria-hidden />
            </ChromeIconButton>
            {/* 共有: ワークフローは artifact（kind=workflow）なので artifact ReBAC（viewer/editor）に
                そのまま載る（新規 relation 不要・#334）。skills/apps と同じ ArtifactShareDialog を再利用。
                持ち主にだけ出す（BUG-120）。 */}
            {canShare ? (
              <ChromeIconButton
                label="共有"
                onClick={() => setShareOpen(true)}
                data-testid="workflow-share"
                className="@max-[56rem]/chrome:hidden"
              >
                <Share2 aria-hidden />
              </ChromeIconButton>
            ) : null}
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
                className="@max-[56rem]/chrome:hidden"
              >
                <Power className="size-4" aria-hidden />
                {enableLabel}
              </Button>
            ) : null}
            {desktop ? (
              <>
                <span className="hidden @min-[36rem]/chrome:@max-[56rem]/chrome:contents">
                  <ChromeMenu items={barMore} title={name} testId="workflow-bar-more" />
                </span>
                <span className="hidden @max-[36rem]/chrome:contents">
                  <ChromeMenu items={barMoreNarrow} title={name} testId="workflow-bar-more-narrow" />
                </span>
              </>
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
      inputsOpen,
      inputErrorCount,
      toggleInputs,
      saving,
      desktop,
      registration,
      showEnable,
      enableLabel,
      firstOpen,
      runsHref,
      router,
      dispatch,
      canShare,
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
        open={shareOpen && canShare}
        onOpenChange={setShareOpen}
        artifactId={workflowId}
        name={name}
        access={{ load: () => checkWorkflowAccess(workflowId), subject: "workflow" }}
        shareUrl={
          typeof window !== "undefined"
            ? `${window.location.origin}/workflows/${workflowId}`
            : undefined
        }
      />
    </>
  );
}
