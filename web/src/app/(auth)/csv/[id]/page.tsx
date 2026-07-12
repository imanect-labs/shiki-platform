"use client";

/// CSV エディタページ（Task 11P.8）。/csv/[id] でグリッド編集＋RO SQL 分析。
///
/// - グリッドは仮想化＋ページ取得で無限スクロール（全量ダウンロードしない）。
/// - 編集は明示保存（Cmd/Ctrl+S）で rev 付きパッチ→楽観ロック（409 は衝突ダイアログ）。
/// - SQL コンソール（RO・隔離 DuckDB 経由）を併設し、結果を「新規 CSV」として保存できる。

import { Loader2 } from "lucide-react";
import { useParams } from "next/navigation";
import * as React from "react";

import { CsvGrid, type CsvGridHandle } from "@/components/csv/csv-grid";
import { CsvHeaderSlot } from "@/components/csv/csv-header-slot";
import { SqlConsole } from "@/components/csv/sql-console";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { getCollabAccess, type CollabAccess } from "@/lib/notes-api";
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

  if (loaded === null) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        CSV を開いています…
      </div>
    );
  }
  if (loaded === "notfound") {
    return (
      <EmptyState
        title="CSV が見つかりません"
        description="削除されたか、アクセス権がありません。"
      />
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col p-3">
      <CsvHeaderSlot
        name={loaded.access.name}
        totalRows={loaded.schema.total_rows ?? 0}
        cols={loaded.schema.columns.length}
        editable={!!editable}
        tab={tab}
        onTabChange={setTab}
        dirty={dirty}
        saving={saving}
        onSave={save}
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
