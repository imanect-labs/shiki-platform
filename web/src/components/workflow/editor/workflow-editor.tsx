"use client";

/// エディタ本体（PC: パレット / キャンバス / 設定の 3 ペイン、スマホ: 縦のステップ一覧）。
///
/// - 保存は**明示**（PUT + expected_version・409 は競合通知→再読込）。autosave しない
///   （版が無限に増える＋検証必須のため）。dirty は離脱ガード。
/// - ライブ検証: IR 変更を 600ms debounce で `POST /workflows/validate` に流し、
///   エラーを該当ノード上へ表示（保存前に気づける）。
/// - レイアウト（座標）のみ 1s debounce で自動保存（安価・非バージョン）。
/// - 上部の操作（実行・保存・⋯）はシェルのページクロームへ `renderChrome` 側で登録する。
/// - スマホ（md 未満）はキャンバスの代わりに `MobileFlow`（縦のカード＋間の＋）を出し、
///   設定は下からのシートで開く。「キャンバスで開く」で従来のキャンバスにも切り替えられる。

import * as React from "react";
import { ReactFlowProvider } from "@xyflow/react";
import { ListTree, Workflow } from "lucide-react";

import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/use-toast";
import { FadeSlide } from "@/components/ui/motion-primitives";
import { useMediaQuery } from "@/hooks/use-media-query";
import type { WorkflowIr } from "@/generated/workflow-ir";
import {
  putLayout,
  updateWorkflow,
  validateWorkflow,
  WorkflowValidationError,
  type EditorLayout,
} from "@/lib/workflow-api";
import { AddNodeSheet } from "./add-node-sheet";
import { addAction, endTarget, type AddTarget } from "./add-target";
import { Canvas } from "./canvas";
import { useEditorState } from "./ir-state";
import { MobileFlow } from "./mobile-flow";
import { Palette } from "./palette";

type Props = {
  workflowId: string;
  initialIr: WorkflowIr;
  initialVersion: number;
  initialLayout: EditorLayout;
  /// 右パネル（設定）とページクローム（上部の操作・スマホの見出し・ダイアログ）の拡張スロット。
  renderSidePanel?: (ctx: EditorContext) => React.ReactNode;
  renderChrome?: (ctx: EditorChromeContext) => React.ReactNode;
};

export type EditorContext = ReturnType<typeof useEditorState> extends [infer S, infer D]
  ? {
      state: S;
      dispatch: D;
      workflowId: string;
      /// 保存する。保存できたら true（変更なし・失敗・保存中は false）。
      save: () => Promise<boolean>;
      saving: boolean;
    }
  : never;

/// クローム側に渡す、エディタ内の表示状態と操作。
export type EditorChromeContext = EditorContext & {
  /// md 以上（キャンバス・PC のバー）か。
  desktop: boolean;
  paletteOpen: boolean;
  togglePalette: () => void;
  /// 流れの末尾へブロックを追加する（下からのシートでブロックを選ぶ）。
  requestAddAtEnd: () => void;
};

export function WorkflowEditor({
  workflowId,
  initialIr,
  initialVersion,
  initialLayout,
  renderSidePanel,
  renderChrome,
}: Props) {
  const [state, dispatch] = useEditorState({
    ir: initialIr,
    version: initialVersion,
    layout: initialLayout,
  });
  const [saving, setSaving] = React.useState(false);
  // ブロックパレットは既定で隠す（視界を奪わない）。トグルで表示、追加は主にノード尻尾の＋から。
  const [paletteOpen, setPaletteOpen] = React.useState(false);
  const desktop = useMediaQuery("(min-width: 768px)");
  // スマホの表示（既定は縦の一覧。キャンバスは任意で開く）。
  const [mobileView, setMobileView] = React.useState<"list" | "canvas">("list");
  // ブロック選択シートの追加先（null で閉じる）。
  const [adding, setAdding] = React.useState<{ target: AddTarget; label: string } | null>(null);

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
    if (saving || !state.dirty) return false;
    setSaving(true);
    // 送るスナップショットを固定し、保存中に続いた編集を「保存済み」と誤表示しない。
    const irToSave = state.ir;
    try {
      const saved = await updateWorkflow(workflowId, irToSave, state.savedVersion);
      dispatch({ type: "saved", version: saved.version, savedIr: irToSave });
      dispatch({ type: "set_errors", errors: [] });
      toast({ title: `保存しました（v${saved.version}）` });
      return true;
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
      return false;
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

  const pick = React.useCallback(
    (target: AddTarget, nodeType: string) =>
      dispatch(addAction(target, nodeType, state.ir, state.layout)),
    [dispatch, state.ir, state.layout],
  );
  const requestAddAtEnd = React.useCallback(
    () => setAdding({ target: endTarget(state.ir), label: "流れの末尾に追加" }),
    [state.ir],
  );
  const togglePalette = React.useCallback(() => setPaletteOpen((p) => !p), []);
  const chromeCtx: EditorChromeContext = {
    ...ctx,
    desktop,
    paletteOpen,
    togglePalette,
    requestAddAtEnd,
  };
  const showCanvas = desktop || mobileView === "canvas";

  return (
    <div className="flex h-full min-h-0 flex-col">
      {renderChrome?.(chromeCtx)}
      {showCanvas ? (
        // 本体。パレットは既定で隠し、トグルで「きっかけ」のように浮遊した角丸カードで重ねる
        // （追加は主にノード尻尾の＋・最初のブロック）。
        <div className="flex min-h-0 flex-1">
          <div className="relative min-w-0 flex-1">
            <ReactFlowProvider>
              <Canvas
                ir={state.ir}
                layout={state.layout}
                selection={state.selection}
                serverErrors={state.serverErrors}
                dispatch={dispatch}
              />
            </ReactFlowProvider>
            {paletteOpen && desktop ? (
              <FadeSlide from="left" className="absolute inset-y-3 left-3 z-10 w-64">
                <Palette onPick={(nodeType) => pick(endTarget(state.ir), nodeType)} />
              </FadeSlide>
            ) : null}
            {!desktop ? (
              <div className="absolute left-1/2 top-3 z-10 -translate-x-1/2">
                <Button
                  variant="outline"
                  size="sm"
                  className="rounded-full bg-background/95 shadow-sm"
                  onClick={() => setMobileView("list")}
                >
                  <ListTree className="size-4" aria-hidden />
                  一覧で表示
                </Button>
              </div>
            ) : null}
          </div>
          {renderSidePanel?.(ctx)}
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain">
          <MobileFlow
            ir={state.ir}
            selection={state.selection}
            serverErrors={state.serverErrors}
            dispatch={dispatch}
            onAdd={(target, label) => setAdding({ target, label })}
          />
          <div className="flex flex-col items-center gap-1 px-4 pb-10 pt-2">
            <Button variant="ghost" size="sm" onClick={() => setMobileView("canvas")}>
              <Workflow className="size-4" aria-hidden />
              キャンバスで開く
            </Button>
            <p className="text-xs text-muted-foreground">
              自由な配置やつなぎ替えはキャンバスで行えます（PC 推奨）
            </p>
          </div>
          {renderSidePanel?.(ctx)}
        </div>
      )}
      <AddNodeSheet
        open={adding !== null}
        onOpenChange={(open) => !open && setAdding(null)}
        contextLabel={adding?.label ?? ""}
        onPick={(nodeType) => {
          if (adding) pick(adding.target, nodeType);
        }}
      />
    </div>
  );
}
