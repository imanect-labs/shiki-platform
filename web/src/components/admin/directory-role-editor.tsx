"use client";

import * as React from "react";
import { Search } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { IconInput } from "@/components/ui/icon-input";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import type { CatalogRole, RoleCatalog } from "./directory-role-catalog";

export function DirectoryRoleEditor({ role, defaultParent, initialName, catalog, groupName, onSave, onClose }: {
  role?: CatalogRole; defaultParent: string | null; initialName?: string; catalog: RoleCatalog; groupName: string;
  onSave: (role: CatalogRole, assign: boolean) => void; onClose: () => void;
}) {
  const [name, setName] = React.useState(role?.label ?? initialName ?? "");
  const [parent, setParent] = React.useState(role?.parent ?? defaultParent);
  const [picking, setPicking] = React.useState(false);
  const [query, setQuery] = React.useState("");
  const [page, setPage] = React.useState(0);
  const [assign, setAssign] = React.useState(!role);
  const [error, setError] = React.useState("");
  const id = React.useRef(role?.id ?? `custom-${crypto.randomUUID()}`).current;
  const candidates = React.useMemo(() => catalog.query({ query, parent: null, all: true, excludeBranch: role?.id, selected: [], selectedOnly: false, page }), [catalog, query, role?.id, page]);
  const changed = !role || name.trim() !== role.label || parent !== role.parent;
  const save = () => {
    try {
      const value = { id, label: name.trim(), parent, members: role?.members ?? 0 };
      catalog.save(value); // 同名・循環・親の検証。保存は親の作業状態へまとめて渡す。
      onSave(value, assign);
    } catch (e) { setError((e as Error).message); }
  };
  const affected = React.useMemo(() => role ? catalog.roles.filter((r) => catalog.rolePath(r.id).some((part) => part.id === role.id)).length - 1 : 0, [catalog, role]);
  return <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}><DialogContent className="max-w-xl">
    <DialogTitle>{role ? "ロールを編集" : "ロールを追加"}</DialogTitle>
    <DialogDescription>{role ? "名前と親ロールをこの画面で変更できます。" : `ロールを作成し、そのまま${groupName}に対応付けられます。`}</DialogDescription>
    <form onSubmit={(e) => { e.preventDefault(); save(); }} className="space-y-4">
      <div><label htmlFor="quick-role-name" className="mb-2 block text-xs font-medium">ロール名</label><Input id="quick-role-name" autoFocus maxLength={80} value={name} onChange={(e) => { setName(e.target.value); setError(""); }} placeholder="例: 営業推進チーム" /></div>
      <div><span className="mb-2 block text-xs font-medium">親ロール</span><div className="flex items-center justify-between gap-3 rounded-md border px-3 py-2 text-[13px]"><span className="min-w-0 break-words" data-testid="role-editor-parent">{parent ? catalog.rolePathLabel(parent) : "なし（最上位）"}</span><Button type="button" size="sm" variant="ghost" onClick={() => setPicking(!picking)}>{picking ? "候補を閉じる" : "親を変更"}</Button></div></div>
      {picking && <div className="rounded-lg border p-3">
        <IconInput icon={Search} aria-label="親ロールを検索" value={query} onValueChange={(value) => { setQuery(value); setPage(0); }} placeholder="名前・ID・階層で検索" className="h-9" />
        <div className="mt-2 max-h-44 overflow-auto"><Button type="button" variant="ghost" size="sm" className="w-full justify-start" onClick={() => { setParent(null); setPicking(false); setError(""); }}>なし（最上位）</Button>{candidates.rows.map((r) => <button type="button" key={r.id} onClick={() => { setParent(r.id); setPicking(false); setError(""); }} className="block min-h-10 w-full rounded px-2 py-2 text-left text-xs outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring">{catalog.rolePathLabel(r.id)}</button>)}{!candidates.total && <p className="p-3 text-xs text-muted-foreground">候補がありません。自身と配下のロールは選べません。</p>}</div>
        <div className="mt-2 flex items-center justify-between text-xs text-muted-foreground"><span>{candidates.total.toLocaleString("ja-JP")}件 / {candidates.page + 1}ページ</span><div className="flex gap-2"><Button type="button" variant="ghost" size="sm" disabled={!candidates.page} onClick={() => setPage(candidates.page - 1)}>前へ</Button><Button type="button" variant="ghost" size="sm" disabled={candidates.page + 1 >= candidates.pages} onClick={() => setPage(candidates.page + 1)}>次へ</Button></div></div>
      </div>}
      {role ? <p className="text-xs leading-5 text-muted-foreground">ロールIDは保持します。{affected ? `配下${affected}ロールの表示階層・所属の継承にも影響します。` : "親を変えると、所属が含まれる上位ロールも変わります。"}</p> : <label className="flex items-center gap-2 text-[13px]"><input type="checkbox" checked={assign} onChange={(e) => setAssign(e.target.checked)} />作成後に {groupName} に対応付ける</label>}
      {error && <p role="alert" className="text-xs text-destructive">{error}</p>}
      <div className="flex justify-end gap-2"><Button type="button" variant="outline" onClick={onClose}>キャンセル</Button><Button type="submit" disabled={!name.trim() || !changed}>{role ? "変更を編集内容に追加" : assign ? "作成して対応付ける" : "ロールを作成"}</Button></div>
    </form>
  </DialogContent></Dialog>;
}
