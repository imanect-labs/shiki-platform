"use client";

import * as React from "react";
import { Download, FileUp } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { AD_GROUPS, sameMapping, type Mapping } from "./mock-directory-model";
import type { RoleCatalog } from "./directory-role-catalog";
import { CSV_TEMPLATE, importMapping, type ImportResult } from "./directory-import";

export function DirectoryImportDialog({ current, catalog, onImport, onClose }: {
  current: Mapping; catalog: RoleCatalog; onImport: (mapping: Mapping) => void; onClose: () => void;
}) {
  const [result, setResult] = React.useState<ImportResult | null>(null);
  const [filename, setFilename] = React.useState("");
  const [loading, setLoading] = React.useState(false);
  const generation = React.useRef(0);
  React.useEffect(() => () => { generation.current++; }, []);
  async function read(file?: File) {
    const id = ++generation.current;
    setResult(null); setFilename(file?.name ?? "");
    if (!file) { setLoading(false); return; }
    if (file.size > 1024 * 1024) {
      setLoading(false); setResult({ mapping: current, errors: ["ファイルは1MB以下にしてください。"], rows: 0 }); return;
    }
    setLoading(true);
    try {
      const text = await file.text();
      if (id === generation.current) setResult(importMapping(text, current, catalog.index));
    } catch {
      if (id === generation.current) setResult({ mapping: current, errors: ["ファイルを読み込めませんでした。"], rows: 0 });
    } finally { if (id === generation.current) setLoading(false); }
  }
  const changes = result ? AD_GROUPS.filter((g) => !sameMapping({ ...current, [g.id]: result.mapping[g.id] }, current)) : [];
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="sm:max-w-2xl">
        <DialogTitle>対応表をCSVからインポート</DialogTitle>
        <DialogDescription>既存のADグループとシキのロールをIDで対応付けます。階層・ユーザー・AD自体は変更しません。</DialogDescription>
        <div className="rounded-lg border bg-muted/30 p-4 text-sm">
          <p>列: <code>group_id,role_id</code></p>
          <p className="mt-2 text-xs leading-5 text-muted-foreground">ファイルにあるグループの対応付けを置き換えます。複数ロールは同じグループIDで行を追加、対応解除はロールIDを空欄にします。ファイルにないグループは保持します。</p>
          <Button className="mt-3" size="sm" variant="outline" onClick={() => {
            const url = URL.createObjectURL(new Blob([CSV_TEMPLATE], { type: "text/csv;charset=utf-8" }));
            const a = document.createElement("a"); a.href = url; a.download = "shiki-ad-mapping.csv"; a.click();
            setTimeout(() => URL.revokeObjectURL(url), 1000);
          }}><Download aria-hidden />現在のサンプルID入りテンプレート</Button>
        </div>
        <label className="flex cursor-pointer flex-col gap-3 rounded-lg border border-dashed p-5 text-sm">
          <span className="flex items-center gap-2 font-medium"><FileUp className="size-4" aria-hidden />CSVファイルを選択</span>
          <input type="file" accept=".csv,text/csv" aria-label="対応表CSV" onChange={(e) => void read(e.target.files?.[0])} className="max-w-full text-xs file:mr-3 file:rounded file:border file:bg-background file:px-3 file:py-2" />
          <span className="text-xs text-muted-foreground">UTF-8 / 1MB以下 / 1000行まで。端末内で読み込みます。</span>
        </label>
        <div aria-live="polite">
          {loading && <p className="text-sm">読み込み中…</p>}
          {result?.errors.length ? <div role="alert" className="rounded-lg border border-destructive/40 p-3 text-sm text-destructive"><p className="font-medium">取り込めません。{result.errors.length}件のエラー</p><ul className="mt-2 max-h-40 list-inside list-disc overflow-auto text-xs">{result.errors.map((error, i) => <li key={i}>{error}</li>)}</ul></div> : result && <div>
            <p className="text-sm font-medium">{filename} · {result.rows}行を確認 / {changes.length}グループを変更</p>
            <ul className="mt-3 max-h-56 divide-y overflow-y-auto rounded-lg border">{changes.map((g) => <li key={g.id} className="p-3 text-sm"><p className="font-medium">{g.name}</p><p className="mt-1 text-xs text-muted-foreground">{current[g.id].map(catalog.rolePathLabel).join("、") || "対応なし"} → {result.mapping[g.id].map(catalog.rolePathLabel).join("、") || "対応なし"}</p></li>)}</ul>
            {!changes.length && <p className="mt-2 text-sm text-muted-foreground">現在の編集内容と差分がありません。</p>}
          </div>}
        </div>
        <div className="flex flex-wrap justify-end gap-2"><Button variant="outline" onClick={onClose}>キャンセル</Button><Button disabled={loading || !result || !!result.errors.length || !changes.length} onClick={() => { if (result && !result.errors.length) onImport(result.mapping); }}>編集内容に取り込む</Button></div>
      </DialogContent>
    </Dialog>
  );
}
