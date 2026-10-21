"use client";

/// CSV エディタページ（Task 11P.8）。/csv/[id] でグリッド編集＋RO SQL 分析。
///
/// - グリッドは仮想化＋ページ取得で無限スクロール（全量ダウンロードしない）。
/// - 編集は明示保存（Cmd/Ctrl+S）で rev 付きパッチ→楽観ロック（409 は衝突ダイアログ）。
/// - SQL コンソール（RO・隔離 DuckDB 経由）を併設し、結果を「新規 CSV」として保存できる。

import { Eye, FileWarning } from "lucide-react";
import { useParams, useRouter } from "next/navigation";
import * as React from "react";

import { CsvGrid, type CsvGridHandle } from "@/components/csv/csv-grid";
import { useCsvChrome } from "@/components/csv/csv-header-slot";
import { SqlConsole } from "@/components/csv/sql-console";
import {
  EditorLoading,
  EditorMobileHeading,
  EditorStatusBadge,
  EditorUnavailable,
} from "@/components/shell/editor-loading";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ShareLinkUnlock, unlockTokenFromUrl } from "@/components/share/share-link-unlock";
import { toast } from "@/components/ui/use-toast";
import { setPendingSelection } from "@/lib/selection-context";
import { getCollabAccess, type CollabAccess } from "@/lib/notes-api";
import { downloadUrl } from "@/lib/storage";
import { applyPatch, getSchema, TabularConflict, type SchemaResponse } from "@/lib/tabular-api";

type Loaded = {
  access: CollabAccess;
  schema: SchemaResponse;
};

export default function CsvPage() {
  const params = useParams<{ id: string }>();
  const nodeId = params.id;
  const [loaded, setLoaded] = React.useState<Loaded | null | "notfound">(null);
  const [baseRev, setBaseRev] = React.useState(0);
  const [dirty, setDirty] = React.useState(false);
  const [saving, setSaving] = React.useState(false);
  const [conflict, setConflict] = React.useState(false);
  const [tab, setTab] = React.useState<"grid" | "sql">("grid");
  const router = useRouter();
  const gridRef = React.useRef<CsvGridHandle>(null);

  const load = React.useCallback(async () => {
    try {
      const access = await getCollabAccess(nodeId);
      if (!access) {
        setLoaded("notfound");
        return;
      }
      const schema = await getSchema(nodeId);
      setLoaded({ access, schema });
      setBaseRev(access.version);
    } catch {
      setLoaded("notfound");
    }
  }, [nodeId]);

  React.useEffect(() => {
    void load();
  }, [load]);

  const editable = loaded && loaded !== "notfound" && loaded.access.mode === "editor";

  const save = React.useCallback(async () => {
    if (!gridRef.current?.hasPatches() || saving) return;
    setSaving(true);
    const ops = gridRef.current.takePatches();
    try {
      const res = await applyPatch(nodeId, baseRev, ops);
      setBaseRev(res.version);
      gridRef.current.reset();
      toast({ title: "保存しました", description: `v${res.version} を作成しました。` });
    } catch (e) {
      if (e instanceof TabularConflict) {
        setConflict(true);
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
  }, [nodeId, baseRev, saving]);

  // Cmd/Ctrl+S で保存。
  React.useEffect(() => {
    if (!editable) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editable, save]);

  const reloadAfterConflict = async () => {
    setConflict(false);
    setLoaded(null);
    await load();
    gridRef.current?.reset();
  };

  // 保存済みの最新版をダウンロードする（未保存の編集は含まない）。
  const download = React.useCallback(() => {
    downloadUrl(nodeId)
      .then(({ url }) => {
        window.location.href = url;
      })
      .catch(() => toast({ variant: "destructive", description: "ダウンロードできませんでした。" }));
  }, [nodeId]);

  const ready = loaded !== null && loaded !== "notfound" ? loaded : null;
  // クロームはページ最上位で登録する（読み込み中・notfound でも戻る導線を残す）。
  useCsvChrome(
    ready
      ? {
          name: ready.access.name,
          totalRows: ready.schema.total_rows ?? 0,
          cols: ready.schema.columns.length,
          editable: !!editable,
          tab,
          onTabChange: setTab,
          dirty,
          saving,
          onSave: save,
          onDownload: download,
        }
      : null,
  );

  if (loaded === null) {
    return <EditorLoading kind="sheet" message="CSV を開いています…" />;
  }
  if (loaded === "notfound") {
    return (
      <EditorUnavailable
        icon={FileWarning}
        title="CSV が見つかりません"
        description="削除されたか、アクセス権がありません。パスワード付き共有リンクの場合はパスワードで開けます。"
      >
        {unlockTokenFromUrl() ? (
          <ShareLinkUnlock
            token={unlockTokenFromUrl()!}
            onUnlocked={() => void load()}
            autoFocus
          />
        ) : null}
      </EditorUnavailable>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col p-3 max-md:px-2 max-md:pb-[max(0.5rem,env(safe-area-inset-bottom))] max-md:pt-0">
      {/* スマホ: 文書名＋状態（PC はバーに出す）。 */}
      <EditorMobileHeading
        className="px-2"
        name={loaded.access.name}
        status={
          <>
            <span className="text-[12px] tabular-nums text-muted-foreground">
              {loaded.schema.total_rows ?? 0} 行・{loaded.schema.columns.length} 列
            </span>
            {!editable ? <EditorStatusBadge icon={Eye}>閲覧のみ</EditorStatusBadge> : null}
            {dirty ? <EditorStatusBadge tone="draft">未保存の編集あり</EditorStatusBadge> : null}
          </>
        }
      />

      <div className="min-h-0 flex-1">
        {tab === "grid" ? (
          <CsvGrid
            ref={gridRef}
            nodeId={nodeId}
            columns={loaded.schema.columns}
            totalRows={loaded.schema.total_rows ?? 0}
            editable={!!editable}
            onDirtyChange={setDirty}
            // 選択→AI 指示（Task 11.10）: チップ化してチャットへ（CSV ページは
            // アシスタントパネルを持たないため、ホームのチャットに引き継ぐ）。
            onAskAi={({ excerpt, rows, cols }) => {
              setPendingSelection({
                kind: "csv_range",
                node_id: nodeId,
                excerpt,
                locator: { rows, cols },
              });
              router.push("/");
            }}
          />
        ) : (
          <SqlConsole nodeId={nodeId} parentId={null} />
        )}
      </div>

      <Dialog open={conflict} onOpenChange={(o) => !o && setConflict(false)}>
        <DialogContent data-testid="csv-conflict-dialog">
          <DialogHeader>
            <DialogTitle>他の編集で更新されました</DialogTitle>
            <DialogDescription>
              この CSV は別の保存で更新されています。最新版を再読み込みします（未保存の編集は破棄されます）。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button onClick={() => void reloadAfterConflict()} data-testid="csv-conflict-reload">
              再読み込み
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
