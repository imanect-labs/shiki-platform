"use client";

import * as React from "react";
import { AlertCircle, ChevronLeft, ChevronRight, Search, Plus, Pencil } from "lucide-react";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { IconInput } from "@/components/ui/icon-input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import { type RoleCatalog, type CatalogRole, type RolePage } from "./directory-role-catalog";
import { DirectoryRoleEditor } from "./directory-role-editor";

export function DirectoryRoleBrowser({ selected, onChange, groupId, groupName, catalog, onSaveRole }: {
  selected: string[]; onChange: (roles: string[]) => void; groupId: string; groupName: string;
  catalog: RoleCatalog; onSaveRole: (role: CatalogRole, assign: boolean) => void;
}) {
  const [query, setQuery] = React.useState("");
  const [parent, setParent] = React.useState<string | null>(() => catalog.index.get(selected[0])?.parent ?? "all");
  const [view, setView] = React.useState("browse");
  const [page, setPage] = React.useState(0);
  const [state, setState] = React.useState<{ data: RolePage | null; loading: boolean; error: string | null }>({ data: null, loading: true, error: null });
  const [editor, setEditor] = React.useState<{ role?: CatalogRole; parent: string | null } | null>(null);
  const [retry, setRetry] = React.useState(0);
  React.useEffect(() => {
    if (parent && !catalog.index.has(parent)) { setParent("all"); setPage(0); }
  }, [parent, catalog]);
  // 選択済み一覧以外は、チェックの変更では再検索・フォーカス移動を起こさない。
  const selectedKey = view === "selected" ? JSON.stringify(selected) : "[]";
  React.useEffect(() => {
    const controller = new AbortController();
    setState((s) => ({ ...s, loading: true, error: null }));
    catalog.fetchPage({ query, parent, selectedOnly: view === "selected", selected: JSON.parse(selectedKey) as string[], page }, controller.signal)
      .then((data) => { if (!controller.signal.aborted) setState({ data, loading: false, error: null }); })
      .catch((error: unknown) => { if (!controller.signal.aborted) setState({ data: null, loading: false, error: error instanceof Error ? error.message : "ロールを取得できませんでした" }); });
    return () => controller.abort();
  }, [query, parent, view, page, selectedKey, retry, catalog]);
  // グループを切り替えても検索位置は維持し、チェックだけを切り替える。
  const navigate = (id: string | null) => { setParent(id); setPage(0); setQuery(""); setView("browse"); };
  const data = state.data;
  const globally = !!query.trim();
  const lineage = parent ? catalog.rolePath(parent) : [];
  return <div data-testid="directory-role-browser" data-group={groupId} className="flex min-w-0 flex-col">
    <div className="space-y-3 border-b px-4 py-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <SegmentedControl size="sm" aria-label="ロールの表示" value={view} onValueChange={(value) => { setView(value); setQuery(""); setPage(0); }} options={[{ value: "browse", label: "ロールを探す" }, { value: "selected", label: `選択済み ${selected.length}` }]} />
        <Button size="sm" variant="outline" onClick={() => setEditor({ parent: globally ? "all" : parent })}><Plus aria-hidden />ロールを追加</Button>
      </div>
      <IconInput icon={Search} type="search" aria-label="ロールを検索" value={query} onValueChange={(value) => { setQuery(value); setPage(0); }} placeholder={view === "selected" ? "選択済みのロールを検索" : "全ロールから名前・ID・階層で検索"} className="h-9" />
      <nav aria-label="ロールの階層" className="flex min-h-6 flex-wrap items-center gap-1 text-xs">
        <button type="button" onClick={() => navigate(null)} className="rounded px-1 py-0.5 text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring">すべて</button>
        {!globally && view === "browse" && lineage.map((part) => <React.Fragment key={part.id}><ChevronRight className="size-3 text-muted-foreground" aria-hidden /><button type="button" onClick={() => navigate(part.id)} aria-current={parent === part.id ? "location" : undefined} className={cn("rounded px-1 py-0.5 outline-none focus-visible:ring-2 focus-visible:ring-ring", parent === part.id ? "font-medium" : "text-muted-foreground")}>{part.label}</button></React.Fragment>)}
        {(globally || view === "selected") && <span className="text-muted-foreground">/ {view === "selected" ? "選択済み" : "全体検索"}</span>}
      </nav>
    </div>
    <div className="h-[320px] overflow-y-auto overscroll-contain" aria-busy={state.loading}>
      {state.error ? <EmptyState icon={AlertCircle} title="ロールを取得できませんでした" description={state.error} action={<Button variant="outline" size="sm" onClick={() => setRetry((n) => n + 1)}>再試行</Button>} /> : state.loading ? <div className="space-y-3 p-4" aria-label="ロールを読み込み中">{Array.from({ length: 6 }, (_, i) => <Skeleton key={i} className="h-8 w-full" />)}</div> : !data?.rows.length ? <EmptyState action={<Button variant="outline" size="sm" onClick={() => setEditor({ parent, role: undefined })}><Plus aria-hidden />ロールを追加</Button>} title={view === "selected" ? "選択済みのロールはありません" : "該当するロールはありません"} description="検索条件を変えるか、階層を戻ってください。" /> : <ul aria-label="ロール候補" className="divide-y divide-border/60">
        {data.rows.map((role) => <li key={role.id} data-testid="directory-role-row" className={cn("flex min-h-14 items-center gap-2 px-4 text-[13px] hover:bg-accent/30", selected.includes(role.id) && "bg-accent/50")}>
          <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-3 py-2.5"><input type="checkbox" checked={selected.includes(role.id)} aria-label={`${role.label} ${role.id}`} onChange={(e) => onChange(e.target.checked ? [...selected, role.id] : selected.filter((id) => id !== role.id))} className="size-4 shrink-0 accent-primary" /><span className="min-w-0"><span className="block truncate font-medium">{role.label}</span><span className="mt-0.5 block truncate text-[11px] text-muted-foreground" title={catalog.rolePathLabel(role.id)}>{globally || view === "selected" ? catalog.rolePathLabel(role.id) : role.id}</span></span></label>
          <Button variant="ghost" size="sm" aria-label={`${role.label}を編集`} onClick={() => setEditor({ role, parent: role.parent })}><Pencil aria-hidden /></Button>
          {!!catalog.childCount(role.id) && <Button size="sm" variant="ghost" aria-label={`${role.label}の配下を開く`} onClick={() => navigate(role.id)} className="shrink-0 text-xs text-muted-foreground">{catalog.childCount(role.id)}<ChevronRight aria-hidden /></Button>}
        </li>)}
      </ul>}
    </div>
    <div className="flex min-h-12 flex-wrap items-center justify-between gap-2 border-t px-4 py-2 text-[11px] text-muted-foreground" aria-live="polite">
      <span>{state.loading ? "検索中…" : data ? `${data.total.toLocaleString("ja-JP")}件${data.total ? `中 ${data.page * 40 + 1}〜${Math.min((data.page + 1) * 40, data.total)}件` : ""}` : "取得失敗"}</span>
      <div className="flex items-center gap-2"><Button variant="ghost" size="sm" disabled={state.loading || !data || !data.page} aria-label="ロールの前のページ" onClick={() => setPage((data?.page ?? 0) - 1)}><ChevronLeft aria-hidden /></Button><span>{(data?.page ?? 0) + 1} / {data?.pages ?? 1}</span><Button variant="ghost" size="sm" disabled={state.loading || !data || data.page + 1 >= data.pages} aria-label="ロールの次のページ" onClick={() => setPage((data?.page ?? 0) + 1)}><ChevronRight aria-hidden /></Button></div>
    </div>
    {editor && <DirectoryRoleEditor initialName={editor.role ? undefined : query.trim()} role={editor.role} defaultParent={editor.parent} catalog={catalog} groupName={groupName} onClose={() => setEditor(null)} onSave={(value, assign) => { onSaveRole(value, assign); setEditor(null); setQuery(""); setPage(0); if (!editor.role) { if (assign) setView("selected"); else { setView("browse"); setQuery(value.id); } } else if (view === "browse") setQuery(value.id); }} />}
  </div>;
}
