"use client";

import * as React from "react";
import { ArrowRight, Check, ChevronLeft, Folder, Plus, Search, Users } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { IconInput } from "@/components/ui/icon-input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { cn } from "@/lib/utils";
import { AD_GROUPS, type Mapping } from "./mock-directory-model";
import { planAutoRoles, type AutoMode } from "./directory-auto-plan";
import type { RoleCatalog } from "./directory-role-catalog";

export function DirectoryAutoDialog({ catalog, mapping, selected, onClose, onAccept }: {
  catalog: RoleCatalog; mapping: Mapping; selected: string;
  onClose: () => void; onAccept: (catalog: RoleCatalog, mapping: Mapping) => void;
}) {
  const [scope, setScope] = React.useState("selected");
  const [ids, setIds] = React.useState([selected]);
  const [mode, setMode] = React.useState<AutoMode>("ou");
  const [descendants, setDescendants] = React.useState(true);
  const [step, setStep] = React.useState<"choose" | "review">("choose");
  const [query, setQuery] = React.useState("");
  const [focus, setFocus] = React.useState(selected);
  const [filter, setFilter] = React.useState("all");
  const [page, setPage] = React.useState(0);
  const targets = scope === "all" ? AD_GROUPS.map((g) => g.id) : ids;
  const key = targets.join(",");
  const plan = React.useMemo(() => planAutoRoles(catalog, mapping, key ? key.split(",") : [], mode, descendants), [catalog, mapping, key, mode, descendants]);
  const created = plan.nodes.filter((n) => n.action === "create").length;
  const reused = plan.nodes.length - created;
  const shown = plan.nodes.filter((n) => filter === "all" || n.action === filter);
  const rootDepth = catalog.index.has("all") ? catalog.rolePath("all").length + 1 : 1;
  const visibleGroups = AD_GROUPS.filter((g) => `${g.name} ${g.ou}`.toLowerCase().includes(query.toLowerCase()));
  const pages = Math.max(1, Math.ceil(shown.length / 40));
  const currentPage = Math.min(page, pages - 1);
  const active = plan.rows.find((r) => r.group.id === focus) ?? plan.rows[0];
  const root = catalog.index.has("all") ? catalog.rolePathLabel("all") : "最上位";
  const valid = !plan.errors.length && !!targets.length;
  return <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
    <DialogContent className="flex h-[min(800px,calc(100dvh-64px))] max-w-6xl flex-col gap-0 overflow-hidden p-0 max-sm:h-[calc(var(--visible-height,100dvh)*0.96)] max-sm:max-h-[calc(var(--visible-height,100dvh)*0.96)] max-sm:px-0 max-sm:pb-0 max-sm:pt-0">
      <header className="shrink-0 border-b px-5 py-4 pr-12 md:px-6">
        <div className="mb-2 flex items-center gap-2 text-[11px] text-muted-foreground"><span className={cn(step === "choose" && "font-medium text-foreground")}>1 対象を選ぶ</span><ArrowRight className="size-3" aria-hidden /><span className={cn(step === "review" && "font-medium text-foreground")}>2 内容を確認</span></div>
        <DialogTitle className="text-base">ADからロールを作成</DialogTitle>
        <DialogDescription className="mt-1 text-xs">{step === "choose" ? "対象を選ぶと、作成後の階層を右側で確認できます。" : "作成する階層と対応付けを確認してください。まだ変更は反映されていません。"}</DialogDescription>
      </header>
      <div className="grid min-h-0 flex-1 grid-cols-1 overflow-y-auto md:grid-cols-[300px_minmax(0,1fr)] md:overflow-hidden">
        <aside className="flex min-h-0 flex-col border-b bg-muted/15 md:border-b-0 md:border-r">
          <div className="shrink-0 space-y-3 px-4 py-4">
            <div className="flex items-center justify-between"><h3 className="text-xs font-semibold">ADグループ</h3><span className="text-[11px] text-muted-foreground">{targets.length}件を選択</span></div>
            {step === "choose" && <><SegmentedControl size="sm" aria-label="自動作成の対象" value={scope} onValueChange={setScope} options={[{ value: "selected", label: "選択したグループ" }, { value: "all", label: `すべて ${AD_GROUPS.length}件` }]} /><IconInput icon={Search} aria-label="自動作成するグループを検索" value={query} onValueChange={setQuery} placeholder="名前・OUで検索" className="h-8 text-xs" /></>}
          </div>
          <ul aria-label="作成対象のADグループ" className="max-h-48 min-h-0 flex-1 overflow-y-auto px-2 pb-2 md:max-h-none">
            {(step === "choose" ? visibleGroups : AD_GROUPS.filter((g) => plan.rows.some((r) => r.group.id === g.id) || targets.includes(g.id))).map((g) => <li key={g.id} className={cn("flex items-center gap-2 rounded-md px-2", active?.group.id === g.id && "bg-accent/70")}>
              {step === "choose" && <input type="checkbox" aria-label={`${g.name}を自動作成の対象にする`} disabled={scope === "all"} checked={targets.includes(g.id)} onChange={(e) => { setFocus(g.id); setIds(e.target.checked ? [...ids, g.id] : ids.filter((id) => id !== g.id)); }} className="size-3.5 shrink-0 accent-primary" />}
              <button type="button" aria-pressed={active?.group.id === g.id} onClick={() => setFocus(g.id)} className="flex min-h-14 min-w-0 flex-1 items-center gap-2 rounded text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"><Users className="size-3.5 shrink-0 text-muted-foreground" aria-hidden /><span className="min-w-0"><span className="block truncate text-xs font-medium">{g.name}</span><span className="mt-0.5 block truncate text-[11px] text-muted-foreground">{g.ou}</span></span></button>
            </li>)}
          </ul>
          {step === "choose" && <div className="shrink-0 space-y-4 border-t px-4 py-3">
            {scope === "selected" && <div className="flex gap-4 text-[11px]"><button type="button" className="text-muted-foreground underline underline-offset-4" onClick={() => setIds(query ? [...new Set([...ids, ...visibleGroups.map((g) => g.id)])] : AD_GROUPS.map((g) => g.id))}>{query ? "検索結果を選択" : "すべて選択"}</button><button type="button" className="text-muted-foreground underline underline-offset-4" onClick={() => setIds([])}>選択を解除</button></div>}
            <fieldset className="space-y-2 text-xs"><legend className="mb-2 font-medium">引き継ぐ階層</legend><label className="flex cursor-pointer items-center gap-2"><input type="radio" name="auto-mode" checked={mode === "ou"} onChange={() => setMode("ou")} />OUの配置と名前</label><label className="flex cursor-pointer items-center gap-2"><input type="radio" name="auto-mode" checked={mode === "membership"} onChange={() => setMode("membership")} />グループの入れ子と名前</label></fieldset>
            {mode === "membership" && <label className="flex items-center gap-2 text-[11px]"><input type="checkbox" checked={descendants} onChange={(e) => setDescendants(e.target.checked)} />配下のグループも含める</label>}
          </div>}
          <details className="shrink-0 border-t px-4 py-3 text-[11px] text-muted-foreground"><summary className="cursor-pointer">取込ルール</summary><div className="mt-2 space-y-2 leading-5"><p>名前は変更しません。同じ親・名前のロールは再利用し、既存対応は残して追加します。</p><p>OUはAD上の配置です。OUから作るロールの親子関係はシキ側に新たに設定され、子のメンバーは親にも含まれます。</p><p>グループの入れ子は必要な親も含めます。複数親・循環は省略せず、取込を止めます。</p></div></details>
        </aside>
        <section className="flex min-h-0 flex-col" aria-label="作成後のロールと対応付け">
          <div className="shrink-0 border-b px-5 py-3"><div className="flex flex-wrap items-center justify-between gap-2"><h3 className="text-xs font-semibold">作成後のロール階層</h3><span className="text-[11px] text-muted-foreground">作成先 · {root}</span></div><div className="mt-3"><SegmentedControl size="sm" aria-label="自動作成の確認フィルター" value={filter} onValueChange={(v) => { setFilter(v); setPage(0); }} options={[{ value: "all", label: "すべて" }, { value: "create", label: `新規 ${created}` }, { value: "reuse", label: `既存 ${reused}` }]} /></div></div>
          {!!plan.errors.length && <div role="alert" className="shrink-0 border-b bg-destructive/5 px-5 py-3 text-xs"><p className="font-medium text-destructive">この階層では作成できません</p><ul className="mt-1 max-h-20 overflow-y-auto leading-5 text-muted-foreground">{plan.errors.map((e) => <li key={e}>{e}</li>)}</ul>{mode === "membership" && <Button className="mt-2 h-7 text-xs" size="sm" variant="outline" onClick={() => { setMode("ou"); setFilter("all"); setPage(0); }}>OUの配置で確認し直す</Button>}</div>}
          <div className="min-h-[180px] flex-1 overflow-y-auto py-2 md:min-h-0">
            <div className="flex items-center gap-2 px-5 py-2 text-xs font-medium"><Folder className="size-3.5 text-muted-foreground" aria-hidden />{root}</div>
            <ul aria-label="作成後のロール一覧">{shown.slice(currentPage * 40, (currentPage + 1) * 40).map((node) => {
              const row = plan.rows.find((r) => r.target === node.role.id);
              const depth = Math.min(5, Math.max(0, node.path.split(" / ").length - rootDepth));
              return <li key={node.role.id}>
                <button type="button" disabled={!row} aria-pressed={!!row && active?.target === node.role.id} onClick={() => { if (row) setFocus(row.group.id); }} className={cn("flex min-h-8 w-full items-center pointer-coarse:min-h-11 gap-2 py-1.5 pr-5 text-left text-xs outline-none disabled:opacity-100 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring", row && "hover:bg-accent/40", row && active?.target === node.role.id && "bg-accent/60")} style={{ paddingLeft: 36 + depth * 20 }}>
                  <span aria-hidden className="h-4 w-2 shrink-0 border-b border-l border-border" />{node.source === "OU" ? <Folder className="size-3.5 shrink-0 text-muted-foreground" aria-hidden /> : <Users className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />}<span className="min-w-0 flex-1 truncate font-medium">{node.role.label}</span><span className={cn("flex shrink-0 items-center gap-1 text-[10px]", node.action === "create" ? "text-muted-foreground" : "text-muted-foreground/70")}>{node.action === "create" ? <Plus className="size-3" aria-hidden /> : <Check className="size-3" aria-hidden />}{node.action === "create" ? "新規" : "既存を利用"}</span>
                </button>
              </li>;
            })}</ul>
            {!shown.length && <p className="px-5 py-10 text-center text-xs text-muted-foreground">{targets.length ? "該当するロールはありません。" : "左側でグループを選択してください。"}</p>}
          </div>
          {pages > 1 && <div className="flex shrink-0 items-center justify-end gap-3 border-t px-4 py-1 text-xs"><Button size="sm" variant="ghost" disabled={!currentPage} onClick={() => setPage(currentPage - 1)}>前へ</Button>{currentPage + 1} / {pages}<Button size="sm" variant="ghost" disabled={currentPage + 1 >= pages} onClick={() => setPage(currentPage + 1)}>次へ</Button></div>}
          <div className="shrink-0 border-t bg-muted/15 px-5 py-3" aria-label="選択項目の対応付け">
            {active ? <><div className="mb-3 flex items-center justify-between gap-2"><h4 className="text-xs font-semibold">{active.group.name}</h4><span className="text-[10px] text-muted-foreground">対応付けの変更</span></div><dl className="grid grid-cols-[48px_minmax(0,1fr)] gap-x-2 gap-y-2 text-[11px]"><dt className="text-muted-foreground">現在</dt><dd className="break-words">{active.before.length ? active.before.map((id) => catalog.index.get(id)?.label ?? id).join("、") : "対応なし"}</dd><dt className="text-muted-foreground">追加先</dt><dd className="break-words" data-testid="auto-role-target-path">{active.path}</dd></dl><p className="mt-3 flex items-center gap-1.5 text-[10px] text-muted-foreground"><Check className="size-3" aria-hidden />{active.before.includes(active.target) ? "対応済み・変更なし" : active.before.length ? "現在の対応付けを残して追加します" : "このグループをロールに対応付けます"}</p></> : <p className="text-xs text-muted-foreground">グループを選ぶと対応付けの変更を確認できます。</p>}
          </div>
        </section>
      </div>
      <footer className="flex shrink-0 flex-wrap items-center justify-between gap-3 border-t bg-background px-5 py-3 md:px-6">
        <div aria-live="polite"><p className="text-xs font-medium">新規 {created}<span className="mx-2 text-border">/</span>既存利用 {reused}<span className="mx-2 text-border">/</span>対応追加 {plan.changed}</p><p className="mt-1 text-[10px] text-muted-foreground">{plan.included > targets.length ? `親・配下を含む${plan.included}グループが対象。` : ""}元のADは変更しません。取込後も取消できます。</p></div>
        <div className="flex gap-2">{step === "review" ? <Button size="sm" variant="outline" onClick={() => setStep("choose")}><ChevronLeft aria-hidden />対象を変更</Button> : <Button size="sm" variant="outline" onClick={onClose}>キャンセル</Button>}{step === "choose" ? <Button size="sm" disabled={!targets.length} onClick={() => setStep("review")}>作成内容を確認<ArrowRight aria-hidden /></Button> : <Button size="sm" disabled={!valid || (!created && !plan.changed)} onClick={() => onAccept(plan.catalog, plan.mapping)}>この内容で作成</Button>}</div>
        {step === "review" && !plan.errors.length && !created && !plan.changed && <p className="w-full text-xs text-muted-foreground">すべて作成・対応付け済みです。追加の変更はありません。</p>}
      </footer>
    </DialogContent>
  </Dialog>;
}
