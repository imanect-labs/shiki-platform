"use client";

/// CSV エディタページ（Task 11P.8）。/csv/[id] でグリッド編集＋RO SQL 分析。
///
/// - グリッドは仮想化＋ページ取得で無限スクロール（全量ダウンロードしない）。
/// - 編集は明示保存（Cmd/Ctrl+S）で rev 付きパッチ→楽観ロック（409 は衝突ダイアログ）。
/// - SQL コンソール（RO・隔離 DuckDB 経由）を併設し、結果を「新規 CSV」として保存できる。

import { ArrowLeft, Eye, Loader2, Save, Table2, Terminal } from "lucide-react";
import Link from "next/link";
import { useParams } from "next/navigation";
import * as React from "react";

import { CsvGrid, type CsvGridHandle } from "@/components/csv/csv-grid";
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
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex items-center gap-3 border-b px-4 py-2">
        <Link
          href="/drive"
          className="flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          aria-label="ドライブへ戻る"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <span className="min-w-0 flex-1 truncate text-sm text-muted-foreground">
          {loaded.access.name}
        </span>
        <span className="text-xs text-muted-foreground tabular-nums">
          {loaded.schema.total_rows ?? 0} 行 × {loaded.schema.columns.length} 列
        </span>
        {!editable && (
          <span
            className="inline-flex items-center gap-1 rounded-full border bg-muted/50 px-2.5 py-0.5 text-xs font-medium text-muted-foreground"
            data-testid="csv-readonly-badge"
          >
            <Eye className="size-3.5" aria-hidden />
            閲覧のみ
          </span>
        )}
        {/* タブ切替（グリッド / SQL） */}
        <div className="flex items-center rounded-lg border p-0.5">
          <button
            type="button"
            onClick={() => setTab("grid")}
            data-testid="csv-tab-grid"
            className={`inline-flex items-center gap-1 rounded-md px-2.5 py-1 text-xs font-medium ${tab === "grid" ? "bg-accent text-accent-foreground" : "text-muted-foreground"}`}
          >
            <Table2 className="size-3.5" aria-hidden />
            グリッド
          </button>
          <button
            type="button"
            onClick={() => setTab("sql")}
            data-testid="csv-tab-sql"
            className={`inline-flex items-center gap-1 rounded-md px-2.5 py-1 text-xs font-medium ${tab === "sql" ? "bg-accent text-accent-foreground" : "text-muted-foreground"}`}
          >
            <Terminal className="size-3.5" aria-hidden />
            SQL
          </button>
        </div>
        {editable && (
          <Button
            type="button"
            size="sm"
            onClick={() => void save()}
            disabled={!dirty || saving}
            data-testid="csv-save"
          >
            {saving ? <Loader2 className="mr-1 size-4 animate-spin" /> : <Save className="mr-1 size-4" />}
            保存{dirty ? "*" : ""}
          </Button>
        )}
      </header>

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
