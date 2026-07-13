"use client";

/// エディタ本体（3 ペイン: パレット / キャンバス / 設定・ヘッダに保存等の操作）。
///
/// - 保存は**明示**（PUT + expected_version・409 は競合通知→再読込）。autosave しない
///   （版が無限に増える＋検証必須のため）。dirty は離脱ガード。
/// - ライブ検証: IR 変更を 600ms debounce で `POST /workflows/validate` に流し、
///   エラーを該当ノード上へ表示（保存前に気づける）。
/// - レイアウト（座標）のみ 1s debounce で自動保存（安価・非バージョン）。

import * as React from "react";
import { ReactFlowProvider } from "@xyflow/react";
import {
  Blocks,
  CheckCircle2,
  CloudUpload,
  Loader2,
  Plus,
  Redo2,
  Undo2,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { toast } from "@/components/ui/use-toast";
import { cn } from "@/lib/utils";
import type { WorkflowIr } from "@/generated/workflow-ir";
import {
  putLayout,
  updateWorkflow,
  validateWorkflow,
  WorkflowValidationError,
  type EditorLayout,
} from "@/lib/workflow-api";
import { AddNodeMenu } from "./add-node-menu";
import { Canvas } from "./canvas";
import { useEditorState } from "./ir-state";
import { Palette } from "./palette";
import { FadeSlide } from "@/components/ui/motion-primitives";

type Props = {
  workflowId: string;
  initialIr: WorkflowIr;
  initialVersion: number;
  initialLayout: EditorLayout;
  /// 右パネル（設定）・ヘッダ右側の拡張スロット（後続 PR: 設定パネル/有効化/実行）。
  renderSidePanel?: (ctx: EditorContext) => React.ReactNode;
  renderHeaderActions?: (ctx: EditorContext) => React.ReactNode;
};

export type EditorContext = ReturnType<typeof useEditorState> extends [infer S, infer D]
  ? { state: S; dispatch: D; workflowId: string; save: () => Promise<void>; saving: boolean }
  : never;

export function WorkflowEditor({
  workflowId,
  initialIr,
  initialVersion,
  initialLayout,
  renderSidePanel,
  renderHeaderActions,
}: Props) {
  const [state, dispatch] = useEditorState({
    ir: initialIr,
    version: initialVersion,
    layout: initialLayout,
  });
  const [saving, setSaving] = React.useState(false);
  // ブロックパレットは既定で隠す（視界を奪わない）。トグルで表示、追加は主にノード尻尾の＋から。
  const [paletteOpen, setPaletteOpen] = React.useState(false);

  // ── ライブ検証（600ms debounce・保存しない）─────────────────────────────
  React.useEffect(() => {
    if (!state.dirty) return;
    const timer = setTimeout(() => {
      validateWorkflow(state.ir)
        .then((errors) => dispatch({ type: "set_errors", errors }))
        .catch(() => {
          // ライブ検証は best-effort（ネットワーク断で編集を妨げない）。保存時に必ず再検証される。
        });
    }, 600);
    return () => clearTimeout(timer);
  }, [state.ir, state.dirty, dispatch]);

  // ── レイアウト自動保存（1s debounce・非バージョン）───────────────────────
  React.useEffect(() => {
    if (!state.layoutDirty) return;
    // 送ったスナップショットを渡し、完了時に「まだ最新か」を reducer 側で照合する
    //（古い PUT の完了が新しいドラッグ分の保存をスキップさせない）。
    const snapshot = state.layout;
    const timer = setTimeout(() => {
      putLayout(workflowId, snapshot)
        .then(() => dispatch({ type: "layout_saved", layout: snapshot }))
        .catch(() => {
          // 座標は化粧品（次回 dagre で復元可能）。失敗は黙って次回に任せる。
        });
    }, 1000);
    return () => clearTimeout(timer);
  }, [state.layout, state.layoutDirty, workflowId, dispatch]);

  // ── 離脱ガード（未保存の IR 変更）────────────────────────────────────────
  React.useEffect(() => {
    if (!state.dirty) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [state.dirty]);

  const save = React.useCallback(async () => {
    // Ctrl/Cmd+S 経由でも「保存中・変更なし」では走らせない（毎 PUT が不変バージョンを
    // 追記するため、無変更保存は版の無駄な増殖・連打は自己競合になる）。
    if (saving || !state.dirty) return;
    setSaving(true);
    // 送るスナップショットを固定し、保存中に続いた編集を「保存済み」と誤表示しない。
    const irToSave = state.ir;
    try {
      const saved = await updateWorkflow(workflowId, irToSave, state.savedVersion);
      dispatch({ type: "saved", version: saved.version, savedIr: irToSave });
      dispatch({ type: "set_errors", errors: [] });
      toast({ title: `保存しました（v${saved.version}）` });
    } catch (e) {
      if (e instanceof WorkflowValidationError) {
        dispatch({ type: "set_errors", errors: e.errors });
        toast({
          variant: "destructive",
          title: "保存できません（検証エラー）",
          description: "赤いバッジのブロックを確認してください。",
        });
      } else if (e instanceof Error && e.message.includes("409")) {
        toast({
          variant: "destructive",
          title: "他の人が更新しました",
          description: "ページを再読み込みして最新版から編集し直してください。",
        });
      } else {
        toast({
          variant: "destructive",
          title: "保存に失敗しました",
          description: e instanceof Error ? e.message : String(e),
        });
      }
    } finally {
      setSaving(false);
    }
  }, [workflowId, state.ir, state.savedVersion, state.dirty, saving, dispatch]);

  // ── キーボード: Ctrl/Cmd+Z / Shift+Z / S ────────────────────────────────
  React.useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      const target = e.target as HTMLElement | null;
      if (target && /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)) return;
      if (e.key.toLowerCase() === "z") {
        e.preventDefault();
        dispatch({ type: e.shiftKey ? "redo" : "undo" });
      }
      if (e.key.toLowerCase() === "s") {
        e.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [dispatch, save]);

  const ctx = { state, dispatch, workflowId, save, saving } as EditorContext;
  const errorCount = state.serverErrors.length;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* ヘッダ（タイトル・保存状態・操作）。 */}
      <header className="flex shrink-0 items-center gap-3 border-b bg-background px-4 py-2.5">
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-sm font-semibold">
            {state.ir.display_name || state.ir.name}
          </h1>
          <p className="flex items-center gap-2 text-xs text-muted-foreground">
            <span>v{state.savedVersion}</span>
            {state.dirty ? (
              <span className="text-[color:var(--season-autumn)]">未保存の変更</span>
            ) : (
              <span className="flex items-center gap-1">
                <CheckCircle2 className="size-3" aria-hidden />
                保存済み
              </span>
            )}
            {errorCount > 0 ? (
              <span className="text-destructive">検証エラー {errorCount} 件</span>
            ) : null}
          </p>
        </div>
        <div className="flex items-center gap-1.5">
          <Button
            variant={paletteOpen ? "secondary" : "ghost"}
            size="sm"
            aria-pressed={paletteOpen}
            onClick={() => setPaletteOpen((p) => !p)}
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
            <Popover>
              <PopoverTrigger asChild>
                <Button variant="outline" size="sm">
                  <Plus className="size-4" aria-hidden />
                  最初のブロック
                </Button>
              </PopoverTrigger>
              <PopoverContent side="bottom" align="end" className="w-auto p-3">
                <AddNodeMenu
                  contextLabel="最初のブロックを追加"
                  onPick={(nodeType) =>
                    dispatch({ type: "add_node", nodeType, position: { x: 0, y: 0 } })
                  }
                />
              </PopoverContent>
            </Popover>
          ) : null}
          {renderHeaderActions?.(ctx)}
          <Button size="sm" onClick={() => void save()} disabled={saving || !state.dirty}>
            {saving ? (
              <Loader2 className="size-4 animate-spin" aria-hidden />
            ) : (
              <CloudUpload className="size-4" aria-hidden />
            )}
            保存
          </Button>
        </div>
      </header>

      {/* 本体。パレットは既定で隠し、トグルで「きっかけ」のように浮遊した角丸カードで重ねる
          （追加は主にノード尻尾の＋・最初のブロック）。 */}
      <div className="flex min-h-0 flex-1">
        <div className={cn("relative min-w-0 flex-1")}>
          <ReactFlowProvider>
            <Canvas
              ir={state.ir}
              layout={state.layout}
              selection={state.selection}
              serverErrors={state.serverErrors}
              dispatch={dispatch}
            />
          </ReactFlowProvider>
          {paletteOpen ? (
            <FadeSlide
              from="left"
              className="absolute inset-y-3 left-3 z-10 w-64"
            >
              <Palette />
            </FadeSlide>
          ) : null}
        </div>
        {renderSidePanel?.(ctx)}
      </div>
    </div>
  );
}
